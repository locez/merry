use merry_llm::{ModelError, ProviderErrorKind};
use std::time::Duration;
use thiserror::Error;

/// Errors returned by the Anthropic Messages adapter.
#[derive(Debug, Error)]
pub enum AnthropicProviderError {
    #[error("invalid Anthropic provider config: {reason}")]
    InvalidConfig { reason: String },
    #[error("invalid Anthropic provider request: {reason}")]
    InvalidRequest { reason: String },
    #[error("invalid Anthropic tool call: {reason}")]
    InvalidToolCall { reason: String },
    #[error("Anthropic provider protocol error: {reason}")]
    Protocol { reason: String },
    #[error("Anthropic provider request failed ({kind:?}): {message}")]
    Provider {
        kind: ProviderErrorKind,
        message: String,
        retry_after: Option<Duration>,
    },
}

impl AnthropicProviderError {
    pub(crate) fn invalid_config(reason: impl Into<String>) -> Self {
        Self::InvalidConfig {
            reason: reason.into(),
        }
    }

    pub(crate) fn invalid_request(reason: impl Into<String>) -> Self {
        Self::InvalidRequest {
            reason: reason.into(),
        }
    }

    pub(crate) fn protocol(reason: impl Into<String>) -> Self {
        Self::Protocol {
            reason: reason.into(),
        }
    }

    pub(crate) fn invalid_tool_call(reason: impl Into<String>) -> Self {
        Self::InvalidToolCall {
            reason: reason.into(),
        }
    }

    pub(crate) fn provider_with_retry_after(
        kind: ProviderErrorKind,
        message: impl Into<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        Self::Provider {
            kind,
            message: message.into(),
            retry_after,
        }
    }
}

impl From<AnthropicProviderError> for ModelError {
    fn from(error: AnthropicProviderError) -> Self {
        match error {
            AnthropicProviderError::InvalidConfig { reason }
            | AnthropicProviderError::InvalidRequest { reason } => Self::invalid_request(reason),
            AnthropicProviderError::InvalidToolCall { reason } => {
                Self::provider_with_retry_after(ProviderErrorKind::InvalidToolCall, reason, None)
            }
            AnthropicProviderError::Protocol { reason } => {
                Self::provider(ProviderErrorKind::Protocol, reason)
            }
            AnthropicProviderError::Provider {
                kind,
                message,
                retry_after,
            } => Self::provider_with_retry_after(kind, message, retry_after),
        }
    }
}
