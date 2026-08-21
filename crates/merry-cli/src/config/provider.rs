use super::{
    ConfigError, MerryConfig, default_true, managed_provider::ProviderAlias,
    resolve_config_relative_path,
};
use merry_llm::{ModelName, ModelRetryPolicy, ModelRetryPolicyError, ReasoningEffort};
use merry_provider_openai::OpenAiProtocol;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, fs, path::PathBuf};

impl MerryConfig {
    pub fn provider_aliases(&self) -> Vec<String> {
        self.raw
            .providers
            .as_ref()
            .map(|providers| providers.named.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn provider_profile(
        &self,
        alias: &str,
    ) -> Result<ConfiguredProviderProfile, ConfigError> {
        let alias = ProviderAlias::new(alias)?;
        let providers = self
            .raw
            .providers
            .as_ref()
            .ok_or_else(|| ConfigError::Invalid(format!("[providers.{alias}] is required")))?;
        let provider = providers
            .named
            .get(alias.as_str())
            .ok_or_else(|| ConfigError::Invalid(format!("[providers.{alias}] is required")))?;
        let kind = match provider.kind.as_deref().unwrap_or(alias.as_str()) {
            "openai-compatible" => ConfiguredProviderKind::OpenAiCompatible,
            "anthropic" => ConfiguredProviderKind::Anthropic,
            other => {
                return Err(ConfigError::Invalid(format!(
                    "unsupported provider type {other:?} for [providers.{alias}]"
                )));
            }
        };
        let display_name = provider
            .display_name
            .clone()
            .unwrap_or_else(|| alias.as_str().to_owned());
        validate_provider_display_name(&display_name)?;
        let default_model = provider
            .default_model
            .as_deref()
            .or_else(|| {
                providers
                    .default
                    .as_ref()
                    .filter(|default| default.provider == alias.as_str())
                    .map(|default| default.model.as_str())
            })
            .map(ModelName::new)
            .transpose()
            .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        let source = if self.managed_provider_aliases.contains(alias.as_str()) {
            ProviderConfigSource::Managed
        } else {
            ProviderConfigSource::User
        };
        let reasoning_effort =
            parse_provider_reasoning_effort(alias.as_str(), provider.reasoning_effort.as_deref())?;
        let protocol = match kind {
            ConfiguredProviderKind::OpenAiCompatible => Some(provider.protocol.unwrap_or_default()),
            ConfiguredProviderKind::Anthropic => None,
        };

        Ok(ConfiguredProviderProfile {
            alias,
            display_name,
            default_model,
            kind,
            protocol,
            reasoning_effort,
            source,
        })
    }

    pub(crate) fn provider_reasoning_effort(
        &self,
        alias: &str,
    ) -> Result<Option<ReasoningEffort>, ConfigError> {
        Ok(self.provider_profile(alias)?.reasoning_effort().cloned())
    }

    pub(crate) fn effective_provider_reasoning_effort(
        &self,
        alias: &str,
    ) -> Result<Option<ReasoningEffort>, ConfigError> {
        let default_reasoning_effort = self
            .raw
            .providers
            .as_ref()
            .and_then(|providers| providers.default.as_ref())
            .filter(|default| default.provider == alias)
            .and_then(|default| default.reasoning_effort.as_deref())
            .map(ReasoningEffort::new)
            .transpose()
            .map_err(|error| {
                ConfigError::Invalid(format!(
                    "providers.default.reasoning_effort is invalid: {error}"
                ))
            })?;

        match default_reasoning_effort {
            Some(reasoning_effort) => Ok(Some(reasoning_effort)),
            None => self.provider_reasoning_effort(alias),
        }
    }

    pub fn validate_provider_settings_if_present(&self) -> Result<(), ConfigError> {
        let Some(providers) = self.raw.providers.as_ref() else {
            return Ok(());
        };
        for alias in providers.named.keys() {
            let _ = self.provider_profile(alias)?;
            let _ = self.provider_by_alias(alias)?;
        }
        if providers.default.is_some() {
            let _ = self.default_provider()?;
        }
        Ok(())
    }

    pub fn openai_compatible_provider(&self) -> Result<EffectiveOpenAiProviderConfig, ConfigError> {
        let default_alias = self
            .raw
            .providers
            .as_ref()
            .and_then(|providers| providers.default.as_ref())
            .map(|default| default.provider.as_str());
        if let Some(alias) = default_alias
            && alias != "openai-compatible"
            && self
                .raw
                .providers
                .as_ref()
                .is_some_and(|providers| !providers.named.contains_key(alias))
        {
            return Err(ConfigError::Invalid(format!(
                "unsupported default provider {alias}"
            )));
        }
        self.legacy_openai_provider()
    }

    pub fn default_provider(&self) -> Result<EffectiveDefaultProviderConfig, ConfigError> {
        let providers =
            self.raw.providers.as_ref().ok_or_else(|| {
                ConfigError::Invalid("[providers.default] is required".to_owned())
            })?;
        let default = providers
            .default
            .as_ref()
            .ok_or_else(|| ConfigError::Invalid("[providers.default] is required".to_owned()))?;
        let provider = self.provider_by_alias(&default.provider)?;
        let reasoning_effort = self.effective_provider_reasoning_effort(&default.provider)?;
        Ok(EffectiveDefaultProviderConfig {
            alias: default.provider.clone(),
            model: default.model.clone(),
            reasoning_effort,
            provider,
        })
    }

    pub fn configured_default_provider(
        &self,
    ) -> Result<Option<EffectiveDefaultProviderConfig>, ConfigError> {
        if self
            .raw
            .providers
            .as_ref()
            .and_then(|providers| providers.default.as_ref())
            .is_none()
        {
            return Ok(None);
        }
        self.default_provider().map(Some)
    }

    pub fn provider_by_alias(&self, alias: &str) -> Result<EffectiveProviderConfig, ConfigError> {
        let providers = self
            .raw
            .providers
            .as_ref()
            .ok_or_else(|| ConfigError::Invalid(format!("[providers.{alias}] is required")))?;
        let provider = providers
            .named
            .get(alias)
            .ok_or_else(|| ConfigError::Invalid(format!("[providers.{alias}] is required")))?;
        let kind = provider.kind.as_deref().unwrap_or(alias);
        let api_key = resolve_api_key_source(alias, provider, &self.config_dir, &self.home)?;
        let reasoning_effort =
            parse_provider_reasoning_effort(alias, provider.reasoning_effort.as_deref())?;
        match kind {
            "openai-compatible" => Ok(EffectiveProviderConfig::OpenAiCompatible(
                EffectiveOpenAiProviderConfig {
                    model: None,
                    reasoning_effort,
                    alias: alias.to_owned(),
                    protocol: provider.protocol.unwrap_or_default(),
                    base_url: provider.base_url.clone(),
                    api_key,
                },
            )),
            "anthropic" => Ok(EffectiveProviderConfig::Anthropic(
                EffectiveAnthropicProviderConfig {
                    alias: alias.to_owned(),
                    reasoning_effort,
                    base_url: provider.base_url.clone(),
                    api_version: provider.api_version.clone(),
                    default_max_output_tokens: provider.default_max_output_tokens,
                    api_key,
                },
            )),
            other => Err(ConfigError::Invalid(format!(
                "unsupported provider type {other:?} for [providers.{alias}]"
            ))),
        }
    }

    pub(super) fn validate_provider_alias(&self, alias: &str) -> Result<(), ConfigError> {
        let _ = self.provider_by_alias(alias)?;
        Ok(())
    }

    fn legacy_openai_provider(&self) -> Result<EffectiveOpenAiProviderConfig, ConfigError> {
        let default = self.default_provider()?;
        let EffectiveProviderConfig::OpenAiCompatible(mut provider) = default.provider else {
            return Err(ConfigError::Invalid(
                "default provider is not openai-compatible".to_owned(),
            ));
        };
        provider.model = Some(default.model);
        provider.reasoning_effort = default.reasoning_effort;
        Ok(provider)
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

fn resolve_api_key_source(
    alias: &str,
    provider: &NamedProviderToml,
    config_dir: &std::path::Path,
    home: &std::path::Path,
) -> Result<EffectiveOpenAiApiKeySource, ConfigError> {
    let api_key = match (
        provider.api_key.as_deref(),
        provider.api_key_file.as_deref(),
    ) {
        (Some(_), Some(_)) => {
            return Err(ConfigError::Invalid(format!(
                "providers.{alias} must not set both api_key and api_key_file; choose one"
            )));
        }
        (Some(value), None) => {
            validate_api_key_text("api_key", value)?;
            EffectiveOpenAiApiKeySource::Inline(value.to_owned())
        }
        (None, Some(path)) => {
            EffectiveOpenAiApiKeySource::File(resolve_config_relative_path(path, config_dir, home)?)
        }
        (None, None) => {
            return Err(ConfigError::Invalid(format!(
                "providers.{alias} must set exactly one of api_key or api_key_file"
            )));
        }
    };
    Ok(api_key)
}

fn parse_provider_reasoning_effort(
    alias: &str,
    value: Option<&str>,
) -> Result<Option<ReasoningEffort>, ConfigError> {
    value
        .map(ReasoningEffort::new)
        .transpose()
        .map_err(|error| {
            ConfigError::Invalid(format!(
                "providers.{alias}.reasoning_effort is invalid: {error}"
            ))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveDefaultProviderConfig {
    pub alias: String,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub provider: EffectiveProviderConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderConfigSource {
    User,
    Managed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfiguredProviderKind {
    OpenAiCompatible,
    Anthropic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredProviderProfile {
    alias: ProviderAlias,
    display_name: String,
    default_model: Option<ModelName>,
    kind: ConfiguredProviderKind,
    protocol: Option<OpenAiProtocol>,
    reasoning_effort: Option<ReasoningEffort>,
    source: ProviderConfigSource,
}

impl ConfiguredProviderProfile {
    pub(crate) fn alias(&self) -> &ProviderAlias {
        &self.alias
    }

    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    pub(crate) fn default_model(&self) -> Option<&ModelName> {
        self.default_model.as_ref()
    }

    pub(crate) fn kind(&self) -> ConfiguredProviderKind {
        self.kind
    }

    pub(crate) fn protocol(&self) -> Option<OpenAiProtocol> {
        self.protocol
    }

    pub(crate) fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }

    pub(crate) fn source(&self) -> ProviderConfigSource {
        self.source
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveProviderConfig {
    OpenAiCompatible(EffectiveOpenAiProviderConfig),
    Anthropic(EffectiveAnthropicProviderConfig),
}

#[derive(Clone, PartialEq, Eq)]
pub struct EffectiveOpenAiProviderConfig {
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub alias: String,
    pub protocol: OpenAiProtocol,
    pub base_url: Option<String>,
    pub api_key: EffectiveOpenAiApiKeySource,
}

impl fmt::Debug for EffectiveOpenAiProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectiveOpenAiProviderConfig")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("alias", &self.alias)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct EffectiveAnthropicProviderConfig {
    pub alias: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub base_url: Option<String>,
    pub api_version: Option<String>,
    pub default_max_output_tokens: Option<u64>,
    pub api_key: EffectiveOpenAiApiKeySource,
}

impl fmt::Debug for EffectiveAnthropicProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectiveAnthropicProviderConfig")
            .field("alias", &self.alias)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("base_url", &self.base_url)
            .field("api_version", &self.api_version)
            .field("default_max_output_tokens", &self.default_max_output_tokens)
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

impl EffectiveAnthropicProviderConfig {
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

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
pub(super) struct ProvidersToml {
    pub(super) default: Option<DefaultProviderToml>,
    retry: Option<ProviderRetryToml>,
    #[serde(flatten)]
    pub(super) named: BTreeMap<String, NamedProviderToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct DefaultProviderToml {
    pub(super) provider: String,
    model: String,
    reasoning_effort: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct NamedProviderToml {
    pub(super) display_name: Option<String>,
    pub(super) default_model: Option<String>,
    pub(super) reasoning_effort: Option<String>,
    #[serde(rename = "type")]
    pub(super) kind: Option<String>,
    pub(super) protocol: Option<OpenAiProtocol>,
    pub(super) base_url: Option<String>,
    pub(super) api_version: Option<String>,
    pub(super) default_max_output_tokens: Option<u64>,
    pub(super) api_key: Option<String>,
    pub(super) api_key_file: Option<String>,
}

pub(super) fn validate_provider_display_name(value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Invalid(
            "provider display_name must not be blank".to_owned(),
        ));
    }
    if value.trim() != value {
        return Err(ConfigError::Invalid(
            "provider display_name must not have leading or trailing whitespace".to_owned(),
        ));
    }
    if value.chars().count() > 128 {
        return Err(ConfigError::Invalid(
            "provider display_name must be at most 128 characters".to_owned(),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(ConfigError::Invalid(
            "provider display_name must not contain control characters".to_owned(),
        ));
    }
    Ok(())
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

pub(super) fn validate_api_key_text(label: &str, value: &str) -> Result<(), ConfigError> {
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
    use crate::config::{XdgPaths, managed_provider::derive_provider_alias};
    use std::{collections::BTreeSet, path::PathBuf};

    fn home() -> PathBuf {
        PathBuf::from("/home/alice")
    }

    #[test]
    fn derives_readable_unique_managed_provider_aliases() {
        let used = BTreeSet::from(["opencode".to_owned(), "opencode-2".to_owned()]);

        assert_eq!(
            derive_provider_alias("OpenCode", &used)
                .expect("alias should derive")
                .as_str(),
            "opencode-3"
        );
        assert_eq!(
            derive_provider_alias("OpenCode Gateway", &BTreeSet::new())
                .expect("alias should derive")
                .as_str(),
            "opencode-gateway"
        );
        assert!(derive_provider_alias("Default", &BTreeSet::new()).is_err());
    }

    #[test]
    fn provider_profile_preserves_display_name_and_default_model() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "opencode"
model = "deepseek-v4-pro"

[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
reasoning_effort = "low"
type = "openai-compatible"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist");

        let profile = config
            .provider_profile("opencode")
            .expect("profile should resolve");

        assert_eq!(profile.alias().as_str(), "opencode");
        assert_eq!(profile.display_name(), "OpenCode");
        assert_eq!(
            profile.default_model().map(merry_llm::ModelName::as_str),
            Some("deepseek-v4-pro")
        );
        assert_eq!(profile.source(), ProviderConfigSource::User);
        assert_eq!(profile.protocol(), Some(OpenAiProtocol::Responses));
        assert_eq!(
            profile.reasoning_effort().map(ReasoningEffort::as_str),
            Some("low")
        );
        let provider = config
            .provider_by_alias("opencode")
            .expect("provider should resolve");
        assert!(matches!(
            provider,
            EffectiveProviderConfig::OpenAiCompatible(provider)
                if provider.reasoning_effort.as_ref().map(ReasoningEffort::as_str) == Some("low")
        ));
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
    fn parses_chat_completions_protocol_and_anthropic_provider() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let openai = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "compat"
model = "gpt-test"

[providers.compat]
type = "openai-compatible"
protocol = "chat_completions"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist")
        .default_provider()
        .expect("default provider should validate");
        let EffectiveProviderConfig::OpenAiCompatible(openai) = openai.provider else {
            panic!("OpenAI-compatible provider expected");
        };
        assert_eq!(openai.alias, "compat");
        assert_eq!(openai.protocol, OpenAiProtocol::ChatCompletions);

        let anthropic = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "claude"
model = "claude-sonnet-test"

[providers.claude]
type = "anthropic"
base_url = "https://anthropic.example.test"
api_version = "2023-06-01"
default_max_output_tokens = 2048
api_key = "sk-ant-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist")
        .default_provider()
        .expect("default provider should validate");
        assert_eq!(anthropic.alias, "claude");
        assert_eq!(anthropic.model, "claude-sonnet-test");
        let EffectiveProviderConfig::Anthropic(anthropic) = anthropic.provider else {
            panic!("Anthropic provider expected");
        };
        assert_eq!(anthropic.alias, "claude");
        assert_eq!(anthropic.default_max_output_tokens, Some(2048));
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
            alias: "openai-compatible".to_owned(),
            protocol: OpenAiProtocol::Responses,
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
