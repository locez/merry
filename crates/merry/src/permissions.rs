//! Runtime permission admission, human review channels, and path-access rules.

pub use merry_runtime::{
    ChannelPermissionAdmissionSource, HostFallbackReason, HostIntegration, PathAccess,
    PathAccessRule, PathAccessRuleSource, PermissionActor, PermissionAdmissionContext,
    PermissionAdmissionDecision, PermissionAdmissionFuture, PermissionAdmissionSource,
    PermissionRequest, PermissionReviewMode, PermissionReviewRequest, PermissionReviewResponse,
    PermissionReviewRisk, PermissionReviewRules, PermissionUserAuthorization, PermissionedAction,
    RequestedCapability, parse_permission_request, request_permissions_tool,
};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Bounded review channel pairing a runtime-facing permission admission source
/// with a host-facing receiver of [`PermissionReviewRequest`] items.
pub struct PermissionReviewChannel {
    source: Arc<ChannelPermissionAdmissionSource>,
    requests: mpsc::Receiver<PermissionReviewRequest>,
}

impl PermissionReviewChannel {
    /// Creates a bounded permission review channel with `capacity` buffered
    /// requests.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (source, requests) = ChannelPermissionAdmissionSource::channel(capacity);
        Self {
            source: Arc::new(source),
            requests,
        }
    }

    /// Returns the admission source to install into a coding permission policy.
    #[must_use]
    pub fn source(&self) -> Arc<dyn PermissionAdmissionSource> {
        Arc::clone(&self.source) as Arc<dyn PermissionAdmissionSource>
    }

    /// Splits the channel into its admission source and request receiver.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Arc<ChannelPermissionAdmissionSource>,
        mpsc::Receiver<PermissionReviewRequest>,
    ) {
        (self.source, self.requests)
    }
}
