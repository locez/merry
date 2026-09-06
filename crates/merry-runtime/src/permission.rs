//! Runtime-owned permission request and approval review primitives.
//!
//! Permission requests wrap an exact planned action and its minimum capabilities.
//! A process may declare known capabilities before execution; a failed sandboxed
//! action may instead request capabilities discovered from its durable result.
//! Runtime owns admission and executes the exact process action after approval.

use crate::{PathAccess, ProcessActionIntent};
use merry_core::{CoreError, PendingToolCall, ToolName};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const REQUEST_PERMISSIONS_TOOL_NAME: &str = "request_permissions";
const MAX_PERMISSION_REASON_BYTES: usize = 2048;
const DEFAULT_PERMISSION_STDOUT_LIMIT_BYTES: usize = 64 * 1024;
const DEFAULT_PERMISSION_STDERR_LIMIT_BYTES: usize = 64 * 1024;
const PERMISSION_REVIEW_MAX_OUTPUT_TOKENS: u64 = 512;

mod input;
mod payload;
mod review;

pub(crate) use input::{
    RequestedCapabilitiesInput, is_request_permissions_tool, normalize_requested_capabilities,
    normalize_requested_path, permission_reason_schema_for_schemars, permission_request_from_call,
    permission_request_from_process_call, process_cwd_schema_for_schemars,
    requested_capabilities_schema_for_schemars, validate_optional_reason,
};
pub use input::{parse_permission_request, request_permissions_tool};
pub(crate) use payload::{
    permission_blocked_outcome, permission_denied_outcome, permission_invalid_arguments_outcome,
    permission_review_error_outcome,
};
pub(crate) use review::{
    ModelBackedPermissionAdmissionSource, permission_request_fingerprint_json,
};

/// How much runtime should trust the host/runtime owner for permission gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTrustLevel {
    /// Agentic/coding-agent runtime. Permissioned actions require review.
    Agent,
    /// Trusted SDK or host app runtime. The host may explicitly disable review.
    TrustedSdk,
}

/// Review mode for explicit permission requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionReviewMode {
    /// Runtime chooses the conservative default for the trust level.
    DefaultForTrust,
    /// Always route permission requests through a review source.
    Required,
    /// Allow an injected non-model admission source to decide without model review.
    HostDecisionOnly,
    /// Try AI review first, then wait on an injected host source only when the
    /// AI review cannot produce a decision.
    ModelThenHostFallback,
    /// Explicit SDK/host mode that admits configured registered tools without
    /// an AI or human approval round.
    FullyTrusted,
}

impl PermissionReviewMode {
    /// Returns whether the caller explicitly disabled permission review.
    pub(crate) const fn is_fully_trusted(self) -> bool {
        matches!(self, Self::FullyTrusted)
    }

    pub(crate) fn requires_model_review(self, trust_level: RuntimeTrustLevel) -> bool {
        match self {
            Self::DefaultForTrust => trust_level == RuntimeTrustLevel::Agent,
            Self::Required => true,
            Self::HostDecisionOnly => false,
            Self::ModelThenHostFallback => true,
            Self::FullyTrusted => false,
        }
    }
}

/// Extra capability requested for a permissioned action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedCapability {
    /// Allow network access in the selected permissioned backend/profile.
    Network,
    /// Allow filesystem access to one path in the selected backend/profile.
    Path(RequestedPathCapability),
    /// Allow one explicitly configured host integration in the selected
    /// permissioned backend/profile.
    HostIntegration(HostIntegration),
}

/// Host-provided IPC integration that may be exposed to inner process actions.
/// The outer sandbox remains the capability ceiling; an enabled integration can
/// be forwarded to the inner action sandbox, while an explicit request can add
/// one for a permissioned action when the backend supports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostIntegration {
    /// The user's SSH authentication agent socket.
    SshAgent,
    /// The user's D-Bus session bus socket, commonly used by keyring clients.
    SessionBus,
}

impl HostIntegration {
    /// Returns the stable model/configuration name for this integration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SshAgent => "ssh-agent",
            Self::SessionBus => "dbus",
        }
    }
}

/// Requested filesystem capability for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedPathCapability {
    path: String,
    access: PathAccess,
}

impl RequestedPathCapability {
    pub fn new(path: String, access: PathAccess) -> Result<Self, PermissionAdmissionError> {
        let path = normalize_requested_path(&path)?;
        Ok(Self { path, access })
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn access(&self) -> PathAccess {
        self.access
    }
}

/// Exact action covered by a permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionedAction {
    /// Execute one validated process intent.
    Process(ProcessActionIntent),
}

impl PermissionedAction {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Process(_) => "process",
        }
    }

    #[must_use]
    pub fn summary(&self) -> &str {
        match self {
            Self::Process(intent) => intent.summary(),
        }
    }
}

/// Runtime-owned normalized permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    tool_call_id: merry_core::ToolCallId,
    tool_name: ToolName,
    reason: Option<String>,
    requested: Vec<RequestedCapability>,
    action: PermissionedAction,
    review_context: Vec<PermissionReviewContextEntry>,
    review_only: bool,
}

impl PermissionRequest {
    pub(crate) fn new(
        call: &PendingToolCall,
        reason: Option<String>,
        requested: Vec<RequestedCapability>,
        action: PermissionedAction,
        review_context: Vec<PermissionReviewContextEntry>,
    ) -> Result<Self, PermissionAdmissionError> {
        if requested.is_empty() {
            return Err(PermissionAdmissionError::InvalidArguments {
                message: "request_permissions requires at least one requested capability"
                    .to_owned(),
            });
        }
        if let Some(reason) = reason.as_deref() {
            validate_optional_reason(reason)?;
        }

        let requested = normalize_requested_capabilities(requested)?;

        Ok(Self {
            tool_call_id: call.id().clone(),
            tool_name: call.name().clone(),
            reason,
            requested,
            action,
            review_context,
            review_only: false,
        })
    }

    /// Creates a review-only request for a high-risk action.
    ///
    /// Unlike a capability request, this does not grant or retain any
    /// capability. It reuses the same admission source to review the exact
    /// action before a separately configured runner executes it.
    pub(crate) fn for_action_review(
        call: &PendingToolCall,
        reason: impl Into<String>,
        action: PermissionedAction,
        review_context: Vec<PermissionReviewContextEntry>,
    ) -> Self {
        Self {
            tool_call_id: call.id().clone(),
            tool_name: call.name().clone(),
            reason: Some(reason.into()),
            requested: Vec::new(),
            action,
            review_context,
            review_only: true,
        }
    }

    #[must_use]
    pub fn tool_call_id(&self) -> &merry_core::ToolCallId {
        &self.tool_call_id
    }

    #[must_use]
    pub fn tool_name(&self) -> &ToolName {
        &self.tool_name
    }

    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    #[must_use]
    pub fn requested(&self) -> &[RequestedCapability] {
        &self.requested
    }

    /// Returns whether this request asks for network capability.
    #[must_use]
    pub fn requests_network(&self) -> bool {
        self.requested
            .iter()
            .any(|capability| matches!(capability, RequestedCapability::Network))
    }

    /// Returns whether this request reviews an action without granting a
    /// capability.
    #[must_use]
    pub fn is_action_review(&self) -> bool {
        self.review_only
    }

    #[must_use]
    pub fn action(&self) -> &PermissionedAction {
        &self.action
    }

    #[must_use]
    pub(crate) fn review_context(&self) -> &[PermissionReviewContextEntry] {
        &self.review_context
    }

    /// Returns the stable fingerprint for the exact action and capability set.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        crate::process::stable_process_input_fingerprint(
            permission_request_fingerprint_json(self)
                .to_string()
                .as_bytes(),
        )
    }

    /// Returns the stable identifier used to correlate a host response.
    #[must_use]
    pub fn approval_id(&self) -> String {
        format!("{}:{}", self.tool_call_id(), self.fingerprint())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionReviewContextEntry {
    role: &'static str,
    text: String,
}

impl PermissionReviewContextEntry {
    pub(crate) fn new(role: &'static str, text: String) -> Self {
        Self { role, text }
    }

    fn role(&self) -> &'static str {
        self.role
    }

    fn text(&self) -> &str {
        &self.text
    }
}

/// Result returned by a permission admission gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAdmissionDecision {
    /// The requested exact action may run through the configured permissioned runner.
    Approved(PermissionAdmissionReview),
    /// The requested action must not run.
    Denied(PermissionAdmissionReview),
}

impl PermissionAdmissionDecision {
    #[must_use]
    pub fn approved(rationale: impl Into<String>) -> Self {
        Self::Approved(PermissionAdmissionReview::new(
            PermissionAdmissionReviewSource::Host,
            PermissionReviewRisk::Unknown,
            PermissionUserAuthorization::Unknown,
            rationale,
        ))
    }

    pub(crate) fn approved_existing_grant() -> Self {
        Self::Approved(PermissionAdmissionReview::new(
            PermissionAdmissionReviewSource::ExistingGrant,
            PermissionReviewRisk::Low,
            PermissionUserAuthorization::High,
            "requested capabilities are already authorized by the current process session",
        ))
    }

    #[must_use]
    pub fn denied(rationale: impl Into<String>) -> Self {
        Self::Denied(PermissionAdmissionReview::new(
            PermissionAdmissionReviewSource::Host,
            PermissionReviewRisk::Unknown,
            PermissionUserAuthorization::Unknown,
            rationale,
        ))
    }

    pub(crate) fn is_approved(&self) -> bool {
        matches!(self, Self::Approved(_))
    }

    pub(crate) fn review(&self) -> &PermissionAdmissionReview {
        match self {
            Self::Approved(review) | Self::Denied(review) => review,
        }
    }
}

/// Metadata for one admission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionAdmissionReview {
    source: PermissionAdmissionReviewSource,
    risk: PermissionReviewRisk,
    user_authorization: PermissionUserAuthorization,
    rationale: String,
}

impl PermissionAdmissionReview {
    fn new(
        source: PermissionAdmissionReviewSource,
        risk: PermissionReviewRisk,
        user_authorization: PermissionUserAuthorization,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            source,
            risk,
            user_authorization,
            rationale: rationale.into(),
        }
    }

    #[must_use]
    pub fn rationale(&self) -> &str {
        &self.rationale
    }

    /// Returns the source that produced this review metadata.
    #[must_use]
    pub const fn source(&self) -> PermissionAdmissionReviewSource {
        self.source
    }

    /// Returns the reviewer's risk assessment.
    #[must_use]
    pub const fn risk(&self) -> PermissionReviewRisk {
        self.risk
    }

    /// Returns the reviewer's user-authorization assessment.
    #[must_use]
    pub const fn user_authorization(&self) -> PermissionUserAuthorization {
        self.user_authorization
    }

    fn can_auto_approve(&self) -> bool {
        matches!(
            self.risk,
            PermissionReviewRisk::Low | PermissionReviewRisk::Medium
        ) && matches!(
            self.user_authorization,
            PermissionUserAuthorization::Medium | PermissionUserAuthorization::High
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAdmissionReviewSource {
    Host,
    Model,
    ExistingGrant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionReviewRisk {
    Low,
    Medium,
    High,
    Critical,
    Unknown,
}

impl PermissionReviewRisk {
    fn from_model(value: &str) -> Result<Self, PermissionAdmissionError> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            "unknown" => Ok(Self::Unknown),
            actual => Err(PermissionAdmissionError::InvalidReviewOutput {
                message: format!("risk must be low|medium|high|critical|unknown, got {actual:?}"),
            }),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionUserAuthorization {
    Unknown,
    Low,
    Medium,
    High,
}

impl PermissionUserAuthorization {
    fn from_model(value: &str) -> Result<Self, PermissionAdmissionError> {
        match value {
            "unknown" => Ok(Self::Unknown),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            actual => Err(PermissionAdmissionError::InvalidReviewOutput {
                message: format!(
                    "user_authorization must be unknown|low|medium|high, got {actual:?}"
                ),
            }),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Boxed admission review future.
pub type PermissionAdmissionFuture<'a> =
    Pin<Box<dyn Future<Output = PermissionAdmissionResult> + Send + 'a>>;

/// Admission gate result.
pub type PermissionAdmissionResult = Result<PermissionAdmissionDecision, PermissionAdmissionError>;

/// Object-safe permission admission boundary.
pub trait PermissionAdmissionSource: Send + Sync {
    fn review<'a>(
        &'a self,
        request: PermissionRequest,
        context: PermissionAdmissionContext,
    ) -> PermissionAdmissionFuture<'a>;
}

/// Cancellation-aware permission admission context.
#[derive(Debug, Clone)]
pub struct PermissionAdmissionContext {
    cancellation_token: CancellationToken,
    review_failure: Option<String>,
}

/// A pending host-facing permission review request.
pub struct PermissionReviewRequest {
    request: PermissionRequest,
    review_failure: Option<String>,
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

    /// Returns the AI review failure that caused the host fallback.
    #[must_use]
    pub fn review_failure(&self) -> Option<&str> {
        self.review_failure.as_deref()
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
                review_failure: context.review_failure().map(str::to_owned),
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

impl PermissionAdmissionContext {
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self {
            cancellation_token,
            review_failure: None,
        }
    }

    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Adds the structured reason that caused an optional host fallback.
    #[must_use]
    pub fn with_review_failure(mut self, failure: impl Into<String>) -> Self {
        self.review_failure = Some(failure.into());
        self
    }

    /// Returns the AI review failure that preceded this fallback, if any.
    #[must_use]
    pub fn review_failure(&self) -> Option<&str> {
        self.review_failure.as_deref()
    }
}

/// Errors raised by permission request parsing or admission.
#[derive(Debug, Error)]
pub enum PermissionAdmissionError {
    /// Tool arguments were invalid and should be returned to the model.
    #[error("invalid permission request arguments: {message}")]
    InvalidArguments { message: String },
    /// No reviewer/model was configured for a required permission gate.
    #[error("permission review is required but no review model is configured")]
    ReviewModelUnavailable,
    /// Model-backed review failed before producing a decision.
    #[error("permission review failed: {message}")]
    ReviewFailed { message: String },
    /// Model-backed review returned unsupported output.
    #[error("permission review output is invalid: {message}")]
    InvalidReviewOutput { message: String },
    /// The optional human fallback could not accept or await a response.
    #[error("human permission review is unavailable: {message}")]
    HumanReviewUnavailable { message: String },
    /// A response did not match the currently pending request identity.
    #[error("stale permission review response: expected {expected}, got {actual}")]
    StaleReviewResponse { expected: String, actual: String },
    /// Permission admission observed cooperative cancellation.
    #[error("permission admission cancelled")]
    Cancelled,
    /// Core protocol value rejected the tool definition.
    #[error(transparent)]
    Core {
        /// Source core validation error.
        #[from]
        source: CoreError,
    },
}

#[cfg(test)]
mod tests;
