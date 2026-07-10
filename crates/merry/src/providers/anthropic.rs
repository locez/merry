use super::ConfiguredModelProvider;
use merry_llm::{ModelError, ModelName, ModelRetryPolicy};
use merry_provider_anthropic::{
    AnthropicProvider, AnthropicProviderConfig, AnthropicProviderError,
};
use std::sync::Arc;
use thiserror::Error;

/// Creates an Anthropic provider component builder.
#[must_use]
pub fn anthropic() -> AnthropicProviderBuilder {
    AnthropicProviderBuilder::new()
}

/// Builder for Anthropic provider components.
#[derive(Clone, Default)]
pub struct AnthropicProviderBuilder {
    config: Option<AnthropicProviderConfig>,
    model: Option<ModelName>,
    retry_policy: Option<ModelRetryPolicy>,
}

impl AnthropicProviderBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn provider_config(mut self, config: AnthropicProviderConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn api_key(mut self, api_key: &str) -> Result<Self, AnthropicProviderBuildError> {
        self.config = Some(AnthropicProviderConfig::new(api_key)?);
        Ok(self)
    }

    pub fn base_url(mut self, base_url: &str) -> Result<Self, AnthropicProviderBuildError> {
        let config = self
            .config
            .ok_or(AnthropicProviderBuildError::MissingProviderConfig)?;
        self.config = Some(config.with_base_url(base_url)?);
        Ok(self)
    }

    #[must_use]
    pub fn model(mut self, model: ModelName) -> Self {
        self.model = Some(model);
        self
    }

    pub fn model_name(mut self, model: &str) -> Result<Self, AnthropicProviderBuildError> {
        self.model = Some(ModelName::new(model)?);
        Ok(self)
    }

    #[must_use]
    pub fn retry_policy(mut self, retry_policy: ModelRetryPolicy) -> Self {
        self.retry_policy = Some(retry_policy);
        self
    }

    pub fn build(self) -> Result<ConfiguredModelProvider, AnthropicProviderBuildError> {
        let config = self
            .config
            .ok_or(AnthropicProviderBuildError::MissingProviderConfig)?;
        let model = self
            .model
            .ok_or(AnthropicProviderBuildError::MissingModel)?;
        let provider =
            ConfiguredModelProvider::primary(Arc::new(AnthropicProvider::new(config)), model);
        Ok(if let Some(retry_policy) = self.retry_policy {
            provider.with_retry_policy(retry_policy)
        } else {
            provider
        })
    }
}

#[derive(Debug, Error)]
pub enum AnthropicProviderBuildError {
    #[error("Anthropic provider config is missing")]
    MissingProviderConfig,
    #[error("Anthropic model is missing")]
    MissingModel,
    #[error(transparent)]
    Provider(#[from] AnthropicProviderError),
    #[error(transparent)]
    Model(#[from] ModelError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_anthropic_component_without_network() {
        let provider = anthropic()
            .api_key("sk-ant-test")
            .expect("valid key")
            .model_name("claude-test")
            .expect("valid model")
            .build()
            .expect("component should build");
        assert_eq!(provider.model().as_str(), "claude-test");
    }
}
