//! Permission mode selection and its mapping onto runtime permission review.
//!
//! The product surface names [`CodingPermissionMode`]. This module turns that
//! choice into the [`CodingPermissionPolicy`] both parent and child coding
//! runtimes apply. A denial is a tool result for the working model; it does
//! not end the turn.

use merry_runtime::{PermissionAdmissionSource, PermissionReviewMode, RuntimeBuilder};
use std::sync::Arc;
use thiserror::Error;

/// What happens when `auto` review denies an action or cannot decide.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodingAutoFallback {
    /// Ask the person at the terminal.
    #[default]
    Ask,
    /// Return the denial to the working model and let that model end the turn.
    Deny,
}

/// Who reviews permission requests, as selected by the product surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodingPermissionMode {
    /// The approval-review model decides first.
    #[default]
    Auto,
    /// The person at the terminal decides.
    Ask,
    /// Configured actions run without a review round.
    AlwaysApprove,
    /// Refuse every permission request without consulting a reviewer.
    Deny,
}

/// Failure to construct a permission policy for a permission mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodingPermissionPolicyError {
    /// A selected mode needs a host admission source, but the surface did not provide one.
    #[error("{mode:?} permission review requires a host admission source")]
    HostAdmissionUnavailable { mode: CodingPermissionMode },
}

/// Permission configuration shared by parent and child coding runtimes.
///
/// This is the only coding-layer representation of permission mode and host
/// admission. Host-dependent variants carry their source directly, so an
/// incomplete host policy cannot be constructed by callers.
#[derive(Clone, Default)]
pub enum CodingPermissionPolicy {
    /// Use the runtime's trust-level default.
    #[default]
    Default,
    /// Route permission requests through model-backed review without a host fallback.
    Required,
    /// Route permission requests through the supplied host admission source.
    HostDecisionOnly {
        /// Host-owned admission source used by the runtime.
        source: Arc<dyn PermissionAdmissionSource>,
    },
    /// Try model-backed review first and use the supplied host source as a fallback.
    ModelThenHostFallback {
        /// Host-owned admission source used by the runtime.
        source: Arc<dyn PermissionAdmissionSource>,
    },
    /// Admit configured registered tools without an approval round.
    FullyTrusted,
    /// Reject every permission request without an approval round.
    DenyAll,
}

impl CodingPermissionPolicy {
    /// Route permission requests through model-backed review without a host fallback.
    #[must_use]
    pub const fn model_only() -> Self {
        Self::Required
    }

    /// Route permission requests through the supplied host admission source.
    #[must_use]
    pub fn host_decision_only(source: Arc<dyn PermissionAdmissionSource>) -> Self {
        Self::HostDecisionOnly { source }
    }

    /// Try model-backed review first and use the supplied host source as a fallback.
    #[must_use]
    pub fn model_then_host_fallback(source: Arc<dyn PermissionAdmissionSource>) -> Self {
        Self::ModelThenHostFallback { source }
    }

    /// Admit configured registered tools without an approval round.
    #[must_use]
    pub const fn fully_trusted() -> Self {
        Self::FullyTrusted
    }

    /// Reject every permission request without an approval round.
    #[must_use]
    pub const fn deny_all() -> Self {
        Self::DenyAll
    }

    /// Selects the runtime policy for one product permission mode.
    ///
    /// `fallback` is read only for [`CodingPermissionMode::Auto`]. A mode that
    /// asks a person is constructed only when the caller supplies a host
    /// admission source.
    pub fn for_permission_mode(
        mode: CodingPermissionMode,
        fallback: CodingAutoFallback,
        host_source: Option<Arc<dyn PermissionAdmissionSource>>,
    ) -> Result<Self, CodingPermissionPolicyError> {
        let host_reviewed = |reviewed: fn(Arc<dyn PermissionAdmissionSource>) -> Self| {
            host_source
                .map(reviewed)
                .ok_or(CodingPermissionPolicyError::HostAdmissionUnavailable { mode })
        };
        match mode {
            CodingPermissionMode::AlwaysApprove => Ok(Self::fully_trusted()),
            CodingPermissionMode::Deny => Ok(Self::deny_all()),
            CodingPermissionMode::Ask => host_reviewed(Self::host_decision_only),
            CodingPermissionMode::Auto => match fallback {
                CodingAutoFallback::Deny => Ok(Self::model_only()),
                CodingAutoFallback::Ask => host_reviewed(Self::model_then_host_fallback),
            },
        }
    }

    pub(super) fn apply_to(&self, mut builder: RuntimeBuilder) -> RuntimeBuilder {
        match self {
            Self::Default => {}
            Self::Required => {
                builder = builder.permission_review_mode(PermissionReviewMode::Required);
            }
            Self::HostDecisionOnly { source } => {
                builder = builder
                    .permission_review_mode(PermissionReviewMode::HostDecisionOnly)
                    .permission_admission_source(Arc::clone(source));
            }
            Self::ModelThenHostFallback { source } => {
                builder = builder
                    .permission_review_mode(PermissionReviewMode::ModelThenHostFallback)
                    .permission_admission_source(Arc::clone(source));
            }
            Self::FullyTrusted => {
                builder = builder.permission_review_mode(PermissionReviewMode::FullyTrusted);
            }
            Self::DenyAll => {
                builder = builder.permission_review_mode(PermissionReviewMode::DenyAll);
            }
        }
        builder
    }
}
