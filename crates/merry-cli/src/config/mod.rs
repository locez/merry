use merry_runtime::{HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub(crate) mod managed_provider;
mod mcp;
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
pub struct XdgPaths {
    home: PathBuf,
    config_base: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    state_base: PathBuf,
    state_dir: PathBuf,
    default_log_file: PathBuf,
}

impl XdgPaths {
    pub fn from_env() -> Result<Self, ConfigError> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(ConfigError::HomeMissingOrRelative)?;
        Ok(Self::from_parts(
            home,
            env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        ))
    }

    pub fn from_parts(
        home: PathBuf,
        xdg_config_home: Option<PathBuf>,
        xdg_state_home: Option<PathBuf>,
    ) -> Self {
        let config_base = absolute_or_default(xdg_config_home, home.join(".config"));
        let state_base = absolute_or_default(xdg_state_home, home.join(".local/state"));
        let config_dir = config_base.join("merry");
        let state_dir = state_base.join("merry");
        let config_file = config_dir.join("config.toml");
        let default_log_file = state_dir.join("logs/merry.jsonl");
        Self {
            home,
            config_base,
            config_dir,
            config_file,
            state_base,
            state_dir,
            default_log_file,
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn config_base_dir(&self) -> &Path {
        &self.config_base
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn state_base_dir(&self) -> &Path {
        &self.state_base
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn default_log_file(&self) -> &Path {
        &self.default_log_file
    }

    pub fn tui_preferences_file(&self) -> PathBuf {
        self.state_dir.join("tui-preferences.toml")
    }

    pub fn managed_config_dir(&self) -> PathBuf {
        self.config_dir.join("managed")
    }

    pub fn managed_providers_file(&self) -> PathBuf {
        self.managed_config_dir().join("providers.toml")
    }

    pub fn managed_secrets_dir(&self) -> PathBuf {
        self.managed_config_dir().join("secrets")
    }

    pub fn model_catalog_cache_dir(&self) -> PathBuf {
        self.state_dir.join("model-catalogs")
    }
}

fn absolute_or_default(value: Option<PathBuf>, default: PathBuf) -> PathBuf {
    match value {
        Some(path) if path.is_absolute() && !path.as_os_str().is_empty() => path,
        _ => default,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerryConfig {
    raw: MerryConfigToml,
    config_dir: PathBuf,
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

    /// Returns user-configured path rules for the outer sandbox ceiling.
    ///
    /// These rules do not become inner action grants automatically; an inner
    /// process still needs an approved request for a path outside its default
    /// development baseline.
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
        Ok(rules)
    }

    pub fn permissions_network_allowed(&self) -> bool {
        self.raw
            .permissions
            .as_ref()
            .and_then(|permissions| permissions.network)
            .unwrap_or(false)
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

fn resolve_user_path(value: &str, config_dir: &Path, home: &Path) -> Result<PathBuf, ConfigError> {
    if let Some(rest) = value.strip_prefix("~/") {
        return Ok(home.join(rest));
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }
    Err(ConfigError::Invalid(format!(
        "log path must be absolute or start with ~/; relative path {value:?} was configured under {}",
        config_dir.display()
    )))
}

fn resolve_config_relative_path(
    value: &str,
    config_dir: &Path,
    home: &Path,
) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else if value.starts_with("~/") {
        resolve_user_path(value, config_dir, home)
    } else {
        Ok(config_dir.join(path))
    }
}

fn resolve_path_access_rule_path(
    value: &str,
    config_dir: &Path,
    home: &Path,
) -> Result<PathBuf, ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Invalid(
            "permissions path entries must not be blank".to_owned(),
        ));
    }
    let path = resolve_config_relative_path(value, config_dir, home)?;
    Ok(normalize_path_lexically(&path))
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

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let is_absolute = path.is_absolute();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() && !is_absolute {
                    normalized.push("..");
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
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
    network: Option<bool>,
    ssh_agent: Option<bool>,
    dbus: Option<bool>,
    #[serde(default)]
    readonly_paths: Vec<String>,
    #[serde(default)]
    readwrite_paths: Vec<String>,
    #[serde(default)]
    deny_paths: Vec<String>,
    #[serde(default)]
    paths: Vec<PathRuleToml>,
    #[serde(default)]
    environment: Vec<EnvironmentVariableToml>,
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
    pub(crate) toggle_plan: Option<String>,
    pub(crate) interrupt: Option<String>,
    pub(crate) quit: Option<String>,
    pub(crate) scroll_up: Option<String>,
    pub(crate) scroll_down: Option<String>,
    pub(crate) review_previous_user_input: Option<String>,
    pub(crate) review_previous_artifact: Option<String>,
    pub(crate) review_next_artifact: Option<String>,
    pub(crate) follow_latest_artifact: Option<String>,
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
mod tests {
    use super::*;
    use crate::config::provider::{EffectiveOpenAiApiKeySource, ProviderConfigSource};
    use std::path::{Path, PathBuf};

    fn home() -> PathBuf {
        PathBuf::from("/home/alice")
    }

    #[test]
    fn xdg_paths_use_defaults_when_env_is_missing_empty_or_relative() {
        let paths = XdgPaths::from_parts(home(), None, None);
        assert_eq!(paths.config_dir(), Path::new("/home/alice/.config/merry"));
        assert_eq!(
            paths.config_file(),
            Path::new("/home/alice/.config/merry/config.toml")
        );
        assert_eq!(
            paths.state_dir(),
            Path::new("/home/alice/.local/state/merry")
        );
        assert_eq!(
            paths.default_log_file(),
            Path::new("/home/alice/.local/state/merry/logs/merry.jsonl")
        );

        let paths =
            XdgPaths::from_parts(home(), Some(PathBuf::new()), Some(PathBuf::from("state")));
        assert_eq!(paths.config_dir(), Path::new("/home/alice/.config/merry"));
        assert_eq!(
            paths.state_dir(),
            Path::new("/home/alice/.local/state/merry")
        );
    }

    #[test]
    fn xdg_paths_use_absolute_env_values() {
        let paths = XdgPaths::from_parts(
            home(),
            Some(PathBuf::from("/tmp/config")),
            Some(PathBuf::from("/tmp/state")),
        );
        assert_eq!(paths.config_dir(), Path::new("/tmp/config/merry"));
        assert_eq!(
            paths.config_file(),
            Path::new("/tmp/config/merry/config.toml")
        );
        assert_eq!(paths.state_dir(), Path::new("/tmp/state/merry"));
        assert_eq!(
            paths.default_log_file(),
            Path::new("/tmp/state/merry/logs/merry.jsonl")
        );
    }

    #[test]
    fn missing_config_is_allowed_for_commands_without_provider_requirement() {
        let loaded =
            MerryConfig::load_optional_from_text(None, &XdgPaths::from_parts(home(), None, None))
                .expect("missing config should not fail optional load");
        assert!(loaded.is_none());
    }

    #[test]
    fn loads_managed_provider_without_user_default_provider() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = XdgPaths::from_parts(
            temp.path().join("home"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        fs::create_dir_all(paths.managed_secrets_dir()).expect("managed secrets dir");
        fs::write(
            paths.managed_secrets_dir().join("opencode.key"),
            "sk-test\n",
        )
        .expect("managed secret");
        fs::write(
            paths.managed_providers_file(),
            r#"
version = 1

[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
type = "openai-compatible"
base_url = "https://opencode.example.test/v1"
api_key_file = "managed/secrets/opencode.key"
"#,
        )
        .expect("managed providers file");

        let config = MerryConfig::load_optional(&paths)
            .expect("managed config should load")
            .expect("managed provider should create config");
        let profile = config
            .provider_profile("opencode")
            .expect("managed profile should resolve");

        assert_eq!(profile.display_name(), "OpenCode");
        assert_eq!(profile.source(), ProviderConfigSource::Managed);
        let EffectiveProviderConfig::OpenAiCompatible(provider) = config
            .provider_by_alias("opencode")
            .expect("managed provider should materialize")
        else {
            panic!("OpenAI-compatible provider expected");
        };
        assert_eq!(
            provider.resolve_api_key().expect("key should resolve"),
            "sk-test"
        );
    }

    #[test]
    fn rejects_user_and_managed_alias_collision() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = XdgPaths::from_parts(
            temp.path().join("home"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        fs::create_dir_all(paths.managed_config_dir()).expect("managed config dir");
        fs::write(
            paths.config_file(),
            r#"
[providers.default]
provider = "opencode"
model = "model-a"

[providers.opencode]
api_key = "sk-user"
"#,
        )
        .expect("user config");
        fs::write(
            paths.managed_providers_file(),
            r#"
version = 1

[providers.opencode]
display_name = "Managed OpenCode"
default_model = "model-b"
type = "openai-compatible"
api_key = "sk-managed"
"#,
        )
        .expect("managed config");

        let error = MerryConfig::load_optional(&paths).expect_err("collision should fail");

        assert!(error.to_string().contains("opencode"));
        assert!(error.to_string().contains("both user and managed"));
    }

    #[test]
    fn parses_mcp_http_servers() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[mcp.context7]
url = "https://mcp.example.test/mcp"
headers = { Authorization = "Bearer test-token" }
tools = ["resolve-library-id", "get-library-docs"]
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let servers = config
            .mcp_servers()
            .expect("MCP server config should validate");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].id(), "context7");
        assert_eq!(servers[0].url(), "https://mcp.example.test/mcp");
        assert_eq!(
            servers[0].headers(),
            &[("Authorization".to_owned(), "Bearer test-token".to_owned())]
        );
        assert_eq!(
            servers[0].tools().expect("tools allowlist should parse"),
            &[
                "resolve-library-id".to_owned(),
                "get-library-docs".to_owned()
            ]
        );
    }

    #[test]
    fn parses_observability_config() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[global]
profile = "default"

[observability.log]
enabled = true
level = "debug"
format = "json"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let log = config
            .effective_log_settings(&paths)
            .expect("log settings should validate")
            .expect("logging should be enabled");
        assert_eq!(log.level, LogLevel::Debug);
        assert_eq!(log.format, LogFormat::Json);
        assert_eq!(log.path, paths.default_log_file());
    }

    #[test]
    fn parses_tui_theme_and_keymap_config() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r##"
[tui.theme]
status = "cyan"
muted = "dark_gray"
focus = "yellow"
assistant = "white"
selection = "blue"
tool_keyword = "light_cyan"
command = "light_blue"
diff_add = "green"
diff_delete = "red"
warning = "yellow"
error = "red"
risk = "magenta"
success = "green"

[tui.keymap]
submit_next = "enter"
submit_backlog = "ctrl+b"
cancel_input_or_quit = "ctrl+c"
insert_newline = "ctrl+j"
paste_image = "ctrl+v"
interrupt = "esc"
quit = "ctrl+q"
scroll_up = "pageup"
scroll_down = "pagedown"
review_previous_user_input = "ctrl+u"
review_previous_artifact = "ctrl+g"
review_next_artifact = "ctrl+f"
follow_latest_artifact = "ctrl+r"
history_previous = "up"
history_next = "down"
resume_suspended = "ctrl+n"
discard_suspended = "ctrl+d"
"##,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let tui = config.tui_config().expect("tui config should validate");
        assert_eq!(tui.theme.status.as_deref(), Some("cyan"));
        assert_eq!(tui.theme.assistant.as_deref(), Some("white"));
        assert_eq!(tui.theme.tool_keyword.as_deref(), Some("light_cyan"));
        assert_eq!(tui.theme.command.as_deref(), Some("light_blue"));
        assert_eq!(tui.keymap.submit_next.as_deref(), Some("enter"));
        assert_eq!(tui.keymap.cancel_input_or_quit.as_deref(), Some("ctrl+c"));
        assert_eq!(tui.keymap.insert_newline.as_deref(), Some("ctrl+j"));
        assert_eq!(tui.keymap.paste_image.as_deref(), Some("ctrl+v"));
        assert_eq!(tui.keymap.scroll_up.as_deref(), Some("pageup"));
        assert_eq!(
            tui.keymap.review_previous_user_input.as_deref(),
            Some("ctrl+u")
        );
        assert_eq!(
            tui.keymap.review_previous_artifact.as_deref(),
            Some("ctrl+g")
        );
        assert_eq!(tui.keymap.review_next_artifact.as_deref(), Some("ctrl+f"));
        assert_eq!(tui.keymap.follow_latest_artifact.as_deref(), Some("ctrl+r"));
        assert_eq!(tui.keymap.history_previous.as_deref(), Some("up"));
    }

    #[test]
    fn parses_trusted_global_path_rules() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[permissions]
network = true
readonly_paths = ["/etc", "~/logs", "shared-readonly"]
readwrite_paths = ["../foo"]
deny_paths = ["~/.ssh"]

[[permissions.paths]]
path = "/var/log/foo"
access = "ro"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let rules = config
            .trusted_global_path_rules()
            .expect("trusted path rules should resolve");
        assert!(config.permissions_network_allowed());
        assert_eq!(rules.len(), 6);
        assert_eq!(rules[0].path(), Path::new("/etc"));
        assert_eq!(rules[0].access(), PathAccess::ReadOnly);
        assert_eq!(rules[1].path(), Path::new("/home/alice/logs"));
        assert_eq!(
            rules[2].path(),
            Path::new("/home/alice/.config/merry/shared-readonly")
        );
        assert_eq!(rules[3].path(), Path::new("/home/alice/.config/foo"));
        assert_eq!(rules[3].access(), PathAccess::ReadWrite);
        assert_eq!(rules[4].path(), Path::new("/home/alice/.ssh"));
        assert_eq!(rules[4].access(), PathAccess::Deny);
        assert_eq!(rules[5].path(), Path::new("/var/log/foo"));
        assert_eq!(rules[5].access(), PathAccess::ReadOnly);
        assert!(
            rules
                .iter()
                .all(|rule| rule.source() == PathAccessRuleSource::TrustedGlobalConfig)
        );
    }

    #[test]
    fn parses_host_integrations_for_outer_sandbox_ceiling() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[permissions]
ssh_agent = true
dbus = true
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        assert_eq!(
            config.host_integrations(),
            vec![
                merry_runtime::HostIntegration::SshAgent,
                merry_runtime::HostIntegration::SessionBus
            ]
        );
    }

    #[test]
    fn rejects_legacy_session_bus_configuration_name() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let error = MerryConfig::load_optional_from_text(
            Some(
                r#"
[permissions]
session_bus = true
"#,
            ),
            &paths,
        )
        .expect_err("unpublished legacy configuration name must be rejected");
        assert!(error.to_string().contains("unknown field `session_bus`"));
    }

    #[test]
    fn parses_and_validates_process_environment_overrides() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[permissions]
environment = [
  { name = "RUSTUP_TOOLCHAIN", value = "stable" },
  { name = "CARGO_TERM_COLOR", value = "always" },
]
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        assert_eq!(
            config
                .process_environment_overrides()
                .expect("environment overrides should validate"),
            vec![
                ("RUSTUP_TOOLCHAIN".to_owned(), "stable".to_owned()),
                ("CARGO_TERM_COLOR".to_owned(), "always".to_owned()),
            ]
        );

        for invalid in ["", "1INVALID", "INVALID-NAME", "INVALID=NAME"] {
            let text =
                format!("[permissions]\nenvironment = [{{ name = {invalid:?}, value = \"x\" }}]");
            let config = MerryConfig::load_optional_from_text(Some(&text), &paths)
                .expect("config should parse");
            let config = config.expect("config should be present");
            assert!(
                config.process_environment_overrides().is_err(),
                "{invalid:?} should be rejected"
            );
        }

        let duplicate = MerryConfig::load_optional_from_text(
            Some(
                r#"
[permissions]
environment = [
  { name = "DUPLICATE", value = "one" },
  { name = "DUPLICATE", value = "two" },
]
"#,
            ),
            &paths,
        )
        .expect("duplicate environment config should parse")
        .expect("duplicate environment config should be present");
        assert!(duplicate.process_environment_overrides().is_err());

        let nul_value = MerryConfig::load_optional_from_text(
            Some("[permissions]\nenvironment = [{ name = \"NUL_VALUE\", value = \"\\u0000\" } ]"),
            &paths,
        )
        .expect("NUL environment config should parse")
        .expect("NUL environment config should be present");
        assert!(nul_value.process_environment_overrides().is_err());
    }

    #[test]
    fn permissions_network_defaults_to_false() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("config should parse")
            .expect("config should be present");
        let permissions_without_network =
            MerryConfig::load_optional_from_text(Some("[permissions]\n"), &paths)
                .expect("config should parse")
                .expect("config should be present");

        assert!(!missing.permissions_network_allowed());
        assert!(!permissions_without_network.permissions_network_allowed());
    }

    #[test]
    fn rejects_unknown_path_access() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let error = MerryConfig::load_optional_from_text(
            Some(
                r#"
[[permissions.paths]]
path = "/etc"
access = "write"
"#,
            ),
            &paths,
        )
        .expect_err("unknown path access should fail parsing");

        assert!(error.to_string().contains("access"));
    }

    #[test]
    fn parses_skill_config_roots() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[skills]
enabled = true
roots = ["skills", "~/shared-skills", "/opt/company/skills"]
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let skills = config.skill_roots().expect("skill roots should resolve");
        assert_eq!(
            skills,
            vec![
                PathBuf::from("/home/alice/.config/merry/skills"),
                PathBuf::from("/home/alice/shared-skills"),
                PathBuf::from("/opt/company/skills"),
            ]
        );
    }

    #[test]
    fn missing_skills_config_uses_default_user_skill_root() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("config should parse")
            .expect("config should be present");
        assert_eq!(
            missing.skill_roots().expect("missing skills is valid"),
            vec![PathBuf::from("/home/alice/.config/merry/skills")]
        );
    }

    #[test]
    fn disabled_skills_return_no_roots() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let disabled = MerryConfig::load_optional_from_text(
            Some("[skills]\nenabled = false\nroots = [\"skills\"]\n"),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        assert_eq!(
            disabled.skill_roots().expect("disabled skills is valid"),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn example_config_toml_matches_current_schema_and_resolves_user_defaults() {
        let example = include_str!("../../../../examples/config.toml");
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(Some(example), &paths)
            .expect("example config should parse")
            .expect("example config should be present");

        assert_eq!(config.profile(), Some("default"));
        assert!(
            config
                .effective_log_settings(&paths)
                .expect("example log settings should validate")
                .is_none(),
            "the user-facing example should not enable persistent logging by default"
        );

        let tui = config
            .tui_config()
            .expect("example TUI config should validate");
        assert_eq!(tui.theme.status.as_deref(), Some("light_magenta"));
        assert_eq!(tui.theme.assistant.as_deref(), Some("white"));
        assert_eq!(tui.theme.tool_keyword.as_deref(), Some("light_cyan"));
        assert_eq!(tui.theme.command.as_deref(), Some("light_blue"));
        assert_eq!(tui.keymap.submit_next.as_deref(), Some("enter"));
        assert_eq!(tui.keymap.cancel_input_or_quit.as_deref(), Some("ctrl+c"));
        assert_eq!(tui.keymap.insert_newline.as_deref(), Some("ctrl+j"));
        assert_eq!(tui.keymap.scroll_up.as_deref(), Some("pageup"));
        assert_eq!(
            tui.keymap.review_previous_user_input.as_deref(),
            Some("ctrl+u")
        );
        assert_eq!(
            tui.keymap.review_previous_artifact.as_deref(),
            Some("ctrl+g")
        );
        assert_eq!(tui.keymap.review_next_artifact.as_deref(), Some("ctrl+f"));
        assert_eq!(tui.keymap.follow_latest_artifact.as_deref(), Some("ctrl+r"));
        assert_eq!(tui.keymap.history_previous.as_deref(), Some("up"));

        let provider = config
            .openai_compatible_provider()
            .expect("example provider should validate");
        assert_eq!(provider.model.as_deref(), Some("gpt-4.1-mini"));
        assert_eq!(
            provider
                .reasoning_effort
                .as_ref()
                .map(|effort| effort.as_str()),
            None
        );
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            provider.api_key,
            EffectiveOpenAiApiKeySource::File(PathBuf::from(
                "/home/alice/.config/merry/secrets/openai.key"
            ))
        );
        let retry = config
            .provider_retry_policy()
            .expect("example retry policy should validate")
            .expect("example retry policy should be configured");
        assert!(retry.enabled());
        assert_eq!(retry.max_attempts(), 6);
        assert_eq!(retry.max_delay(), std::time::Duration::from_secs(120));
        assert_eq!(retry.max_elapsed(), std::time::Duration::from_secs(300));
        assert!(retry.jitter());
        let auto_compaction = config
            .automatic_compaction_config()
            .expect("example auto compaction config should validate");
        assert!(auto_compaction.is_enabled());
        let policy = auto_compaction.policy();
        assert_eq!(policy.target_output_tokens(), None);
        assert_eq!(policy.max_accepted_output_bytes(), None);
        assert_eq!(policy.retained_model_turns(), 5);
        assert_eq!(
            config
                .skill_roots()
                .expect("example skill roots should validate"),
            vec![PathBuf::from("/home/alice/.config/merry/skills")]
        );
        let trusted_path_rules = config
            .trusted_global_path_rules()
            .expect("example trusted path rules should validate");
        assert!(!config.permissions_network_allowed());
        assert_eq!(trusted_path_rules.len(), 5);
        assert_eq!(trusted_path_rules[0].path(), Path::new("/etc"));
        assert_eq!(trusted_path_rules[0].access(), PathAccess::ReadOnly);
        assert_eq!(trusted_path_rules[1].path(), Path::new("/var/log"));
        assert_eq!(trusted_path_rules[1].access(), PathAccess::ReadOnly);
        assert_eq!(
            trusted_path_rules[2].path(),
            Path::new("/home/alice/.config/merry/company-readonly")
        );
        assert_eq!(trusted_path_rules[2].access(), PathAccess::ReadOnly);
        assert_eq!(
            trusted_path_rules[3].path(),
            Path::new("/home/alice/.config/merry/company-work")
        );
        assert_eq!(trusted_path_rules[3].access(), PathAccess::ReadWrite);
        assert_eq!(trusted_path_rules[4].path(), Path::new("/home/alice/.ssh"));
        assert_eq!(trusted_path_rules[4].access(), PathAccess::Deny);

        let models = config
            .runtime_models()
            .expect("example runtime model roles should validate");
        let context_compaction = models
            .context_compaction
            .expect("example should configure context compaction model role");
        assert_eq!(context_compaction.provider, "openai-compatible");
        assert_eq!(context_compaction.model, "gpt-4.1-mini");
        let approval_review = models
            .approval_review
            .expect("example should configure approval review model role");
        assert_eq!(approval_review.provider, "openai-compatible");
        assert_eq!(approval_review.model, "gpt-4.1-mini");
    }

    #[test]
    fn disabled_logging_has_no_effective_log_settings() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[observability.log]
enabled = false
level = "debug"
format = "json"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        assert!(
            config
                .effective_log_settings(&paths)
                .expect("settings should validate")
                .is_none()
        );
    }

    #[test]
    fn rejects_invalid_log_level_format_and_relative_log_path() {
        let paths = XdgPaths::from_parts(home(), None, None);

        let invalid_level = MerryConfig::load_optional_from_text(
            Some("[observability.log]\nenabled = true\nlevel = \"verbose\"\nformat = \"json\"\n"),
            &paths,
        )
        .expect_err("invalid level should fail");
        assert!(invalid_level.to_string().contains("level"));

        let invalid_format = MerryConfig::load_optional_from_text(
            Some("[observability.log]\nenabled = true\nlevel = \"info\"\nformat = \"yaml\"\n"),
            &paths,
        )
        .expect_err("invalid format should fail");
        assert!(invalid_format.to_string().contains("format"));

        let relative_path = MerryConfig::load_optional_from_text(
            Some(
                "[observability.log]\nenabled = true\nlevel = \"info\"\nformat = \"json\"\npath = \"logs/merry.jsonl\"\n",
            ),
            &paths,
        )
        .expect("TOML should parse")
        .expect("config should be present")
        .effective_log_settings(&paths)
        .expect_err("relative log path should fail");
        assert!(relative_path.to_string().contains("absolute"));
    }
}
