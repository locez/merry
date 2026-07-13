//! Public recursive plan snapshots and lifecycle vocabulary.

use crate::{
    ArtifactRef, ErrorInfo, EvidenceRef, PlanApprovalRequirementId, PlanAttemptId, PlanDirectiveId,
    PlanId, PlanLeaseId, PlanNodeId, SessionId, SessionUsage, SkillId, ToolName,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

const MAX_REVISION_SUMMARY_BYTES: usize = 2 * 1024;

/// Runtime-owned lifecycle phase for one active plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanPhase {
    Planning,
    AwaitingApproval,
    Executing,
    Completed,
    Blocked,
    Cancelled,
}

/// Semantic state persisted for one plan node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanNodeStatus {
    Pending,
    InProgress,
    Expanded,
    Verifying,
    Completed,
    Blocked,
    Failed,
    Superseded,
}

/// Model-authored execution preference for a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanExecutorPolicy {
    Local,
    Delegate,
    Auto,
}

/// Runtime-owned scheduler admission state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanSchedulerStatus {
    Active,
    Paused,
    Draining,
}

/// Terminal outcome of one continuous execution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanAttemptOutcome {
    Completed,
    Decomposed,
    Blocked,
    SemanticFailure,
    TransientFailure,
    Yielded,
    Cancelled,
    Interrupted,
}

/// Runtime-owned lifecycle state of one execution lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanLeaseStatus {
    Live,
    Resolved,
    Cancelled,
    Expired,
}

/// Kind of coordinator steering sent to one live attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanDirectiveKind {
    RequestStatus,
    Steer,
    Converge,
    CheckpointAndContinue,
    CheckpointAndYield,
    CancelAtSafePoint,
}

/// Persisted delivery/application state for a coordinator directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanDirectiveStatus {
    Queued,
    Delivered,
    Acknowledged,
    Applied,
    Superseded,
    Expired,
}

/// Source that activated Plan Mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanActivationSource {
    Coordinator {
        reason: String,
        governing_skill_id: Option<SkillId>,
    },
    User,
}

/// Typed reason that prevents autonomous execution until resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanApprovalRequirementKind {
    UserReviewRequested,
    SkillReviewRequested { skill_id: SkillId },
    RootObjectiveChange,
    RootAcceptanceChange,
    CapabilityOrPermissionExpansion,
    DestructiveExternalAuthority,
    RequiredExternalInput { prompt: String },
}

/// Resolution state for one typed approval requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanApprovalRequirementStatus {
    Pending,
    Resolved,
    Rejected,
}

/// Persisted approval requirement shown to SDK and UI consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanApprovalRequirementSnapshot {
    pub requirement_id: PlanApprovalRequirementId,
    pub kind: PlanApprovalRequirementKind,
    pub status: PlanApprovalRequirementStatus,
    pub created_revision: u64,
    pub resolution_ref: Option<String>,
}

/// Bounded node execution harness projected without provider credentials.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanHarnessSnapshot {
    pub model_role: Option<String>,
    pub reasoning_effort: Option<String>,
    pub checkpoint_turn_interval: Option<u32>,
    pub provider_request_timeout_ms: Option<u64>,
    pub tool_timeout_ms: Option<u64>,
    pub allowed_tools: Vec<ToolName>,
    pub read_scope: Vec<String>,
    pub write_scope: Vec<String>,
    pub forbidden_paths: Vec<String>,
}

/// Retry policy captured on one node revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanRecoveryPolicySnapshot {
    pub max_transient_attempts: u8,
    pub retry_backoff_ms: u64,
    pub retry_only_before_observable_side_effects: bool,
}

impl Default for PlanRecoveryPolicySnapshot {
    fn default() -> Self {
        Self {
            max_transient_attempts: 2,
            retry_backoff_ms: 0,
            retry_only_before_observable_side_effects: true,
        }
    }
}

/// Runtime/operator policy snapshot for scheduling and progress diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanResourcePolicySnapshot {
    pub max_concurrency: usize,
    pub worker_heartbeat_interval_ms: u64,
    pub worker_heartbeat_ttl_ms: u64,
    pub provider_request_timeout_ms: Option<u64>,
    pub tool_timeout_ms: Option<u64>,
    pub checkpoint_turn_interval: u32,
    pub no_durable_progress_review_window_ms: u64,
    pub repeated_failure_limit: u8,
}

impl Default for PlanResourcePolicySnapshot {
    fn default() -> Self {
        Self {
            max_concurrency: 6,
            worker_heartbeat_interval_ms: 10_000,
            worker_heartbeat_ttl_ms: 30_000,
            provider_request_timeout_ms: None,
            tool_timeout_ms: None,
            checkpoint_turn_interval: 8,
            no_durable_progress_review_window_ms: 900_000,
            repeated_failure_limit: 3,
        }
    }
}

/// Authorized provider-invisible capability ceiling for one executing plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanCapabilityEnvelopeSnapshot {
    pub allowed_tools: Vec<ToolName>,
    pub read_scope: Vec<String>,
    pub write_scope: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub destructive_external_authority: bool,
}

/// Compact completion or failure result for one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanNodeResult {
    pub conclusion: String,
    pub evidence_refs: Vec<EvidenceRef>,
    pub artifact_refs: Vec<ArtifactRef>,
    pub changed_paths: Vec<String>,
    pub verification: Vec<String>,
    pub open_questions: Vec<String>,
}

/// Public flat node record that can be rendered recursively by parent/order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanNodeSnapshot {
    pub id: PlanNodeId,
    pub parent_id: Option<PlanNodeId>,
    pub sibling_order: u16,
    pub objective: String,
    pub acceptance: Vec<String>,
    pub status: PlanNodeStatus,
    pub executor_policy: PlanExecutorPolicy,
    pub harness: PlanHarnessSnapshot,
    pub recovery_policy: PlanRecoveryPolicySnapshot,
    pub depends_on: Vec<PlanNodeId>,
    pub result: Option<PlanNodeResult>,
    pub created_revision: u64,
    pub updated_revision: u64,
}

/// Compact persisted view of one execution attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanAttemptSnapshot {
    pub attempt_id: PlanAttemptId,
    pub node_id: PlanNodeId,
    pub node_revision: u64,
    pub lease_id: PlanLeaseId,
    pub executor_session_id: SessionId,
    pub harness_fingerprint: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub outcome: Option<PlanAttemptOutcome>,
    pub result: Option<PlanNodeResult>,
    pub diagnostic: Option<ErrorInfo>,
    pub latest_checkpoint_ref: Option<String>,
    pub last_applied_directive_sequence: u64,
}

/// Compact persisted view of one scheduler lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanLeaseSnapshot {
    pub lease_id: PlanLeaseId,
    pub attempt_id: PlanAttemptId,
    pub node_id: PlanNodeId,
    pub node_revision: u64,
    pub executor_session_id: SessionId,
    pub started_at_ms: u64,
    pub last_heartbeat_at_ms: u64,
    pub lease_expires_at_ms: u64,
    pub status: PlanLeaseStatus,
}

/// Latest bounded liveness and semantic progress for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanAttemptProgressSnapshot {
    pub attempt_id: PlanAttemptId,
    pub node_id: PlanNodeId,
    pub elapsed_ms: u64,
    pub model_turns: u32,
    pub reported_usage: Option<SessionUsage>,
    pub last_worker_heartbeat_at_ms: u64,
    pub last_runtime_activity_at_ms: u64,
    pub last_durable_progress_at_ms: Option<u64>,
    pub provider_request_in_flight: bool,
    pub tool_call_in_flight: bool,
    pub artifacts_created: usize,
    #[serde(default)]
    pub artifact_refs: Vec<ArtifactRef>,
    pub changed_paths: Vec<String>,
    pub acceptance_evidence: Vec<EvidenceRef>,
    pub repeated_failure_fingerprint: Option<String>,
    pub summary: Option<String>,
    pub next_action: Option<String>,
    pub request_coordinator_review: bool,
}

/// Runtime-enforceable part of a coordinator directive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanDirectiveConstraints {
    pub allow_decomposition: bool,
    pub require_terminal_report: bool,
    pub preserve_partial_result: bool,
}

impl Default for PlanDirectiveConstraints {
    fn default() -> Self {
        Self {
            allow_decomposition: true,
            require_terminal_report: true,
            preserve_partial_result: true,
        }
    }
}

/// Persisted attempt-scoped coordinator steering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorDirectiveSnapshot {
    pub directive_id: PlanDirectiveId,
    pub sequence: u64,
    pub plan_id: PlanId,
    pub node_id: PlanNodeId,
    pub node_revision: u64,
    pub attempt_id: PlanAttemptId,
    pub lease_id: PlanLeaseId,
    pub kind: PlanDirectiveKind,
    pub reason: String,
    pub instruction: Option<String>,
    pub constraints: PlanDirectiveConstraints,
    pub requested_output: Vec<String>,
    pub issued_at_ms: u64,
    pub status: PlanDirectiveStatus,
    pub delivered_at_ms: Option<u64>,
    pub acknowledged_at_ms: Option<u64>,
    pub applied_at_ms: Option<u64>,
}

/// Bounded explanation of one committed plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanRevisionSummary {
    revision: u64,
    summary: String,
}

impl PlanRevisionSummary {
    pub fn new(revision: u64, summary: &str) -> Result<Self, crate::CoreError> {
        validate_revision_summary(summary)?;
        Ok(Self {
            revision,
            summary: summary.to_owned(),
        })
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanRevisionSummaryWire {
    revision: u64,
    summary: String,
}

impl<'de> Deserialize<'de> for PlanRevisionSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PlanRevisionSummaryWire::deserialize(deserializer)?;
        Self::new(wire.revision, &wire.summary).map_err(de::Error::custom)
    }
}

/// Complete bounded state projection for one active or terminal plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanSnapshot {
    pub plan_id: PlanId,
    pub revision: u64,
    pub phase: PlanPhase,
    pub activation_source: PlanActivationSource,
    pub root_node_id: Option<PlanNodeId>,
    pub coordinator_node_id: Option<PlanNodeId>,
    pub execution_contract_fingerprint: Option<String>,
    pub execution_authorization_refs: Vec<String>,
    pub authorized_capability_envelope: Option<PlanCapabilityEnvelopeSnapshot>,
    pub approval_requirements: Vec<PlanApprovalRequirementSnapshot>,
    pub nodes: Vec<PlanNodeSnapshot>,
    pub attempts: Vec<PlanAttemptSnapshot>,
    pub leases: Vec<PlanLeaseSnapshot>,
    pub attempt_progress: Vec<PlanAttemptProgressSnapshot>,
    pub directives: Vec<CoordinatorDirectiveSnapshot>,
    pub resource_policy_snapshot: PlanResourcePolicySnapshot,
    pub max_concurrency_hint: Option<usize>,
    pub scheduler_status: PlanSchedulerStatus,
    pub revision_summaries: Vec<PlanRevisionSummary>,
}

fn validate_revision_summary(summary: &str) -> Result<(), crate::CoreError> {
    if summary.trim().is_empty() {
        return Err(invalid_summary(summary, "must not be blank"));
    }
    if summary.len() > MAX_REVISION_SUMMARY_BYTES {
        return Err(invalid_summary(
            summary,
            "is longer than the allowed maximum",
        ));
    }
    if summary.chars().any(char::is_control) {
        return Err(invalid_summary(
            summary,
            "must not contain control characters",
        ));
    }
    Ok(())
}

fn invalid_summary(value: &str, reason: &'static str) -> crate::CoreError {
    crate::CoreError::InvalidIdentifier {
        kind: "PlanRevisionSummary",
        value: value.to_owned(),
        reason,
    }
}
