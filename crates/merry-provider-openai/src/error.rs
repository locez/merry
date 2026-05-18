//! OpenAI-compatible provider errors.

use merry_llm::{ModelError, ProviderErrorKind};
use thiserror::Error;

/// Errors returned by the OpenAI-compatible provider adapter.
#[derive(Debug, Error)]
pub enum OpenAiProviderError {
    /// Local configuration is invalid.
    #[error("invalid OpenAI provider config: {reason}")]
    InvalidConfig { reason: String },
    /// Provider payload could not be rendered from Merry-owned model types.
    #[error("invalid OpenAI provider request: {reason}")]
    InvalidRequest { reason: String },
    /// Provider response did not match the expected Chat Completions protocol.
    #[error("OpenAI provider protocol error: {reason}")]
    Protocol { reason: String },
    /// Provider returned a non-successful HTTP status or transport failure.
    #[error("OpenAI provider request failed ({kind:?}): {message}")]
    Provider {
        /// Provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider-neutral message.
        message: String,
    },
}

impl OpenAiProviderError {
    pub(crate) fn invalid_config(reason: impl Into<String>) -> Self {
        Self::InvalidConfig {
            reason: reason.into(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn invalid_request(reason: impl Into<String>) -> Self {
        Self::InvalidRequest {
            reason: reason.into(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn protocol(reason: impl Into<String>) -> Self {
        Self::Protocol {
            reason: reason.into(),
        }
    }

    pub(crate) fn provider(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self::Provider {
            kind,
            message: message.into(),
        }
    }
}

impl From<OpenAiProviderError> for ModelError {
    fn from(error: OpenAiProviderError) -> Self {
        match error {
            OpenAiProviderError::InvalidConfig { reason }
            | OpenAiProviderError::InvalidRequest { reason } => Self::invalid_request(reason),
            OpenAiProviderError::Protocol { reason } => {
                Self::provider(ProviderErrorKind::Protocol, reason)
            }
            OpenAiProviderError::Provider { kind, message } => Self::provider(kind, message),
        }
    }
}
