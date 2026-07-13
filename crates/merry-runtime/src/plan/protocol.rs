use merry_core::{
    ArtifactRef, ErrorInfo, EvidenceRef, PlanApprovalRequirementSnapshot, PlanAttemptId,
    PlanAttemptOutcome, PlanDirectiveConstraints, PlanDirectiveId, PlanDirectiveKind,
    PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanLeaseId, PlanNodeId, PlanNodeResult,
    PlanPhase, PlanRecoveryPolicySnapshot, PlanSchedulerStatus, PlanSnapshot, SkillId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Coordinator request to activate Plan Mode without changing general permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BeginPlanInput {
    pub reason: String,
    pub governing_skill_id: Option<SkillId>,
}

/// Compact activation result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BeginPlanOutput {
    pub plan_id: merry_core::PlanId,
    pub phase: PlanPhase,
    pub revision: u64,
}

/// Bounded exact-read selector for the active plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadPlanInput {
    pub plan_id: Option<merry_core::PlanId>,
    pub node_id: Option<PlanNodeId>,
    pub max_depth: Option<u8>,
    pub include_attempts: Option<bool>,
    pub include_progress: Option<bool>,
    pub include_directives: Option<bool>,
    pub cursor: Option<String>,
}

/// Provider-visible steering request for one live attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlPlanAttemptInput {
    pub attempt_id: PlanAttemptId,
    pub expected_lease_id: PlanLeaseId,
    pub expected_node_revision: u64,
    pub kind: PlanDirectiveKind,
    pub reason: String,
    pub instruction: Option<String>,
    pub constraints: Option<PlanDirectiveConstraints>,
    pub requested_output: Vec<String>,
}

/// Non-terminal semantic progress report for one live lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportPlanProgressInput {
    pub lease_id: PlanLeaseId,
    pub expected_node_revision: u64,
    pub summary: String,
    pub evidence_refs: Vec<EvidenceRef>,
    pub artifact_refs: Vec<ArtifactRef>,
    pub next_action: Option<String>,
    pub checkpoint_ref: Option<String>,
    pub acknowledged_directive_ids: Vec<PlanDirectiveId>,
    pub applied_directive_ids: Vec<PlanDirectiveId>,
    pub request_coordinator_review: Option<bool>,
}

/// Direct-child lazy decomposition reported by one worker attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanDecompositionInput {
    pub reason: String,
    pub children: Vec<PlanNodeInput>,
}

/// Exactly-once terminal report for one live lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportPlanAttemptInput {
    pub lease_id: PlanLeaseId,
    pub expected_node_revision: u64,
    pub outcome: PlanAttemptOutcome,
    pub result: Option<PlanNodeResult>,
    pub diagnostic: Option<ErrorInfo>,
    pub decomposition: Option<PlanDecompositionInput>,
    pub acknowledged_directive_ids: Vec<PlanDirectiveId>,
    pub applied_directive_ids: Vec<PlanDirectiveId>,
}

/// Requested phase behavior after a successful plan update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanExecutionIntent {
    ContinuePlanning,
    ExecuteIfAuthorized,
    RequestUserReview,
}

/// Reference to an existing runtime id or a request-local client key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanNodeReferenceInput {
    Id { id: PlanNodeId },
    ClientKey { client_key: String },
}

/// Provider-visible authored node input. Runtime-owned state is intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanNodeInput {
    pub id: Option<PlanNodeId>,
    pub client_key: Option<String>,
    pub objective: String,
    pub acceptance: Vec<String>,
    pub executor_policy: PlanExecutorPolicy,
    pub harness: PlanHarnessSnapshot,
    pub recovery_policy: PlanRecoveryPolicySnapshot,
    pub depends_on: Vec<PlanNodeReferenceInput>,
    pub children: Vec<PlanNodeInput>,
}

/// Stable tagged change shape for complete planning and execution-time revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanChangeInput {
    DefinePlan {
        expected_plan_revision: u64,
        root: PlanNodeInput,
    },
    ReplaceSubtree {
        target_node_id: PlanNodeId,
        expected_node_revision: u64,
        subtree: PlanNodeInput,
    },
}

/// Coordinator-authored plan update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlanInput {
    pub reason: String,
    pub execution_intent: PlanExecutionIntent,
    pub coordinator_node_id: Option<PlanNodeId>,
    pub max_concurrency_hint: Option<usize>,
    pub change: PlanChangeInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlanUpdateOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) client_key_ids: BTreeMap<String, PlanNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlanUpdateToolOutput {
    pub(crate) plan_id: PlanId,
    pub(crate) revision: u64,
    pub(crate) phase: PlanPhase,
    pub(crate) client_key_ids: BTreeMap<String, PlanNodeId>,
    pub(crate) scheduler_status: PlanSchedulerStatus,
    pub(crate) approval_requirements: Vec<PlanApprovalRequirementSnapshot>,
}

impl From<&PlanUpdateOutput> for PlanUpdateToolOutput {
    fn from(output: &PlanUpdateOutput) -> Self {
        Self {
            plan_id: output.snapshot.plan_id.clone(),
            revision: output.snapshot.revision,
            phase: output.snapshot.phase,
            client_key_ids: output.client_key_ids.clone(),
            scheduler_status: output.snapshot.scheduler_status,
            approval_requirements: output.snapshot.approval_requirements.clone(),
        }
    }
}
