use super::{ConfigError, MerryConfig, default_true, resolve_config_relative_path};
use merry_llm::{ModelRetryPolicy, ModelRetryPolicyError, ReasoningEffort};
use serde::Deserialize;
use std::{collections::BTreeMap, fmt, fs, path::PathBuf};

impl MerryConfig {
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
        let reasoning_effort = default
            .reasoning_effort
            .as_deref()
            .map(ReasoningEffort::new)
            .transpose()
            .map_err(|error| {
                ConfigError::Invalid(format!(
                    "providers.default.reasoning_effort is invalid: {error}"
                ))
            })?;
        Ok(EffectiveOpenAiProviderConfig {
            model: Some(default.model.clone()),
            reasoning_effort,
            base_url: provider.base_url.clone(),
            api_key,
        })
    }

    pub fn provider_retry_policy(&self) -> Result<Option<ModelRetryPolicy>, ConfigError> {
        let Some(providers) = self.raw.providers.as_ref() else {
            return Ok(None);
        };
        providers
            .retry
            .as_ref()
            .map(ProviderRetryToml::to_policy)
            .transpose()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct EffectiveOpenAiProviderConfig {
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub base_url: Option<String>,
    pub api_key: EffectiveOpenAiApiKeySource,
}

impl fmt::Debug for EffectiveOpenAiProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectiveOpenAiProviderConfig")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
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

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub(super) struct ProvidersToml {
    pub(super) default: Option<DefaultProviderToml>,
    retry: Option<ProviderRetryToml>,
    #[serde(flatten)]
    pub(super) named: BTreeMap<String, OpenAiCompatibleProviderToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct DefaultProviderToml {
    pub(super) provider: String,
    model: String,
    reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenAiCompatibleProviderToml {
    base_url: Option<String>,
    api_key: Option<String>,
    api_key_file: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProviderRetryToml {
    #[serde(default = "default_true")]
    enabled: bool,
    max_attempts: Option<usize>,
    initial_delay_ms: Option<u64>,
    max_delay_ms: Option<u64>,
    max_elapsed_ms: Option<u64>,
    jitter: Option<bool>,
}

impl ProviderRetryToml {
    fn to_policy(&self) -> Result<ModelRetryPolicy, ConfigError> {
        let defaults = ModelRetryPolicy::coding_agent_default();
        let policy = ModelRetryPolicy::new(
            self.enabled,
            self.max_attempts.unwrap_or(defaults.max_attempts()),
            std::time::Duration::from_millis(
                self.initial_delay_ms
                    .unwrap_or_else(|| duration_millis_u64(defaults.initial_delay())),
            ),
            std::time::Duration::from_millis(
                self.max_delay_ms
                    .unwrap_or_else(|| duration_millis_u64(defaults.max_delay())),
            ),
            std::time::Duration::from_millis(
                self.max_elapsed_ms
                    .unwrap_or_else(|| duration_millis_u64(defaults.max_elapsed())),
            ),
            self.jitter.unwrap_or_else(|| defaults.jitter()),
        )
        .map_err(provider_retry_policy_error)?;
        Ok(policy)
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

fn duration_millis_u64(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn provider_retry_policy_error(error: ModelRetryPolicyError) -> ConfigError {
    ConfigError::Invalid(format!("providers.retry is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::XdgPaths;
    use std::path::PathBuf;

    fn home() -> PathBuf {
        PathBuf::from("/home/alice")
    }

    #[test]
    fn parses_provider_config_and_retry_policy() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"
reasoning_effort = "high"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
api_key_file = "secrets/openai.key"

[providers.retry]
enabled = true
max_attempts = 7
initial_delay_ms = 500
max_delay_ms = 120000
max_elapsed_ms = 300000
jitter = true
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let provider = config
            .openai_compatible_provider()
            .expect("provider should validate");
        assert_eq!(provider.model.as_deref(), Some("gpt-4.1-mini"));
        assert_eq!(
            provider
                .reasoning_effort
                .as_ref()
                .map(|effort| effort.as_str()),
            Some("high")
        );
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
        let retry = config
            .provider_retry_policy()
            .expect("retry policy should validate")
            .expect("retry policy should be configured");
        assert!(retry.enabled());
        assert_eq!(retry.max_attempts(), 7);
        assert_eq!(retry.initial_delay(), std::time::Duration::from_millis(500));
        assert_eq!(retry.max_delay(), std::time::Duration::from_secs(120));
        assert_eq!(retry.max_elapsed(), std::time::Duration::from_secs(300));
        assert!(retry.jitter());
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
            reasoning_effort: Some(ReasoningEffort::new("high").expect("valid effort")),
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
