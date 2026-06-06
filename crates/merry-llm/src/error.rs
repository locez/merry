//! Provider-boundary errors.

use std::time::Duration;
use thiserror::Error;

/// Broad provider error category for policy and retry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Request validation failed before provider work could start.
    InvalidRequest,
    /// Provider work was cancelled cooperatively.
    Cancelled,
    /// Provider authentication or authorization failed.
    Authentication,
    /// Provider rejected the request because a quota or rate limit was exceeded.
    RateLimited,
    /// Provider reported a temporary or transport-level failure.
    Unavailable,
    /// Provider returned a response Merry could not interpret.
    Protocol,
    /// Provider failed for an uncategorized reason.
    Other,
}

/// Error returned by Merry-owned model provider implementations.
#[derive(Debug, Error)]
pub enum ModelError {
    /// Model request failed local validation.
    #[error("invalid model request: {reason}")]
    InvalidRequest { reason: String },
    /// Provider work was cancelled before or during streaming.
    #[error("model stream cancelled")]
    Cancelled,
    /// Provider reported an actionable failure.
    #[error("provider error ({kind:?}): {message}")]
    Provider {
        /// Stable provider-neutral error category.
        kind: ProviderErrorKind,
        /// Actionable provider-neutral error message.
        message: String,
        /// Optional provider-supplied retry-after delay.
        retry_after: Option<Duration>,
    },
}

impl ModelError {
    /// Creates an invalid-request error.
    pub fn invalid_request(reason: impl Into<String>) -> Self {
        Self::InvalidRequest {
            reason: reason.into(),
        }
    }

    /// Creates a provider error with a provider-neutral category and message.
    pub fn provider(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self::Provider {
            kind,
            message: message.into(),
            retry_after: None,
        }
    }

    /// Creates a provider error with a provider-neutral category, message, and
    /// optional provider-supplied retry-after delay.
    pub fn provider_with_retry_after(
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

    /// Returns the provider-neutral category for this error.
    #[must_use]
    pub fn kind(&self) -> ProviderErrorKind {
        match self {
            Self::InvalidRequest { .. } => ProviderErrorKind::InvalidRequest,
            Self::Cancelled => ProviderErrorKind::Cancelled,
            Self::Provider { kind, .. } => *kind,
        }
    }

    /// Returns a provider-supplied retry-after delay when available.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Provider { retry_after, .. } => *retry_after,
            Self::InvalidRequest { .. } | Self::Cancelled => None,
        }
    }
}
