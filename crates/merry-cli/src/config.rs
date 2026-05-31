use merry_runtime::{AutomaticCompactionConfig, CitationCompactionPolicy};
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

    pub fn automatic_compaction_config(&self) -> Result<AutomaticCompactionConfig, ConfigError> {
        let Some(auto_compaction) = self
            .raw
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.auto_compaction.as_ref())
        else {
            return Ok(AutomaticCompactionConfig::default());
        };

        auto_compaction.to_config()
    }

    pub fn skill_roots(&self) -> Result<Vec<PathBuf>, ConfigError> {
        let Some(skills) = self.raw.skills.as_ref() else {
            return Ok(Vec::new());
        };
        if !skills.enabled {
            return Ok(Vec::new());
        }

        let mut roots = Vec::with_capacity(skills.roots.len());
        for root in &skills.roots {
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

    pub fn runtime_models(&self) -> Result<EffectiveRuntimeModelsConfig, ConfigError> {
        let Some(models) = self.raw.models.as_ref() else {
            return Ok(EffectiveRuntimeModelsConfig::default());
        };

        let context_compaction = models
            .context_compaction
            .as_ref()
            .map(|model| self.effective_runtime_model("context_compaction", model))
            .transpose()?;

        Ok(EffectiveRuntimeModelsConfig { context_compaction })
    }

    fn effective_runtime_model(
        &self,
        role: &str,
        model: &RuntimeModelToml,
    ) -> Result<EffectiveRuntimeModelConfig, ConfigError> {
        validate_model_text(&format!("models.{role}.model"), &model.model)?;
        let provider = match model.provider.as_deref() {
            Some(provider) => provider,
            None => self
                .raw
                .providers
                .as_ref()
                .and_then(|providers| providers.default.as_ref())
                .map(|default| default.provider.as_str())
                .ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "[providers.default] is required when [models.{role}].provider is omitted"
                    ))
                })?,
        };
        self.validate_runtime_model_provider(role, provider)?;
        Ok(EffectiveRuntimeModelConfig {
            provider: provider.to_owned(),
            model: model.model.clone(),
        })
    }

    fn validate_runtime_model_provider(
        &self,
        role: &str,
        provider_alias: &str,
    ) -> Result<(), ConfigError> {
        if provider_alias != "openai-compatible" {
            return Err(ConfigError::Invalid(format!(
                "unsupported provider {provider_alias:?} for [models.{role}]; only openai-compatible is supported"
            )));
        }
        let providers = self.raw.providers.as_ref().ok_or_else(|| {
            ConfigError::Invalid(format!(
                "[providers.{provider_alias}] is required for [models.{role}]"
            ))
        })?;
        if !providers.named.contains_key(provider_alias) {
            return Err(ConfigError::Invalid(format!(
                "[providers.{provider_alias}] is required for [models.{role}]"
            )));
        }
        Ok(())
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
                &self.home,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLogSettings {
    pub level: LogLevel,
    pub format: LogFormat,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRuntimeModelConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveRuntimeModelsConfig {
    pub context_compaction: Option<EffectiveRuntimeModelConfig>,
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
    runtime: Option<RuntimeToml>,
    skills: Option<SkillsToml>,
    models: Option<ModelsToml>,
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
struct RuntimeToml {
    auto_compaction: Option<AutoCompactionToml>,
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
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeModelToml {
    provider: Option<String>,
    model: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AutoCompactionToml {
    #[serde(default = "default_true")]
    enabled: bool,
    target_output_tokens: Option<u64>,
    model_output_token_limit: Option<u64>,
    max_accepted_output_bytes: Option<usize>,
    retained_raw_tail_items: Option<usize>,
    max_ref_excerpt_bytes: Option<usize>,
    max_carried_prior_refs: Option<usize>,
}

impl AutoCompactionToml {
    fn to_config(&self) -> Result<AutomaticCompactionConfig, ConfigError> {
        if !self.enabled {
            return Ok(AutomaticCompactionConfig::disabled());
        }

        let defaults = AutomaticCompactionConfig::default().policy();
        let policy = CitationCompactionPolicy::new(
            self.target_output_tokens
                .unwrap_or_else(|| defaults.target_output_tokens()),
            self.model_output_token_limit
                .or_else(|| defaults.model_output_token_limit()),
            self.max_accepted_output_bytes
                .unwrap_or_else(|| defaults.max_accepted_output_bytes()),
            self.retained_raw_tail_items
                .unwrap_or_else(|| defaults.retained_raw_tail_items()),
            self.max_ref_excerpt_bytes
                .unwrap_or_else(|| defaults.max_ref_excerpt_bytes()),
            self.max_carried_prior_refs
                .unwrap_or_else(|| defaults.max_carried_prior_refs()),
        )
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        Ok(AutomaticCompactionConfig::enabled(policy))
    }
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
    fn parses_runtime_auto_compaction_config() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
enabled = true
target_output_tokens = 160
model_output_token_limit = 256
max_accepted_output_bytes = 4096
retained_raw_tail_items = 4
max_ref_excerpt_bytes = 900
max_carried_prior_refs = 12
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let auto_compaction = config
            .automatic_compaction_config()
            .expect("auto compaction config should validate");
        assert!(auto_compaction.is_enabled());
        let policy = auto_compaction.policy();
        assert_eq!(policy.target_output_tokens(), 160);
        assert_eq!(policy.model_output_token_limit(), Some(256));
        assert_eq!(policy.max_accepted_output_bytes(), 4096);
        assert_eq!(policy.retained_raw_tail_items(), 4);
        assert_eq!(policy.max_ref_excerpt_bytes(), 900);
        assert_eq!(policy.max_carried_prior_refs(), 12);
    }

    #[test]
    fn runtime_auto_compaction_config_defaults_and_disabled_mode() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("empty config should parse")
            .expect("config should be present")
            .automatic_compaction_config()
            .expect("default auto compaction config should validate");
        assert_eq!(missing, merry_runtime::AutomaticCompactionConfig::default());

        let disabled = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
enabled = false
retained_raw_tail_items = 4
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present")
        .automatic_compaction_config()
        .expect("disabled auto compaction config should validate");
        assert!(!disabled.is_enabled());
        assert_eq!(
            disabled.policy(),
            merry_runtime::AutomaticCompactionConfig::default().policy()
        );
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
    fn disabled_or_missing_skills_return_no_roots() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("config should parse")
            .expect("config should be present");
        assert_eq!(
            missing.skill_roots().expect("missing skills is valid"),
            Vec::<PathBuf>::new()
        );

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
    fn context_compaction_model_role_defaults_to_default_provider() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.openai-compatible]
api_key = "sk-inline-secret"

[models.context_compaction]
model = "gpt-compact"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let models = config
            .runtime_models()
            .expect("runtime model role config should validate");
        let context_compaction = models
            .context_compaction
            .expect("context compaction model role should be configured");
        assert_eq!(context_compaction.provider, "openai-compatible");
        assert_eq!(context_compaction.model, "gpt-compact");
    }

    #[test]
    fn runtime_model_roles_default_to_no_overrides() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let models = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("empty config should parse")
            .expect("config should be present")
            .runtime_models()
            .expect("empty runtime model role config should validate");

        assert!(models.context_compaction.is_none());
    }

    #[test]
    fn rejects_unknown_runtime_model_role_or_provider_alias() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let unknown_role = MerryConfig::load_optional_from_text(
            Some(
                r#"
[models.tool_planner]
provider = "openai-compatible"
model = "gpt-other"
"#,
            ),
            &paths,
        )
        .expect_err("unknown runtime model role should fail parsing");
        assert!(unknown_role.to_string().contains("tool_planner"));

        let unknown_provider = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.openai-compatible]
api_key = "sk-inline-secret"

[models.context_compaction]
provider = "not-configured"
model = "gpt-compact"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present")
        .runtime_models()
        .expect_err("unknown model role provider should fail validation");
        assert!(unknown_provider.to_string().contains("not-configured"));
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
            Vec::<PathBuf>::new()
        );

        let models = config
            .runtime_models()
            .expect("example runtime model roles should validate");
        let context_compaction = models
            .context_compaction
            .expect("example should configure context compaction model role");
        assert_eq!(context_compaction.provider, "openai-compatible");
        assert_eq!(context_compaction.model, "gpt-4.1-mini");
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
