use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};
use serde::Deserialize;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

mod provider;
mod runtime;

pub use provider::EffectiveOpenAiProviderConfig;
use provider::ProvidersToml;
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
    config_dir: PathBuf,
    config_file: PathBuf,
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
            config_dir,
            config_file,
            state_dir,
            default_log_file,
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn default_log_file(&self) -> &Path {
        &self.default_log_file
    }

    fn home(&self) -> &Path {
        &self.home
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
}

impl MerryConfig {
    pub fn load_optional(paths: &XdgPaths) -> Result<Option<Self>, ConfigError> {
        match fs::read_to_string(paths.config_file()) {
            Ok(text) => Self::load_optional_from_text(Some(&text), paths),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ConfigError::Read {
                path: paths.config_file().to_path_buf(),
                source,
            }),
        }
    }

    pub fn load_optional_from_text(
        text: Option<&str>,
        paths: &XdgPaths,
    ) -> Result<Option<Self>, ConfigError> {
        let Some(text) = text else {
            return Ok(None);
        };
        let raw = toml::from_str::<MerryConfigToml>(text).map_err(|source| ConfigError::Parse {
            path: paths.config_file().to_path_buf(),
            source,
        })?;
        Ok(Some(Self {
            raw,
            config_dir: paths.config_dir().to_path_buf(),
            home: paths.home().to_path_buf(),
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
    #[serde(default)]
    readonly_paths: Vec<String>,
    #[serde(default)]
    readwrite_paths: Vec<String>,
    #[serde(default)]
    deny_paths: Vec<String>,
    #[serde(default)]
    paths: Vec<PathRuleToml>,
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
    pub(crate) interrupt: Option<String>,
    pub(crate) quit: Option<String>,
    pub(crate) scroll_up: Option<String>,
    pub(crate) scroll_down: Option<String>,
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
    use crate::config::provider::EffectiveOpenAiApiKeySource;
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
interrupt = "esc"
quit = "ctrl+q"
scroll_up = "pageup"
scroll_down = "pagedown"
history_previous = "up"
history_next = "down"
resume_suspended = "ctrl+r"
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
        assert_eq!(tui.keymap.scroll_up.as_deref(), Some("pageup"));
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
        let log = config
            .effective_log_settings(&paths)
            .expect("example log settings should validate")
            .expect("example should enable logs for smoke/debug use");
        assert_eq!(log.level, LogLevel::Debug);
        assert_eq!(log.format, LogFormat::Json);
        assert_eq!(log.path, paths.default_log_file());

        let tui = config
            .tui_config()
            .expect("example TUI config should validate");
        assert_eq!(tui.theme.status.as_deref(), Some("light_magenta"));
        assert_eq!(tui.theme.assistant.as_deref(), Some("white"));
        assert_eq!(tui.theme.tool_keyword.as_deref(), Some("light_cyan"));
        assert_eq!(tui.theme.command.as_deref(), Some("light_blue"));
        assert_eq!(tui.keymap.submit_next.as_deref(), Some("enter"));
        assert_eq!(tui.keymap.scroll_up.as_deref(), Some("pageup"));
        assert_eq!(tui.keymap.history_previous.as_deref(), Some("up"));

        let provider = config
            .openai_compatible_provider()
            .expect("example provider should validate");
        assert_eq!(provider.model.as_deref(), Some("gpt-4.1-mini"));
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
        assert_eq!(policy.target_output_tokens(), 192);
        assert_eq!(policy.model_output_token_limit(), None);
        assert_eq!(policy.max_accepted_output_bytes(), 8192);
        assert_eq!(policy.retained_raw_tail_items(), 2);
        assert_eq!(policy.max_ref_excerpt_bytes(), 1200);
        assert_eq!(policy.max_carried_prior_refs(), 16);
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
