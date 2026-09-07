use merry::profiles::NoSandboxReviewMode;
use merry_runtime::{HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub(crate) mod managed_provider;
mod mcp;
mod path_review;
mod paths;
pub use paths::XdgPaths;
use paths::{resolve_config_relative_path, resolve_path_access_rule_path, resolve_user_path};
mod provider;
mod runtime;

pub(crate) use managed_provider::{
    ManagedProviderDefinition, ManagedProviderKind, ManagedProviderStore,
    ManagedProviderStoreError, ProviderAlias, derive_provider_alias,
};
use mcp::McpToml;
use provider::ProvidersToml;
pub(crate) use provider::{
    ConfiguredProviderKind, ConfiguredProviderProfile, ProviderConfigSource,
};
pub use provider::{EffectiveOpenAiProviderConfig, EffectiveProviderConfig};
use runtime::RuntimeToml;
pub use runtime::SubagentsConfig;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("HOME must be set to an absolute path to resolve Merry config")]
    HomeMissingOrRelative,
    #[error("failed to read Merry config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse Merry config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("Merry config is invalid: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerryConfig {
    raw: MerryConfigToml,
    config_dir: PathBuf,
    state_dir: PathBuf,
    home: PathBuf,
    managed_provider_aliases: BTreeSet<String>,
}

impl MerryConfig {
    /// Returns the absolute host home used to resolve user configuration.
    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn load_optional(paths: &XdgPaths) -> Result<Option<Self>, ConfigError> {
        let user_text = read_optional_config_text(paths.config_file())?;
        let managed_path = paths.managed_providers_file();
        let managed_text = read_optional_config_text(&managed_path)?;
        if user_text.is_none() && managed_text.is_none() {
            return Ok(None);
        }

        let mut raw = user_text
            .as_deref()
            .map(|text| parse_config_text(text, paths.config_file()))
            .transpose()?
            .unwrap_or_default();
        let mut managed_provider_aliases = BTreeSet::new();
        if let Some(text) = managed_text.as_deref() {
            let managed = managed_provider::parse_managed_providers(text, &managed_path)?;
            let providers = raw.providers.get_or_insert_with(ProvidersToml::default);
            for (alias, provider) in managed {
                if providers.named.contains_key(&alias) {
                    return Err(ConfigError::Invalid(format!(
                        "provider alias {alias:?} exists in both user and managed config"
                    )));
                }
                managed_provider_aliases.insert(alias.clone());
                providers.named.insert(alias, provider);
            }
        }

        Ok(Some(Self {
            raw,
            config_dir: paths.config_dir().to_path_buf(),
            state_dir: paths.state_dir().to_path_buf(),
            home: paths.home().to_path_buf(),
            managed_provider_aliases,
        }))
    }

    #[cfg(test)]
    pub fn load_optional_from_text(
        text: Option<&str>,
        paths: &XdgPaths,
    ) -> Result<Option<Self>, ConfigError> {
        let Some(text) = text else {
            return Ok(None);
        };
        let raw = parse_config_text(text, paths.config_file())?;
        Ok(Some(Self {
            raw,
            config_dir: paths.config_dir().to_path_buf(),
            state_dir: paths.state_dir().to_path_buf(),
            home: paths.home().to_path_buf(),
            managed_provider_aliases: BTreeSet::new(),
        }))
    }

    pub fn effective_log_settings(
        &self,
        paths: &XdgPaths,
    ) -> Result<Option<EffectiveLogSettings>, ConfigError> {
        let Some(log) = self
            .raw
            .observability
            .as_ref()
            .and_then(|value| value.log.as_ref())
        else {
            return Ok(None);
        };
        if !log.enabled {
            return Ok(None);
        }
        let path = match log.path.as_deref() {
            None => paths.default_log_file().to_path_buf(),
            Some(path) => resolve_user_path(path, &self.config_dir, &self.home)?,
        };
        Ok(Some(EffectiveLogSettings {
            level: log.level,
            format: log.format,
            path,
        }))
    }

    pub fn profile(&self) -> Option<&str> {
        self.raw.global.profile.as_deref()
    }

    pub(crate) fn no_sandbox_review_mode(&self) -> NoSandboxReviewMode {
        self.raw
            .permissions
            .as_ref()
            .and_then(|permissions| permissions.no_sandbox_review)
            .map(Into::into)
            .unwrap_or_default()
    }

    /// Resolves product-owned directories and credential files without reading secrets.
    pub(crate) fn private_process_paths(&self) -> Result<Vec<PathBuf>, ConfigError> {
        let mut paths = vec![self.config_dir.clone(), self.state_dir.clone()];
        if let Some(providers) = &self.raw.providers {
            for path in providers
                .named
                .values()
                .filter_map(|provider| provider.api_key_file.as_deref())
            {
                paths.push(resolve_config_relative_path(
                    path,
                    &self.config_dir,
                    &self.home,
                )?);
            }
        }
        Ok(paths)
    }

    /// Returns preauthorized path access and action-scoped review restrictions.
    ///
    /// Outer sandboxes enforce the access ceiling. Inner sandboxes withhold
    /// only paths marked for review and product-owned private resources.
    pub fn trusted_global_path_rules(&self) -> Result<Vec<PathAccessRule>, ConfigError> {
        let Some(permissions) = self.raw.permissions.as_ref() else {
            return Ok(Vec::new());
        };
        let mut rules = Vec::new();
        for path in &permissions.readonly_paths {
            let path = resolve_path_access_rule_path(path, &self.config_dir, &self.home)?;
            rules.push(PathAccessRule::new(
                path,
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            ));
        }
        for path in &permissions.readwrite_paths {
            let path = resolve_path_access_rule_path(path, &self.config_dir, &self.home)?;
            rules.push(PathAccessRule::new(
                path,
                PathAccess::ReadWrite,
                PathAccessRuleSource::TrustedGlobalConfig,
            ));
        }
        for path in &permissions.deny_paths {
            let path = resolve_path_access_rule_path(path, &self.config_dir, &self.home)?;
            rules.push(PathAccessRule::new(
                path,
                PathAccess::Deny,
                PathAccessRuleSource::TrustedGlobalConfig,
            ));
        }
        for rule in &permissions.paths {
            let path = resolve_path_access_rule_path(&rule.path, &self.config_dir, &self.home)?;
            rules.push(PathAccessRule::new(
                path,
                rule.access.into(),
                PathAccessRuleSource::TrustedGlobalConfig,
            ));
        }
        path_review::append_review_rules(
            &mut rules,
            &permissions.review_paths,
            &self.config_dir,
            &self.home,
        )?;
        Ok(rules)
    }

    /// Returns host IPC integrations explicitly enabled by trusted global
    /// configuration. These form the outer sandbox capability ceiling and are
    /// forwarded to inner process sandboxes when their endpoints are present.
    pub fn host_integrations(&self) -> Vec<HostIntegration> {
        let Some(permissions) = self.raw.permissions.as_ref() else {
            return Vec::new();
        };
        let mut integrations = Vec::new();
        if permissions.ssh_agent.unwrap_or(false) {
            integrations.push(HostIntegration::SshAgent);
        }
        if permissions.dbus.unwrap_or(false) {
            integrations.push(HostIntegration::SessionBus);
        }
        if permissions.gpg_agent.unwrap_or(false) {
            integrations.push(HostIntegration::GpgAgent);
        }
        integrations
    }

    pub fn process_environment_overrides(&self) -> Result<Vec<(String, String)>, ConfigError> {
        let Some(permissions) = self.raw.permissions.as_ref() else {
            return Ok(Vec::new());
        };
        let mut names = BTreeSet::new();
        let mut overrides = Vec::with_capacity(permissions.environment.len());
        for entry in &permissions.environment {
            validate_environment_name(&entry.name)?;
            if entry.value.contains('\0') {
                return Err(ConfigError::Invalid(format!(
                    "permissions.environment value for {:?} must not contain NUL",
                    entry.name
                )));
            }
            if !names.insert(entry.name.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "permissions.environment contains duplicate variable {:?}",
                    entry.name
                )));
            }
            overrides.push((entry.name.clone(), entry.value.clone()));
        }
        Ok(overrides)
    }

    pub fn skill_roots(&self) -> Result<Vec<PathBuf>, ConfigError> {
        let Some(skills) = self.raw.skills.as_ref() else {
            return Ok(vec![self.config_dir.join("skills")]);
        };
        if !skills.enabled {
            return Ok(Vec::new());
        }

        let configured_roots = if skills.roots.is_empty() {
            vec!["skills".to_owned()]
        } else {
            skills.roots.clone()
        };

        let mut roots = Vec::with_capacity(configured_roots.len());
        for root in &configured_roots {
            if root.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "skills.roots entries must not be blank".to_owned(),
                ));
            }
            roots.push(resolve_config_relative_path(
                root,
                &self.config_dir,
                &self.home,
            )?);
        }
        Ok(roots)
    }
}

fn read_optional_config_text(path: &Path) -> Result<Option<String>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn parse_config_text(text: &str, path: &Path) -> Result<MerryConfigToml, ConfigError> {
    toml::from_str::<MerryConfigToml>(text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_environment_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty()
        || name.contains('=')
        || name.contains('\0')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || name.as_bytes()[0].is_ascii_digit()
    {
        return Err(ConfigError::Invalid(format!(
            "permissions.environment name {name:?} is not a valid environment variable"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLogSettings {
    pub level: LogLevel,
    pub format: LogFormat,
    pub path: PathBuf,
}

fn validate_model_text(label: &str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Invalid(format!("{label} must not be blank")));
    }
    if value.chars().any(char::is_control) {
        return Err(ConfigError::Invalid(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Text,
}

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MerryConfigToml {
    #[serde(default)]
    global: GlobalToml,
    permissions: Option<PermissionsToml>,
    runtime: Option<RuntimeToml>,
    skills: Option<SkillsToml>,
    models: Option<ModelsToml>,
    observability: Option<ObservabilityToml>,
    providers: Option<ProvidersToml>,
    mcp: Option<McpToml>,
    tui: Option<TuiToml>,
}

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GlobalToml {
    profile: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PermissionsToml {
    ssh_agent: Option<bool>,
    dbus: Option<bool>,
    gpg_agent: Option<bool>,
    no_sandbox_review: Option<NoSandboxReviewToml>,
    #[serde(default)]
    readonly_paths: Vec<String>,
    #[serde(default)]
    readwrite_paths: Vec<String>,
    #[serde(default)]
    deny_paths: Vec<String>,
    #[serde(default)]
    review_paths: Vec<String>,
    #[serde(default)]
    paths: Vec<PathRuleToml>,
    #[serde(default)]
    environment: Vec<EnvironmentVariableToml>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum NoSandboxReviewToml {
    Host,
    Model,
}

impl From<NoSandboxReviewToml> for NoSandboxReviewMode {
    fn from(value: NoSandboxReviewToml) -> Self {
        match value {
            NoSandboxReviewToml::Host => Self::Host,
            NoSandboxReviewToml::Model => Self::Model,
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EnvironmentVariableToml {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PathRuleToml {
    path: String,
    access: PathAccessToml,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum PathAccessToml {
    #[serde(alias = "readonly", alias = "read-only")]
    Ro,
    #[serde(alias = "readwrite", alias = "read-write")]
    Rw,
    Deny,
}

impl From<PathAccessToml> for PathAccess {
    fn from(value: PathAccessToml) -> Self {
        match value {
            PathAccessToml::Ro => Self::ReadOnly,
            PathAccessToml::Rw => Self::ReadWrite,
            PathAccessToml::Deny => Self::Deny,
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SkillsToml {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    roots: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ModelsToml {
    context_compaction: Option<RuntimeModelToml>,
    approval_review: Option<RuntimeModelToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeModelToml {
    provider: Option<String>,
    model: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TuiToml {
    theme: Option<TuiThemeToml>,
    keymap: Option<TuiKeymapToml>,
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiThemeToml {
    pub(crate) status: Option<String>,
    pub(crate) muted: Option<String>,
    pub(crate) focus: Option<String>,
    pub(crate) assistant: Option<String>,
    pub(crate) selection: Option<String>,
    pub(crate) tool_keyword: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) diff_add: Option<String>,
    pub(crate) diff_delete: Option<String>,
    pub(crate) warning: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) risk: Option<String>,
    pub(crate) success: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiKeymapToml {
    pub(crate) submit_next: Option<String>,
    pub(crate) submit_backlog: Option<String>,
    pub(crate) cancel_input_or_quit: Option<String>,
    pub(crate) insert_newline: Option<String>,
    pub(crate) paste_image: Option<String>,
    pub(crate) open_session_in_browser: Option<String>,
    pub(crate) toggle_plan: Option<String>,
    pub(crate) interrupt: Option<String>,
    pub(crate) quit: Option<String>,
    pub(crate) scroll_up: Option<String>,
    pub(crate) scroll_down: Option<String>,
    pub(crate) review_previous_user_input: Option<String>,
    pub(crate) history_previous: Option<String>,
    pub(crate) history_next: Option<String>,
    pub(crate) resume_suspended: Option<String>,
    pub(crate) discard_suspended: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TuiConfig {
    pub(crate) theme: TuiThemeToml,
    pub(crate) keymap: TuiKeymapToml,
}

impl MerryConfig {
    pub(crate) fn tui_config(&self) -> Result<TuiConfig, ConfigError> {
        let Some(tui) = self.raw.tui.as_ref() else {
            return Ok(TuiConfig::default());
        };
        let config = TuiConfig {
            theme: tui.theme.clone().unwrap_or_default(),
            keymap: tui.keymap.clone().unwrap_or_default(),
        };
        validate_tui_config(&config)?;
        Ok(config)
    }
}

pub(crate) fn validate_tui_config(config: &TuiConfig) -> Result<(), ConfigError> {
    let _ = crate::tui::theme::TuiTheme::from_config(&config.theme)?;
    let _ = crate::tui::keymap::Keymap::from_config(&config.keymap)?;
    Ok(())
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ObservabilityToml {
    log: Option<LogToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LogToml {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_log_level")]
    level: LogLevel,
    #[serde(default = "default_log_format")]
    format: LogFormat,
    path: Option<String>,
}

fn default_log_level() -> LogLevel {
    LogLevel::Info
}

fn default_log_format() -> LogFormat {
    LogFormat::Json
}

#[cfg(test)]
mod tests;
