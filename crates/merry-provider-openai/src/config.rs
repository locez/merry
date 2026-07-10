//! Public configuration for the OpenAI-compatible provider.

use crate::OpenAiProviderError;
use merry_core::ProviderName;
use merry_llm::ModelCapabilities;
use serde::{Deserialize, Serialize};
use std::fmt;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_PROVIDER_NAME: &str = "openai-compatible";

/// OpenAI-compatible wire protocol used by the provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiProtocol {
    /// OpenAI Responses API at `/responses`.
    #[default]
    Responses,
    /// OpenAI Chat Completions API at `/chat/completions`.
    ChatCompletions,
}

/// OpenAI-compatible provider configuration.
#[derive(Clone)]
pub struct OpenAiProviderConfig {
    api_key: String,
    base_url: String,
    organization: Option<String>,
    project: Option<String>,
    provider_name: ProviderName,
    capabilities: ModelCapabilities,
    protocol: OpenAiProtocol,
}

impl OpenAiProviderConfig {
    /// Creates a validated provider config with OpenAI-compatible defaults.
    pub fn new(api_key: &str) -> Result<Self, OpenAiProviderError> {
        validate_required_text("api_key", api_key)?;

        Ok(Self {
            api_key: api_key.to_owned(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            organization: None,
            project: None,
            provider_name: ProviderName::new(DEFAULT_PROVIDER_NAME).map_err(|error| {
                OpenAiProviderError::invalid_config(format!(
                    "default provider name is invalid: {error}"
                ))
            })?,
            capabilities: default_capabilities()?,
            protocol: OpenAiProtocol::Responses,
        })
    }

    /// Returns a redacted API key for diagnostics.
    #[must_use]
    pub fn api_key_redacted(&self) -> String {
        redact_secret(&self.api_key)
    }

    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Returns the base URL used for Responses requests.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Returns the optional organization header value.
    #[must_use]
    pub fn organization(&self) -> Option<&str> {
        self.organization.as_deref()
    }

    /// Returns the optional project header value.
    #[must_use]
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// Returns the Merry-owned provider name.
    #[must_use]
    pub fn provider_name(&self) -> &ProviderName {
        &self.provider_name
    }

    /// Returns the provider capability declaration.
    #[must_use]
    pub fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    /// Returns the selected OpenAI-compatible wire protocol.
    #[must_use]
    pub fn protocol(&self) -> OpenAiProtocol {
        self.protocol
    }

    /// Returns a config with a validated replacement base URL.
    pub fn with_base_url(mut self, base_url: &str) -> Result<Self, OpenAiProviderError> {
        validate_base_url(base_url)?;
        self.base_url = base_url.to_owned();
        Ok(self)
    }

    /// Returns a config with a validated organization value.
    pub fn with_organization(mut self, organization: &str) -> Result<Self, OpenAiProviderError> {
        validate_required_text("organization", organization)?;
        self.organization = Some(organization.to_owned());
        Ok(self)
    }

    /// Returns a config with a validated project value.
    pub fn with_project(mut self, project: &str) -> Result<Self, OpenAiProviderError> {
        validate_required_text("project", project)?;
        self.project = Some(project.to_owned());
        Ok(self)
    }

    /// Returns a config with a validated Merry-owned provider name.
    pub fn with_provider_name(mut self, provider_name: &str) -> Result<Self, OpenAiProviderError> {
        self.provider_name = ProviderName::new(provider_name).map_err(|error| {
            OpenAiProviderError::invalid_config(format!("provider_name is invalid: {error}"))
        })?;
        Ok(self)
    }

    /// Returns a config with a replacement capability declaration.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Returns a config using the selected OpenAI-compatible wire protocol.
    #[must_use]
    pub fn with_protocol(mut self, protocol: OpenAiProtocol) -> Self {
        self.protocol = protocol;
        self
    }
}

impl fmt::Debug for OpenAiProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiProviderConfig")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("organization", &self.organization)
            .field("project", &self.project)
            .field("provider_name", &self.provider_name)
            .field("capabilities", &self.capabilities)
            .field("protocol", &self.protocol)
            .finish()
    }
}

fn default_capabilities() -> Result<ModelCapabilities, OpenAiProviderError> {
    ModelCapabilities::new(true, true, true, true, None, None).map_err(|error| {
        OpenAiProviderError::invalid_config(format!("default capabilities are invalid: {error}"))
    })
}

fn validate_required_text(field: &'static str, value: &str) -> Result<(), OpenAiProviderError> {
    if value.trim().is_empty() {
        return Err(OpenAiProviderError::invalid_config(format!(
            "{field} must not be blank"
        )));
    }

    if value.trim() != value {
        return Err(OpenAiProviderError::invalid_config(format!(
            "{field} must not have leading or trailing whitespace"
        )));
    }

    if value.chars().any(char::is_control) {
        return Err(OpenAiProviderError::invalid_config(format!(
            "{field} must not contain control characters"
        )));
    }

    Ok(())
}

fn validate_base_url(base_url: &str) -> Result<(), OpenAiProviderError> {
    validate_required_text("base_url", base_url)?;
    let url = reqwest::Url::parse(base_url).map_err(|error| {
        OpenAiProviderError::invalid_config(format!("base_url is invalid: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(OpenAiProviderError::invalid_config(
            "base_url must use http or https",
        ));
    }
    if base_url
        .split_once("://")
        .is_none_or(|(_, authority)| authority.starts_with('/'))
    {
        return Err(OpenAiProviderError::invalid_config(
            "base_url must include a host",
        ));
    }
    if url.host_str().is_none() {
        return Err(OpenAiProviderError::invalid_config(
            "base_url must include a host",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(OpenAiProviderError::invalid_config(
            "base_url must not include a query or fragment",
        ));
    }

    Ok(())
}

fn redact_secret(secret: &str) -> String {
    let char_count = secret.chars().count();
    if char_count <= 4 {
        return "<redacted>".to_owned();
    }

    let prefix: String = secret.chars().take(3).collect();
    let suffix_len = 4.min(char_count.saturating_sub(3));
    let suffix: String = secret
        .chars()
        .skip(char_count - suffix_len)
        .take(suffix_len)
        .collect();

    format!("{prefix}...{suffix}")
}
