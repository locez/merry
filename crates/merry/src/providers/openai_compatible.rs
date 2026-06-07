use super::ConfiguredModelProvider;
use merry_llm::{ModelError, ModelName, ModelRetryPolicy};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig, OpenAiProviderError};
use std::sync::Arc;
use thiserror::Error;

/// Creates an OpenAI-compatible provider component builder.
#[must_use]
pub fn openai_compatible() -> OpenAiCompatibleProviderBuilder {
    OpenAiCompatibleProviderBuilder::new()
}

/// Builder for OpenAI-compatible provider components.
#[derive(Clone, Default)]
pub struct OpenAiCompatibleProviderBuilder {
    config: Option<OpenAiProviderConfig>,
    model: Option<ModelName>,
    retry_policy: Option<ModelRetryPolicy>,
}

impl OpenAiCompatibleProviderBuilder {
    /// Creates an empty OpenAI-compatible provider builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the full provider configuration.
    #[must_use]
    pub fn provider_config(mut self, config: OpenAiProviderConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Sets the provider API key and default OpenAI-compatible config.
    pub fn api_key(mut self, api_key: &str) -> Result<Self, OpenAiCompatibleProviderBuildError> {
        self.config = Some(OpenAiProviderConfig::new(api_key)?);
        Ok(self)
    }

    /// Sets the provider base URL on the existing provider config.
    ///
    /// Call [`OpenAiCompatibleProviderBuilder::api_key`] or
    /// [`OpenAiCompatibleProviderBuilder::provider_config`] before this method.
    pub fn base_url(mut self, base_url: &str) -> Result<Self, OpenAiCompatibleProviderBuildError> {
        let config = self
            .config
            .ok_or(OpenAiCompatibleProviderBuildError::MissingProviderConfig)?;
        self.config = Some(config.with_base_url(base_url)?);
        Ok(self)
    }

    /// Sets the provider model.
    #[must_use]
    pub fn model(mut self, model: ModelName) -> Self {
        self.model = Some(model);
        self
    }

    /// Sets and validates the provider model by name.
    pub fn model_name(mut self, model: &str) -> Result<Self, OpenAiCompatibleProviderBuildError> {
        self.model = Some(ModelName::new(model)?);
        Ok(self)
    }

    /// Sets provider-neutral retry behavior for this provider component.
    #[must_use]
    pub fn retry_policy(mut self, retry_policy: ModelRetryPolicy) -> Self {
        self.retry_policy = Some(retry_policy);
        self
    }

    /// Builds a provider-neutral configured provider component.
    pub fn build(self) -> Result<ConfiguredModelProvider, OpenAiCompatibleProviderBuildError> {
        let config = self
            .config
            .ok_or(OpenAiCompatibleProviderBuildError::MissingProviderConfig)?;
        let model = self
            .model
            .ok_or(OpenAiCompatibleProviderBuildError::MissingModel)?;
        let provider =
            ConfiguredModelProvider::primary(Arc::new(OpenAiProvider::new(config)), model);
        Ok(if let Some(retry_policy) = self.retry_policy {
            provider.with_retry_policy(retry_policy)
        } else {
            provider
        })
    }
}

/// Errors raised while constructing an OpenAI-compatible provider component.
#[derive(Debug, Error)]
pub enum OpenAiCompatibleProviderBuildError {
    /// No provider config was supplied.
    #[error("OpenAI-compatible provider config is missing")]
    MissingProviderConfig,
    /// No model was supplied.
    #[error("OpenAI-compatible model is missing")]
    MissingModel,
    /// Provider configuration was invalid.
    #[error(transparent)]
    Provider(#[from] OpenAiProviderError),
    /// Model configuration was invalid.
    #[error(transparent)]
    Model(#[from] ModelError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_provider_component_without_network_request() {
        let provider = openai_compatible()
            .api_key("sk-test")
            .expect("api key should be valid")
            .base_url("https://api.example.test/v1")
            .expect("base URL should be valid")
            .model_name("gpt-test")
            .expect("model should be valid")
            .retry_policy(ModelRetryPolicy::coding_agent_default())
            .build()
            .expect("provider component should build");

        assert_eq!(provider.model().as_str(), "gpt-test");
    }

    #[test]
    fn validates_provider_component_inputs() {
        assert!(matches!(
            openai_compatible().api_key(""),
            Err(OpenAiCompatibleProviderBuildError::Provider(_))
        ));
        assert!(matches!(
            openai_compatible().model_name(" "),
            Err(OpenAiCompatibleProviderBuildError::Model(_))
        ));
        assert!(matches!(
            openai_compatible().base_url("https://api.example.test/v1"),
            Err(OpenAiCompatibleProviderBuildError::MissingProviderConfig)
        ));
        assert!(matches!(
            openai_compatible()
                .api_key("sk-test")
                .expect("api key should be valid")
                .build(),
            Err(OpenAiCompatibleProviderBuildError::MissingModel)
        ));
    }
}
