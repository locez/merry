//! Approval policy selection and its mapping onto runtime permission review.
//!
//! The product surface names a reviewer through [`CodingApprovalPolicy`]; this
//! module turns that choice into the [`CodingPermissionPolicy`] both parent and
//! child coding runtimes apply, and owns the typed error for policies that need
//! a host admission source the surface did not supply.

use merry_runtime::{PermissionAdmissionSource, PermissionReviewMode, RuntimeBuilder};
use std::sync::Arc;
use thiserror::Error;

/// Who reviews permission requests, as selected by the product surface.
///
/// This is the coding-layer form of the user-facing `approval_policy`
/// setting; each variant names the reviewer rather than a runtime mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CodingApprovalPolicy {
    /// No approval round: configured actions run without model or host review.
    NoApproval,
    /// Reject every permission request without consulting a reviewer.
    Deny,
    /// The approval-review model decides; no human fallback.
    ModelOnly,
    /// The model reviews first; the host decides when the model denies or
    /// cannot decide.
    #[default]
    ModelThenHuman,
    /// The host admission source (the person at the terminal) decides.
    HumanOnly,
}

/// Failure to construct a permission policy for an approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CodingPermissionPolicyError {
    /// A selected policy needs a host admission source, but the surface did not provide one.
    #[error("{policy:?} permission review requires a host admission source")]
    HostAdmissionUnavailable { policy: CodingApprovalPolicy },
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
    ///
    /// The variant name is retained for public API compatibility; use
    /// [`CodingPermissionPolicy::model_only`] in new code.
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

    /// Route permission requests through model-backed review without a host fallback.
    #[must_use]
    pub const fn required() -> Self {
        Self::model_only()
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

    /// Selects the product policy for one approval policy.
    ///
    /// A host-reviewed policy is only constructed when the caller supplies a
    /// host admission source. Callers that need host review must handle the
    /// typed error instead of silently degrading to another policy.
    pub fn for_approval_policy(
        policy: CodingApprovalPolicy,
        host_source: Option<Arc<dyn PermissionAdmissionSource>>,
    ) -> Result<Self, CodingPermissionPolicyError> {
        let host_reviewed = |reviewed: fn(Arc<dyn PermissionAdmissionSource>) -> Self| {
            host_source
                .map(reviewed)
                .ok_or(CodingPermissionPolicyError::HostAdmissionUnavailable { policy })
        };
        match policy {
            CodingApprovalPolicy::NoApproval => Ok(Self::fully_trusted()),
            CodingApprovalPolicy::Deny => Ok(Self::deny_all()),
            CodingApprovalPolicy::ModelOnly => Ok(Self::model_only()),
            CodingApprovalPolicy::ModelThenHuman => host_reviewed(Self::model_then_host_fallback),
            CodingApprovalPolicy::HumanOnly => host_reviewed(Self::host_decision_only),
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
