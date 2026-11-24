use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource, PermissionReviewRules,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

mod cli;
pub(crate) mod managed_provider;
mod mcp;
mod paths;
pub use paths::XdgPaths;
use paths::{
    resolve_config_relative_path, resolve_path_access_rule_path,
    resolve_provider_api_key_file_path, resolve_user_path,
};
mod provider;
mod runtime;
mod subagents;

pub(crate) use cli::CliDefaults;
use cli::CliToml;
pub(crate) use managed_provider::{
    ManagedProviderDefinition, ManagedProviderKind, ManagedProviderStore,
    ManagedProviderStoreError, ProviderAlias, derive_provider_alias,
};
use mcp::McpToml;
pub(crate) use provider::{ConfiguredProviderKind, ConfiguredProviderProfile};
pub use provider::{EffectiveOpenAiProviderConfig, EffectiveProviderConfig};
use provider::{NamedProviderToml, ProvidersToml};
use runtime::RuntimeToml;
use subagents::SubagentsToml;
pub use subagents::{EffectiveSubagentRoleConfig, SubagentsConfig, load_project_subagent_roles};

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
        source: Box<toml::de::Error>,
    },
    #[error("Merry config is invalid: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerryConfig {
    raw: MerryConfigToml,
    config_dir: PathBuf,
    state_dir: PathBuf,
    providers_dir: PathBuf,
    home: PathBuf,
    managed_providers: BTreeMap<String, NamedProviderToml>,
}

impl MerryConfig {
    /// Returns the absolute host home used to resolve user configuration.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Returns the absolute Merry config directory (`$XDG_CONFIG_HOME/merry`).
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn load_optional(paths: &XdgPaths) -> Result<Option<Self>, ConfigError> {
        relocate_legacy_managed_dir(paths)?;
        let user_text = read_optional_config_text(paths.config_file())?;
        let managed_text = read_optional_managed_config_text(&paths.managed_providers_file())?;
        Self::from_texts(user_text.as_deref(), managed_text.as_deref(), paths)
    }

    fn from_texts(
        user_text: Option<&str>,
        managed_text: Option<&str>,
        paths: &XdgPaths,
    ) -> Result<Option<Self>, ConfigError> {
        if user_text.is_none() && managed_text.is_none() {
            return Ok(None);
        }
        let raw = user_text
            .map(|text| parse_config_text(text, paths.config_file()))
            .transpose()?
            .unwrap_or_default();
        let managed_providers = managed_text
            .map(|text| {
                managed_provider::parse_managed_providers(text, &paths.managed_providers_file())
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Some(Self {
            raw,
            config_dir: paths.config_dir().to_path_buf(),
            state_dir: paths.state_dir().to_path_buf(),
            providers_dir: paths.managed_config_dir(),
            home: paths.home().to_path_buf(),
            managed_providers,
        }))
    }

    #[cfg(test)]
    pub fn load_optional_from_text(
        text: Option<&str>,
        paths: &XdgPaths,
    ) -> Result<Option<Self>, ConfigError> {
        Self::from_texts(text, None, paths)
    }

    #[cfg(test)]
    pub fn load_with_managed_from_text(
        user_text: Option<&str>,
        managed_text: Option<&str>,
        paths: &XdgPaths,
    ) -> Result<Option<Self>, ConfigError> {
        Self::from_texts(user_text, managed_text, paths)
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

    /// Resolves product-owned state and credential paths without reading secrets.
    ///
    /// Merry's own non-secret config directory (`$XDG_CONFIG_HOME/merry`) and
    /// configured skill roots are intentionally excluded here so sandboxed
    /// actions may read and update user config and skills while state
    /// (`$XDG_STATE_HOME/merry`) and provider credentials
    /// (`$XDG_DATA_HOME/merry/providers`) remain protected.
    pub(crate) fn private_process_paths(&self) -> Result<Vec<PathBuf>, ConfigError> {
        let mut paths = vec![self.state_dir.clone(), self.providers_dir.clone()];
        for path in self
            .managed_providers
            .values()
            .filter_map(|provider| provider.api_key_file.as_deref())
        {
            let resolved =
                resolve_provider_api_key_file_path(path, &self.providers_dir, &self.home)?;
            // Files already inside `providers_dir` or `state_dir` are covered by
            // the directory denies above; skipping them keeps the denied path
            // list (and profile hash) stable as providers change in Settings.
            if !resolved.starts_with(&self.providers_dir) && !resolved.starts_with(&self.state_dir)
            {
                paths.push(resolved);
            }
        }
        Ok(paths)
    }

    /// Returns extra path grants from trusted global config.
    ///
    /// `readonly_paths` and `readwrite_paths` add trees. `deny_paths`,
    /// `review_paths`, and `paths` are rejected because the workspace sandbox
    /// cannot withhold a subdirectory of an allowed path.
    pub fn trusted_global_path_rules(&self) -> Result<Vec<PathAccessRule>, ConfigError> {
        let Some(permissions) = self.raw.permissions.as_ref() else {
            return Ok(Vec::new());
        };
        let mut removed = Vec::new();
        if !permissions.deny_paths.is_empty() {
            removed.push(format!(
                "deny_paths ({})",
                permissions.deny_paths.join(", ")
            ));
        }
        if !permissions.review_paths.is_empty() {
            removed.push(format!(
                "review_paths ({})",
                permissions.review_paths.join(", ")
            ));
        }
        if !permissions.paths.is_empty() {
            let entries = permissions
                .paths
                .iter()
                .map(|rule| format!("{} {}", rule.path, rule.access.label()))
                .collect::<Vec<_>>()
                .join(", ");
            removed.push(format!("paths ({entries})"));
        }
        if !removed.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "[permissions] {} is not applied by the workspace sandbox. Remove it from {}. Extra trees belong in readonly_paths or readwrite_paths.",
                removed.join("; "),
                self.config_dir.join("config.toml").display()
            )));
        }
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
        Ok(rules)
    }

    /// Returns whether sandboxed actions may request network.
    ///
    /// Defaults to true so a new install can fetch dependencies after review.
    /// `network = false` closes that sandbox option. Each action still has to
    /// request network and pass permission review. Merry's own model and MCP
    /// connections are outside this switch.
    pub fn network_requests_allowed(&self) -> bool {
        self.raw
            .permissions
            .as_ref()
            .and_then(|permissions| permissions.network)
            .unwrap_or(true)
    }

    /// Returns host IPC integrations explicitly enabled by trusted global
    /// configuration.
    ///
    /// When a validated endpoint exists, ordinary sandboxed actions use it
    /// without another permission request.
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

    /// Returns user-trusted custom rules for AI permission review from global
    /// config (`[permissions.review]`).
    ///
    /// Rules live only in global `$XDG_CONFIG_HOME/merry/config.toml` so
    /// project-local repositories cannot silently weaken review boundaries.
    pub fn permission_review_rules(&self) -> Result<PermissionReviewRules, ConfigError> {
        let Some(review) = self
            .raw
            .permissions
            .as_ref()
            .and_then(|permissions| permissions.review.as_ref())
        else {
            return Ok(PermissionReviewRules::default());
        };
        PermissionReviewRules::new(
            review.environment.clone(),
            review.allow.clone(),
            review.soft_deny.clone(),
            review.hard_deny.clone(),
        )
        .map_err(|error| ConfigError::Invalid(error.to_string()))
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

/// Moves a legacy `$XDG_CONFIG_HOME/merry/managed` directory to
/// `$XDG_DATA_HOME/merry/providers` when the new directory does not yet exist.
///
/// Compatibility shim so existing users keep their Settings-managed providers
/// and secrets when `config_dir` becomes writable by sandboxed actions.
/// TODO: Remove once pre-relocation managed directories are no longer supported.
fn relocate_legacy_managed_dir(paths: &XdgPaths) -> Result<(), ConfigError> {
    let legacy_dir = paths.legacy_managed_config_dir();
    let target_dir = paths.managed_config_dir();
    if legacy_dir == target_dir {
        return Ok(());
    }
    match fs::symlink_metadata(&legacy_dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Ok(()),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(source) => {
            return Err(ConfigError::Read {
                path: legacy_dir,
                source,
            });
        }
    }
    let legacy_registry = legacy_dir.join("providers.toml");
    if !fs::symlink_metadata(&legacy_registry).is_ok_and(|metadata| metadata.file_type().is_file())
    {
        return Ok(());
    }
    match fs::symlink_metadata(&target_dir) {
        Ok(_) => {
            tracing::warn!(
                legacy = %legacy_dir.display(),
                target = %target_dir.display(),
                "legacy managed provider directory still exists alongside the active providers directory; leaving it untouched"
            );
            return Ok(());
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(source) if source.kind() == io::ErrorKind::NotADirectory => return Ok(()),
        Err(source) => {
            return Err(ConfigError::Read {
                path: target_dir,
                source,
            });
        }
    }

    if let Some(parent) = target_dir.parent() {
        fs::create_dir_all(parent).map_err(|source| {
            ConfigError::Invalid(format!(
                "failed to create parent directory {} for legacy provider relocation: {source}",
                parent.display()
            ))
        })?;
    }
    match fs::rename(&legacy_dir, &target_dir) {
        Ok(()) => {
            tracing::info!(
                from = %legacy_dir.display(),
                to = %target_dir.display(),
                "relocated legacy managed provider directory"
            );
            Ok(())
        }
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
            ) && target_dir.exists() =>
        {
            Ok(())
        }
        Err(source) if source.kind() == io::ErrorKind::CrossesDevices => {
            move_managed_dir_cross_device(&legacy_dir, &target_dir).map_err(|copy_err| {
                ConfigError::Invalid(format!(
                    "failed to move legacy provider directory {} to {}: {copy_err}",
                    legacy_dir.display(),
                    target_dir.display()
                ))
            })?;
            tracing::info!(
                from = %legacy_dir.display(),
                to = %target_dir.display(),
                "relocated legacy managed provider directory across filesystems"
            );
            Ok(())
        }
        Err(source) => Err(ConfigError::Invalid(format!(
            "failed to move legacy provider directory {} to {}: {source}",
            legacy_dir.display(),
            target_dir.display()
        ))),
    }
}

fn move_managed_dir_cross_device(legacy_dir: &Path, target_dir: &Path) -> io::Result<()> {
    let parent = target_dir.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "target providers directory has no parent",
        )
    })?;
    let temp_dir = parent.join(format!(".providers.relocate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    let copy_result = (|| -> io::Result<()> {
        create_private_dir_sync(&temp_dir)?;
        copy_private_file_sync(
            &legacy_dir.join("providers.toml"),
            &temp_dir.join("providers.toml"),
        )?;
        let legacy_secrets = legacy_dir.join("secrets");
        if fs::symlink_metadata(&legacy_secrets).is_ok_and(|metadata| metadata.file_type().is_dir())
        {
            let temp_secrets = temp_dir.join("secrets");
            create_private_dir_sync(&temp_secrets)?;
            for entry in fs::read_dir(&legacy_secrets)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    copy_private_file_sync(&entry.path(), &temp_secrets.join(entry.file_name()))?;
                }
            }
        }
        fs::rename(&temp_dir, target_dir)
    })();
    if let Err(error) = copy_result {
        let _ = fs::remove_dir_all(&temp_dir);
        return Err(error);
    }
    let _ = fs::remove_dir_all(legacy_dir);
    Ok(())
}

fn create_private_dir_sync(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn copy_private_file_sync(from: &Path, to: &Path) -> io::Result<()> {
    let content = fs::read(from)?;
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(to)?;
    io::Write::write_all(&mut file, &content)?;
    file.sync_all()
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

fn read_optional_managed_config_text(path: &Path) -> Result<Option<String>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(source) => Err(ConfigError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn parse_config_text(text: &str, path: &Path) -> Result<MerryConfigToml, ConfigError> {
    toml::from_str::<MerryConfigToml>(text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

pub(crate) fn validate_environment_name(name: &str) -> Result<(), ConfigError> {
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
    cli: Option<CliToml>,
    permissions: Option<PermissionsToml>,
    runtime: Option<RuntimeToml>,
    subagents: Option<SubagentsToml>,
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
    gpg_agent: Option<bool>,
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
    review: Option<PermissionReviewToml>,
}

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PermissionReviewToml {
    #[serde(default)]
    environment: Vec<String>,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    soft_deny: Vec<String>,
    #[serde(default)]
    hard_deny: Vec<String>,
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

impl PathAccessToml {
    const fn label(self) -> &'static str {
        match self {
            Self::Ro => "ro",
            Self::Rw => "rw",
            Self::Deny => "deny",
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

pub(crate) const DEFAULT_MOUSE_SCROLL_LINES: usize = 3;
pub(crate) const MAX_MOUSE_SCROLL_LINES: usize = 10;
pub(crate) const DEFAULT_MAX_READING_WIDTH: u16 = 104;
pub(crate) const MIN_MAX_READING_WIDTH: u16 = 40;
pub(crate) const MAX_MAX_READING_WIDTH: u16 = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HeaderItemKind {
    Workspace,
    Model,
    Context,
    Cache,
    Rate,
    Tokens,
}

impl HeaderItemKind {
    pub(crate) const ALL: [Self; 6] = [
        Self::Workspace,
        Self::Model,
        Self::Context,
        Self::Cache,
        Self::Rate,
        Self::Tokens,
    ];

    pub(crate) const DEFAULT_ITEMS: [Self; 5] = [
        Self::Workspace,
        Self::Model,
        Self::Context,
        Self::Cache,
        Self::Rate,
    ];
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TextSizingMode {
    #[default]
    Off,
    Auto,
    On,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrandMarkMode {
    #[default]
    Emoji,
    Snowflake,
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiHeaderToml {
    pub(crate) items: Option<Vec<HeaderItemKind>>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TuiToml {
    show_successful_command_output: Option<bool>,
    mouse_scroll_lines: Option<usize>,
    animations: Option<bool>,
    max_reading_width: Option<u16>,
    text_sizing: Option<TextSizingMode>,
    brand_mark: Option<BrandMarkMode>,
    header: Option<TuiHeaderToml>,
    theme: Option<TuiThemeToml>,
    keymap: Option<TuiKeymapToml>,
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiThemeToml {
    pub(crate) preset: Option<crate::tui::theme::ThemePreset>,
    pub(crate) transparent: Option<bool>,
    pub(crate) background: Option<String>,
    pub(crate) surface: Option<String>,
    pub(crate) surface_subtle: Option<String>,
    pub(crate) header_bg: Option<String>,
    pub(crate) status_bg: Option<String>,
    pub(crate) user_bg: Option<String>,
    pub(crate) code_bg: Option<String>,
    pub(crate) inline_code_bg: Option<String>,
    pub(crate) selection_bg: Option<String>,
    pub(crate) chip_bg: Option<String>,
    pub(crate) border: Option<String>,
    pub(crate) border_focus: Option<String>,
    pub(crate) diff_add_bg: Option<String>,
    pub(crate) diff_delete_bg: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) muted: Option<String>,
    pub(crate) text_secondary: Option<String>,
    pub(crate) focus: Option<String>,
    pub(crate) assistant: Option<String>,
    pub(crate) heading: Option<String>,
    pub(crate) link: Option<String>,
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
    #[serde(alias = "submit_next")]
    pub(crate) submit: Option<String>,
    pub(crate) cancel_input_or_quit: Option<String>,
    pub(crate) insert_newline: Option<String>,
    pub(crate) paste_image: Option<String>,
    pub(crate) open_session_in_browser: Option<String>,
    pub(crate) toggle_plan: Option<String>,
    pub(crate) interrupt: Option<String>,
    pub(crate) quit: Option<String>,
    pub(crate) scroll_up: Option<String>,
    pub(crate) scroll_down: Option<String>,
    pub(crate) open_command_details: Option<String>,
    pub(crate) review_previous_user_input: Option<String>,
    pub(crate) history_previous: Option<String>,
    pub(crate) history_next: Option<String>,
    pub(crate) history_search: Option<String>,
    pub(crate) toggle_comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiConfig {
    pub(crate) show_successful_command_output: bool,
    pub(crate) mouse_scroll_lines: usize,
    pub(crate) animations: bool,
    pub(crate) max_reading_width: u16,
    pub(crate) text_sizing: TextSizingMode,
    pub(crate) brand_mark: BrandMarkMode,
    pub(crate) header_items: Vec<HeaderItemKind>,
    pub(crate) theme: TuiThemeToml,
    pub(crate) keymap: TuiKeymapToml,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            show_successful_command_output: true,
            mouse_scroll_lines: DEFAULT_MOUSE_SCROLL_LINES,
            animations: true,
            max_reading_width: DEFAULT_MAX_READING_WIDTH,
            text_sizing: TextSizingMode::Off,
            brand_mark: BrandMarkMode::Emoji,
            header_items: HeaderItemKind::DEFAULT_ITEMS.to_vec(),
            theme: TuiThemeToml::default(),
            keymap: TuiKeymapToml::default(),
        }
    }
}

impl MerryConfig {
    pub(crate) fn tui_config(&self) -> Result<TuiConfig, ConfigError> {
        let Some(tui) = self.raw.tui.as_ref() else {
            return Ok(TuiConfig::default());
        };
        let header_items = tui
            .header
            .as_ref()
            .and_then(|header| header.items.clone())
            .unwrap_or_else(|| HeaderItemKind::DEFAULT_ITEMS.to_vec());
        let config = TuiConfig {
            show_successful_command_output: tui.show_successful_command_output.unwrap_or(true),
            mouse_scroll_lines: tui.mouse_scroll_lines.unwrap_or(DEFAULT_MOUSE_SCROLL_LINES),
            animations: tui.animations.unwrap_or(true),
            max_reading_width: tui.max_reading_width.unwrap_or(DEFAULT_MAX_READING_WIDTH),
            text_sizing: tui.text_sizing.unwrap_or(TextSizingMode::Off),
            brand_mark: tui.brand_mark.unwrap_or(BrandMarkMode::Emoji),
            header_items,
            theme: tui.theme.clone().unwrap_or_default(),
            keymap: tui.keymap.clone().unwrap_or_default(),
        };
        validate_tui_config(&config)?;
        Ok(config)
    }
}

pub(crate) fn validate_tui_config(config: &TuiConfig) -> Result<(), ConfigError> {
    if !(1..=MAX_MOUSE_SCROLL_LINES).contains(&config.mouse_scroll_lines) {
        return Err(ConfigError::Invalid(format!(
            "tui.mouse_scroll_lines must be between 1 and {MAX_MOUSE_SCROLL_LINES}, got {}",
            config.mouse_scroll_lines
        )));
    }
    if config.max_reading_width != 0
        && !(MIN_MAX_READING_WIDTH..=MAX_MAX_READING_WIDTH).contains(&config.max_reading_width)
    {
        return Err(ConfigError::Invalid(format!(
            "tui.max_reading_width must be 0 (unlimited) or between {MIN_MAX_READING_WIDTH} and {MAX_MAX_READING_WIDTH}, got {}",
            config.max_reading_width
        )));
    }
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
