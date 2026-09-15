//! Host-facing permission review transport.
//!
//! A host (TUI, headless driver, or embedding app) receives one typed
//! [`PermissionReviewRequest`] per pending review and answers with one
//! [`PermissionReviewResponse`]. The runtime validates the answer identity
//! before it can grant anything, so a stale UI response cannot authorize a
//! later call. Nothing here decides policy: this module only moves a request to
//! the host and returns the host's answer.

use super::{
    HostFallbackReason, PermissionAdmissionContext, PermissionAdmissionDecision,
    PermissionAdmissionError, PermissionAdmissionFuture, PermissionAdmissionSource,
    PermissionRequest, validate_optional_reason,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// A pending host-facing permission review request.
pub struct PermissionReviewRequest {
    request: PermissionRequest,
    host_fallback_reason: Option<HostFallbackReason>,
    cancellation_token: CancellationToken,
    response_sender: Option<oneshot::Sender<PermissionReviewResponse>>,
}

impl std::fmt::Debug for PermissionReviewRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PermissionReviewRequest")
            .field("approval_id", &self.approval_id())
            .field("fingerprint", &self.fingerprint())
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl PermissionReviewRequest {
    /// Returns the request that must be shown to the host.
    #[must_use]
    pub fn request(&self) -> &PermissionRequest {
        &self.request
    }

    /// Returns the stable response correlation id.
    #[must_use]
    pub fn approval_id(&self) -> String {
        self.request.approval_id()
    }

    /// Returns the exact request fingerprint required in a response.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        self.request.fingerprint()
    }

    /// Returns why AI review handed this request to the host, when it did.
    #[must_use]
    pub const fn host_fallback_reason(&self) -> Option<&HostFallbackReason> {
        self.host_fallback_reason.as_ref()
    }

    /// Returns whether the runtime cancelled this pending review.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token.is_cancelled()
    }

    /// Resolves this request as an approval.
    pub fn approve(
        self,
        rationale: impl Into<String>,
    ) -> Result<(), PermissionReviewResponseError> {
        let approval_id = self.approval_id();
        let fingerprint = self.fingerprint();
        self.respond(PermissionReviewResponse::allow(
            approval_id,
            fingerprint,
            rationale,
        ))
    }

    /// Resolves this request as a denial.
    pub fn deny(self, rationale: impl Into<String>) -> Result<(), PermissionReviewResponseError> {
        let approval_id = self.approval_id();
        let fingerprint = self.fingerprint();
        self.respond(PermissionReviewResponse::deny(
            approval_id,
            fingerprint,
            rationale,
        ))
    }

    /// Sends a response, including its caller-supplied correlation fields.
    ///
    /// The runtime validates both fields before it can grant the request, so a
    /// stale UI response cannot authorize a later call.
    pub fn respond(
        mut self,
        response: PermissionReviewResponse,
    ) -> Result<(), PermissionReviewResponseError> {
        let Some(sender) = self.response_sender.take() else {
            return Err(PermissionReviewResponseError::AlreadyResolved);
        };
        sender
            .send(response)
            .map_err(|_| PermissionReviewResponseError::Closed)
    }
}

/// Host response for one pending permission review request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionReviewResponse {
    approval_id: String,
    fingerprint: String,
    decision: PermissionReviewResponseDecision,
    rationale: String,
}

impl PermissionReviewResponse {
    /// Creates an approval response for an exact request.
    #[must_use]
    pub fn allow(
        approval_id: impl Into<String>,
        fingerprint: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            approval_id: approval_id.into(),
            fingerprint: fingerprint.into(),
            decision: PermissionReviewResponseDecision::Allow,
            rationale: rationale.into(),
        }
    }

    /// Creates a denial response for an exact request.
    #[must_use]
    pub fn deny(
        approval_id: impl Into<String>,
        fingerprint: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            approval_id: approval_id.into(),
            fingerprint: fingerprint.into(),
            decision: PermissionReviewResponseDecision::Deny,
            rationale: rationale.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionReviewResponseDecision {
    Allow,
    Deny,
}

/// Channel-backed human fallback source.
#[derive(Debug, Clone)]
pub struct ChannelPermissionAdmissionSource {
    sender: mpsc::Sender<PermissionReviewRequest>,
}

impl ChannelPermissionAdmissionSource {
    /// Creates a source and its host-facing pending-review receiver.
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<PermissionReviewRequest>) {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        (Self { sender }, receiver)
    }
}

impl PermissionAdmissionSource for ChannelPermissionAdmissionSource {
    fn review<'a>(
        &'a self,
        request: PermissionRequest,
        context: PermissionAdmissionContext,
    ) -> PermissionAdmissionFuture<'a> {
        Box::pin(async move {
            let approval_id = request.approval_id();
            let fingerprint = request.fingerprint();
            let (response_sender, response_receiver) = oneshot::channel();
            let pending = PermissionReviewRequest {
                request: request.clone(),
                host_fallback_reason: context.host_fallback_reason().cloned(),
                cancellation_token: context.cancellation_token().clone(),
                response_sender: Some(response_sender),
            };
            tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => {
                    Err(PermissionAdmissionError::Cancelled)
                }
                result = self.sender.send(pending) => {
                    result.map_err(|_| PermissionAdmissionError::HumanReviewUnavailable {
                        message: "human permission review channel is closed".to_owned(),
                    })?;
                    let response = tokio::select! {
                        biased;
                        () = context.cancellation_token().cancelled() => {
                            return Err(PermissionAdmissionError::Cancelled);
                        }
                        response = response_receiver => response.map_err(|_| {
                            PermissionAdmissionError::HumanReviewUnavailable {
                                message: "human permission review response was closed".to_owned(),
                            }
                        })?,
                    };
                    if response.approval_id != approval_id {
                        return Err(PermissionAdmissionError::StaleReviewResponse {
                            expected: approval_id,
                            actual: response.approval_id,
                        });
                    }
                    if response.fingerprint != fingerprint {
                        return Err(PermissionAdmissionError::StaleReviewResponse {
                            expected: fingerprint,
                            actual: response.fingerprint,
                        });
                    }
                    validate_optional_reason(&response.rationale)?;
                    let decision = match response.decision {
                        PermissionReviewResponseDecision::Allow => {
                            PermissionAdmissionDecision::approved(response.rationale)
                        }
                        PermissionReviewResponseDecision::Deny => {
                            PermissionAdmissionDecision::denied(response.rationale)
                        }
                    };
                    Ok(decision)
                }
            }
        })
    }
}

/// Error returned when a host responds to a pending review request.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PermissionReviewResponseError {
    /// The pending runtime request was already resolved or cancelled.
    #[error("permission review request is already resolved")]
    AlreadyResolved,
    /// The runtime no longer accepts a response for this request.
    #[error("permission review response channel is closed")]
    Closed,
}
