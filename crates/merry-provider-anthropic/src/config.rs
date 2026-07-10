use crate::AnthropicProviderError;
use merry_core::ProviderName;
use merry_llm::ModelCapabilities;
use std::{fmt, num::NonZeroU64};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_API_VERSION: &str = "2023-06-01";
const DEFAULT_PROVIDER_NAME: &str = "anthropic";
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 4096;

/// Public Anthropic Messages provider configuration.
#[derive(Clone)]
pub struct AnthropicProviderConfig {
    api_key: String,
    base_url: String,
    api_version: String,
    default_max_output_tokens: NonZeroU64,
    provider_name: ProviderName,
    capabilities: ModelCapabilities,
}

impl AnthropicProviderConfig {
    /// Creates a validated config with Anthropic API defaults.
    pub fn new(api_key: &str) -> Result<Self, AnthropicProviderError> {
        validate_required_text("api_key", api_key)?;
        Ok(Self {
            api_key: api_key.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            api_version: DEFAULT_API_VERSION.to_owned(),
            default_max_output_tokens: NonZeroU64::new(DEFAULT_MAX_OUTPUT_TOKENS)
                .expect("default output-token limit is non-zero"),
            provider_name: ProviderName::new(DEFAULT_PROVIDER_NAME).map_err(|error| {
                AnthropicProviderError::invalid_config(format!(
                    "default provider name is invalid: {error}"
                ))
            })?,
            capabilities: ModelCapabilities::new(true, true, true, true, None, None).map_err(
                |error| {
                    AnthropicProviderError::invalid_config(format!(
                        "default capabilities are invalid: {error}"
                    ))
                },
            )?,
        })
    }

    #[must_use]
    pub fn api_key_redacted(&self) -> String {
        redact_secret(&self.api_key)
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn api_version(&self) -> &str {
        &self.api_version
    }

    #[must_use]
    pub fn default_max_output_tokens(&self) -> NonZeroU64 {
        self.default_max_output_tokens
    }

    #[must_use]
    pub fn provider_name(&self) -> &ProviderName {
        &self.provider_name
    }

    #[must_use]
    pub fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    pub fn with_base_url(mut self, base_url: &str) -> Result<Self, AnthropicProviderError> {
        validate_base_url(base_url)?;
        self.base_url = base_url.to_owned();
        Ok(self)
    }

    pub fn with_api_version(mut self, version: &str) -> Result<Self, AnthropicProviderError> {
        validate_required_text("api_version", version)?;
        self.api_version = version.to_owned();
        Ok(self)
    }

    #[must_use]
    pub fn with_default_max_output_tokens(mut self, limit: NonZeroU64) -> Self {
        self.default_max_output_tokens = limit;
        self
    }

    pub fn with_provider_name(mut self, name: &str) -> Result<Self, AnthropicProviderError> {
        self.provider_name = ProviderName::new(name).map_err(|error| {
            AnthropicProviderError::invalid_config(format!("provider_name is invalid: {error}"))
        })?;
        Ok(self)
    }

    #[must_use]
    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

impl fmt::Debug for AnthropicProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicProviderConfig")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("api_version", &self.api_version)
            .field("default_max_output_tokens", &self.default_max_output_tokens)
            .field("provider_name", &self.provider_name)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

fn validate_required_text(field: &'static str, value: &str) -> Result<(), AnthropicProviderError> {
    if value.trim().is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(AnthropicProviderError::invalid_config(format!(
            "{field} must be non-blank, trimmed, and free of control characters"
        )));
    }
    Ok(())
}

fn validate_base_url(base_url: &str) -> Result<(), AnthropicProviderError> {
    validate_required_text("base_url", base_url)?;
    let url = reqwest::Url::parse(base_url).map_err(|error| {
        AnthropicProviderError::invalid_config(format!("base_url is invalid: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
        || base_url
            .split_once("://")
            .is_none_or(|(_, authority)| authority.starts_with('/'))
    {
        return Err(AnthropicProviderError::invalid_config(
            "base_url must be an http(s) URL with a host and no query or fragment",
        ));
    }
    Ok(())
}

fn redact_secret(secret: &str) -> String {
    if secret.chars().count() <= 4 {
        return "<redacted>".to_owned();
    }
    let prefix = secret.chars().take(3).collect::<String>();
    let suffix = secret.chars().rev().take(4).collect::<String>();
    format!("{prefix}...{}", suffix.chars().rev().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_valid_and_debug_is_redacted() {
        let config = AnthropicProviderConfig::new("sk-ant-test").expect("valid config");
        assert_eq!(config.base_url(), DEFAULT_BASE_URL);
        assert_eq!(config.api_version(), DEFAULT_API_VERSION);
        assert_eq!(config.default_max_output_tokens().get(), 4096);
        assert_eq!(config.provider_name().as_str(), "anthropic");
        assert!(config.capabilities().supports_parallel_tool_calls());
        let debug = format!("{config:?}");
        assert!(!debug.contains("sk-ant-test"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn config_rejects_invalid_secrets_versions_and_urls() {
        for key in ["", " key", "key ", "bad\nkey"] {
            assert!(AnthropicProviderConfig::new(key).is_err());
        }
        assert!(
            AnthropicProviderConfig::new("key")
                .and_then(|config| config.with_base_url("ftp://example.com"))
                .is_err()
        );
        assert!(
            AnthropicProviderConfig::new("key")
                .and_then(|config| config.with_base_url("https://example.com?secret=1"))
                .is_err()
        );
        assert!(
            AnthropicProviderConfig::new("key")
                .and_then(|config| config.with_api_version(" "))
                .is_err()
        );
    }
}
