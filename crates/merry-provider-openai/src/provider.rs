//! Provider holder for the OpenAI-compatible adapter.

use crate::OpenAiProviderConfig;

/// Config-backed OpenAI-compatible provider skeleton.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    config: OpenAiProviderConfig,
}

impl OpenAiProvider {
    /// Creates a provider holder from validated config.
    #[must_use]
    pub fn new(config: OpenAiProviderConfig) -> Self {
        Self { config }
    }

    /// Returns the provider configuration.
    #[must_use]
    pub fn config(&self) -> &OpenAiProviderConfig {
        &self.config
    }
}
