//! Provider components for Merry runtime construction.

mod anthropic;
mod openai_compatible;

use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use merry_runtime::{RuntimeBuilder, RuntimeModelRole};
use std::sync::Arc;

pub use anthropic::{AnthropicProviderBuildError, AnthropicProviderBuilder, anthropic};
pub use merry_provider_anthropic::{
    AnthropicProvider, AnthropicProviderConfig, AnthropicProviderError,
};
pub use merry_provider_openai::{OpenAiProtocol, OpenAiProviderConfig, OpenAiProviderError};
pub use openai_compatible::{
    OpenAiCompatibleProviderBuildError, OpenAiCompatibleProviderBuilder, openai_compatible,
};

/// A configured model provider component.
///
/// This is provider-neutral. OpenAI-compatible, Gemini, Claude, local model,
/// and custom provider builders should all produce this same shape.
#[derive(Clone)]
pub struct ConfiguredModelProvider {
    role: RuntimeModelRole,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    retry_policy: Option<ModelRetryPolicy>,
}

impl ConfiguredModelProvider {
    /// Creates a primary model provider component.
    #[must_use]
    pub fn primary(provider: Arc<dyn ModelProvider>, model: ModelName) -> Self {
        Self {
            role: RuntimeModelRole::Primary,
            provider,
            model,
            retry_policy: None,
        }
    }

    /// Creates a model provider component for a specific runtime role.
    #[must_use]
    pub fn for_role(
        role: RuntimeModelRole,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
    ) -> Self {
        Self {
            role,
            provider,
            model,
            retry_policy: None,
        }
    }

    /// Sets provider-neutral retry policy for this component.
    #[must_use]
    pub fn with_retry_policy(mut self, retry_policy: ModelRetryPolicy) -> Self {
        self.retry_policy = Some(retry_policy);
        self
    }

    /// Returns the runtime model role for this provider component.
    #[must_use]
    pub const fn role(&self) -> RuntimeModelRole {
        self.role
    }

    /// Returns the configured model.
    #[must_use]
    pub fn model(&self) -> &ModelName {
        &self.model
    }

    fn into_parts(self) -> ProviderComponentParts {
        ProviderComponentParts {
            role: self.role,
            provider: self.provider,
            model: self.model,
            retry_policy: self.retry_policy,
        }
    }
}

struct ProviderComponentParts {
    role: RuntimeModelRole,
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
    retry_policy: Option<ModelRetryPolicy>,
}

/// Facade extension for installing configured provider components.
pub trait RuntimeBuilderProviderExt {
    /// Installs a provider component into the runtime builder.
    ///
    /// This is a facade-level composition helper. It does not introduce a
    /// provider-specific runtime type.
    fn with_provider(self, provider: ConfiguredModelProvider) -> Self;
}

impl RuntimeBuilderProviderExt for RuntimeBuilder {
    fn with_provider(self, provider: ConfiguredModelProvider) -> Self {
        let ProviderComponentParts {
            role,
            provider,
            model,
            retry_policy,
        } = provider.into_parts();
        if let Some(retry_policy) = retry_policy {
            self.model_provider_for_role_with_retry(role, provider, model, retry_policy)
        } else {
            self.model_provider_for_role(role, provider, model)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::ProviderName;
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelEvent, ModelEventStream, ModelOutput,
        ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext,
    };
    use merry_runtime::Runtime;
    use std::sync::OnceLock;

    #[derive(Clone)]
    struct StaticModelProvider;

    fn fake_provider() -> Arc<dyn ModelProvider> {
        Arc::new(StaticModelProvider)
    }

    impl ModelProvider for StaticModelProvider {
        fn name(&self) -> &ProviderName {
            static PROVIDER_NAME: OnceLock<ProviderName> = OnceLock::new();
            PROVIDER_NAME
                .get_or_init(|| ProviderName::new("facade-test-provider").expect("valid name"))
        }

        fn capabilities(&self) -> &ModelCapabilities {
            static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
            CAPABILITIES.get_or_init(|| {
                ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities")
            })
        }

        fn stream_model<'a>(
            &'a self,
            _request: ModelRequest,
            _context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, merry_llm::ModelError>> {
            Box::pin(async move {
                let stream: ModelEventStream = Box::pin(futures_util::stream::iter([
                    Ok(ModelEvent::Started),
                    Ok(ModelEvent::Completed {
                        response: ModelResponse::new(
                            vec![ModelOutput::text("ok")],
                            FinishReason::Stop,
                            None,
                        ),
                    }),
                ]));
                Ok(stream)
            })
        }
    }

    #[test]
    fn configured_provider_installs_on_runtime_builder() {
        let session_id =
            merry_core::SessionId::new("provider-component-test").expect("valid session id");
        let model = ModelName::new("fake-model").expect("valid model name");
        let provider = ConfiguredModelProvider::primary(fake_provider(), model.clone());

        Runtime::builder(session_id)
            .with_provider(provider)
            .build()
            .expect("runtime should build");
    }
}
