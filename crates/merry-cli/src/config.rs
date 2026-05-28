use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env, fmt, fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

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
            Some(path) => resolve_user_path(path, &self.config_dir)?,
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

    pub fn validate_provider_settings_if_present(&self) -> Result<(), ConfigError> {
        if self.raw.providers.is_some() {
            let _ = self.openai_compatible_provider()?;
        }
        Ok(())
    }

    pub fn openai_compatible_provider(&self) -> Result<EffectiveOpenAiProviderConfig, ConfigError> {
        let providers =
            self.raw.providers.as_ref().ok_or_else(|| {
                ConfigError::Invalid("[providers.default] is required".to_owned())
            })?;
        let default = providers
            .default
            .as_ref()
            .ok_or_else(|| ConfigError::Invalid("[providers.default] is required".to_owned()))?;
        if default.provider != "openai-compatible" {
            return Err(ConfigError::Invalid(format!(
                "unsupported default provider {}",
                default.provider
            )));
        }
        let provider = providers.named.get("openai-compatible").ok_or_else(|| {
            ConfigError::Invalid("[providers.openai-compatible] is required".to_owned())
        })?;
        let api_key = match (
            provider.api_key.as_deref(),
            provider.api_key_file.as_deref(),
        ) {
            (Some(_), Some(_)) => {
                return Err(ConfigError::Invalid(
                    "providers.openai-compatible must not set both api_key and api_key_file; choose one".to_owned(),
                ));
            }
            (Some(value), None) => {
                validate_api_key_text("api_key", value)?;
                EffectiveOpenAiApiKeySource::Inline(value.to_owned())
            }
            (None, Some(path)) => EffectiveOpenAiApiKeySource::File(resolve_config_relative_path(
                path,
                &self.config_dir,
            )?),
            (None, None) => {
                return Err(ConfigError::Invalid(
                    "providers.openai-compatible must set exactly one of api_key or api_key_file"
                        .to_owned(),
                ));
            }
        };
        Ok(EffectiveOpenAiProviderConfig {
            model: Some(default.model.clone()),
            base_url: provider.base_url.clone(),
            api_key,
        })
    }
}

fn resolve_user_path(value: &str, config_dir: &Path) -> Result<PathBuf, ConfigError> {
    if let Some(rest) = value.strip_prefix("~/") {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(ConfigError::HomeMissingOrRelative)?;
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

fn resolve_config_relative_path(value: &str, config_dir: &Path) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else if value.starts_with("~/") {
        resolve_user_path(value, config_dir)
    } else {
        Ok(config_dir.join(path))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLogSettings {
    pub level: LogLevel,
    pub format: LogFormat,
    pub path: PathBuf,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EffectiveOpenAiProviderConfig {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: EffectiveOpenAiApiKeySource,
}

impl fmt::Debug for EffectiveOpenAiProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectiveOpenAiProviderConfig")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum EffectiveOpenAiApiKeySource {
    Inline(String),
    File(PathBuf),
}

impl fmt::Debug for EffectiveOpenAiApiKeySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inline(_) => formatter.write_str("Inline(<redacted>)"),
            Self::File(path) => formatter.debug_tuple("File").field(path).finish(),
        }
    }
}

impl EffectiveOpenAiProviderConfig {
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        match &self.api_key {
            EffectiveOpenAiApiKeySource::Inline(value) => Ok(value.clone()),
            EffectiveOpenAiApiKeySource::File(path) => {
                let value = fs::read_to_string(path).map_err(|source| ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                })?;
                let value = value.trim().to_owned();
                validate_api_key_text(&format!("api_key_file {}", path.display()), &value)?;
                Ok(value)
            }
        }
    }
}

fn validate_api_key_text(label: &str, value: &str) -> Result<(), ConfigError> {
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
    observability: Option<ObservabilityToml>,
    providers: Option<ProvidersToml>,
}

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GlobalToml {
    profile: Option<String>,
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

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
struct ProvidersToml {
    default: Option<DefaultProviderToml>,
    #[serde(flatten)]
    named: BTreeMap<String, OpenAiCompatibleProviderToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DefaultProviderToml {
    provider: String,
    model: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OpenAiCompatibleProviderToml {
    base_url: Option<String>,
    api_key: Option<String>,
    api_key_file: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn parses_observability_and_provider_config() {
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

[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
api_key_file = "secrets/openai.key"
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

        let provider = config
            .openai_compatible_provider()
            .expect("provider should validate");
        assert_eq!(provider.model.as_deref(), Some("gpt-4.1-mini"));
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.example.test/v1")
        );
        assert_eq!(
            provider.api_key,
            EffectiveOpenAiApiKeySource::File(PathBuf::from(
                "/home/alice/.config/merry/secrets/openai.key"
            ))
        );
    }

    #[test]
    fn example_config_toml_matches_current_schema_and_resolves_user_defaults() {
        let example = include_str!("../../../examples/config.toml");
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

    #[test]
    fn parses_inline_api_key_and_redacts_debug_output() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = "sk-inline-secret"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        let provider = config
            .openai_compatible_provider()
            .expect("provider should validate");

        assert_eq!(
            provider.resolve_api_key().expect("key should resolve"),
            "sk-inline-secret"
        );
        let debug = format!("{provider:?}");
        assert!(debug.contains("Inline(<redacted>)"));
        assert!(!debug.contains("sk-inline-secret"));
    }

    #[test]
    fn rejects_missing_or_ambiguous_api_key_sources() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
"#,
            ),
            &paths,
        )
        .expect("TOML should parse")
        .expect("config should be present")
        .openai_compatible_provider()
        .expect_err("missing key source should fail");
        assert!(
            missing
                .to_string()
                .contains("exactly one of api_key or api_key_file")
        );

        let ambiguous = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = "sk-inline-secret"
api_key_file = "secrets/openai.key"
"#,
            ),
            &paths,
        )
        .expect("TOML should parse")
        .expect("config should be present")
        .openai_compatible_provider()
        .expect_err("ambiguous key source should fail");
        assert!(
            ambiguous
                .to_string()
                .contains("must not set both api_key and api_key_file")
        );
    }

    #[test]
    fn rejects_blank_or_control_character_inline_api_key() {
        let paths = XdgPaths::from_parts(home(), None, None);
        for api_key in ["  ", "sk-test\n"] {
            let error = MerryConfig::load_optional_from_text(
                Some(&format!(
                    r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
api_key = {api_key:?}
"#
                )),
                &paths,
            )
            .expect("TOML should parse")
            .expect("config should be present")
            .openai_compatible_provider()
            .expect_err("invalid key should fail");

            assert!(error.to_string().contains("api_key"));
        }
    }

    #[test]
    fn redacted_provider_debug_does_not_include_api_key_file_contents() {
        let provider = EffectiveOpenAiProviderConfig {
            model: Some("gpt-test".to_owned()),
            base_url: Some("https://api.example.test/v1".to_owned()),
            api_key: EffectiveOpenAiApiKeySource::File(PathBuf::from(
                "/home/alice/.config/merry/secrets/openai.key",
            )),
        };
        let debug = format!("{provider:?}");
        assert!(debug.contains("openai.key"));
        assert!(!debug.contains("sk-"));
    }
}
