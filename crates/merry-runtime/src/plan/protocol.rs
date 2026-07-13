use merry_core::{
    ArtifactRef, ErrorInfo, EvidenceRef, PlanApprovalRequirementId,
    PlanApprovalRequirementSnapshot, PlanAttemptId, PlanAttemptOutcome,
    PlanCapabilityEnvelopeSnapshot, PlanDirectiveConstraints, PlanDirectiveId, PlanDirectiveKind,
    PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanNodeId, PlanNodeResult, PlanPhase,
    PlanRecoveryPolicySnapshot, PlanSchedulerStatus, PlanSnapshot, SkillId,
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

/// Runtime-owned approval material supplied at an interactive boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanApprovalInput {
    pub plan_id: PlanId,
    pub expected_plan_revision: u64,
    pub review_resolution_ref: String,
    pub capability_envelope: Option<PlanCapabilityEnvelopeSnapshot>,
    pub authorization_refs: Vec<String>,
    pub requirement_resolution_refs: BTreeMap<PlanApprovalRequirementId, String>,
}

/// Bounded exact-read selector for the active plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadPlanInput {
    pub plan_id: Option<merry_core::PlanId>,
    pub node_id: Option<PlanNodeId>,
    pub max_depth: Option<u8>,
    pub include_attempts: Option<bool>,
    /// Include subagent lease records for the selected attempt page. This
    /// requires `include_attempts=true`; attempts include leases by default.
    pub include_leases: Option<bool>,
    pub include_progress: Option<bool>,
    pub include_directives: Option<bool>,
    pub cursor: Option<String>,
}

/// Provider-visible steering request for one live attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlPlanAttemptInput {
    pub attempt_id: PlanAttemptId,
    pub kind: PlanDirectiveKind,
    pub reason: String,
    pub instruction: Option<String>,
    pub constraints: Option<PlanDirectiveConstraints>,
    pub requested_output: Vec<String>,
}

/// Non-terminal semantic progress for the attempt bound to this runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportPlanProgressInput {
    pub summary: String,
    pub evidence_refs: Vec<EvidenceRef>,
    pub artifact_refs: Vec<ArtifactRef>,
    pub next_action: Option<String>,
    pub checkpoint_ref: Option<String>,
    pub acknowledged_directive_ids: Vec<PlanDirectiveId>,
    pub applied_directive_ids: Vec<PlanDirectiveId>,
    pub request_coordinator_review: Option<bool>,
}

/// Direct-child lazy decomposition reported by one subagent attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanDecompositionInput {
    pub reason: String,
    pub children: Vec<PlanNodeInput>,
}

/// Exactly-once semantic terminal report for the attempt bound to this runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportPlanAttemptInput {
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
#[schemars(
    description = "Choose continue_planning while the tree is not ready. Choose execute_if_authorized when the user already requested execution, including requests to use the plan and proceed; this establishes the initial plan capability baseline but does not bypass per-tool permission checks. Choose request_user_review only when the user asked to inspect or approve the plan before work starts, or when an explicit review boundary is required."
)]
pub enum PlanExecutionIntent {
    /// Keep the plan in its authoring phase because more refinement is needed.
    ContinuePlanning,
    /// Start work when the current request already authorizes execution and no
    /// typed review boundary requires another user decision.
    ExecuteIfAuthorized,
    /// Stop at the interactive Plan approval UI before starting work.
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
#[schemars(
    description = "One authored plan node. New nodes use client_key and omit id. Existing mutable nodes use id and omit client_key. Node status, attempts, leases, progress, results, parent ids, and sibling order are runtime-owned and cannot be authored here.",
    extend("oneOf" = [
        {
            "title": "New node",
            "required": ["client_key"],
            "properties": {
                "id": { "type": "null" },
                "client_key": { "type": "string", "minLength": 1, "maxLength": 128 }
            }
        },
        {
            "title": "Existing mutable node",
            "required": ["id"],
            "properties": {
                "id": { "type": "string" },
                "client_key": { "type": "null" }
            }
        }
    ])
)]
pub struct PlanNodeInput {
    /// Runtime-owned id from a prior plan result or read. Set this only when
    /// retaining an existing mutable node; otherwise omit it or use null.
    pub id: Option<PlanNodeId>,
    /// Unique request-local key for a new node. Set this for every new node and
    /// omit `id`; the update result maps this key to its runtime-owned id.
    pub client_key: Option<String>,
    /// Concrete task objective for this node.
    pub objective: String,
    /// Observable checks that determine whether this node is complete.
    pub acceptance: Vec<String>,
    /// Whether the node should run locally, in a delegated subagent, or be chosen
    /// automatically by the scheduler.
    pub executor_policy: PlanExecutorPolicy,
    /// Tools, workspace scopes, and per-call limits available to this node.
    pub harness: PlanHarnessSnapshot,
    /// Typed transient retry policy for this node revision.
    pub recovery_policy: PlanRecoveryPolicySnapshot,
    /// Dependencies expressed as existing runtime ids or client keys declared in
    /// the same update request.
    pub depends_on: Vec<PlanNodeReferenceInput>,
    /// Recursive direct children. Lazy subagent decomposition reports only direct
    /// children through `report_plan_attempt` instead of calling `update_plan`.
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
    /// Preserve the exact current tree while changing lifecycle intent, such as
    /// starting execution after the user approves an already-authored plan.
    UseCurrentPlan { expected_plan_revision: u64 },
}

/// Coordinator-authored plan update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "Call begin_plan before update_plan. Define a complete tree while planning, or replace one mutable future subtree while planning or executing. New nodes use client_key and omit id; existing nodes use id and omit client_key. update_plan never marks nodes completed and never authors status, attempts, leases, progress, or results because those are runtime-owned.",
    example = update_plan_define_example()
)]
pub struct UpdatePlanInput {
    /// Short explanation recorded in the durable plan revision history.
    pub reason: String,
    /// Lifecycle choice. Use `execute_if_authorized` when the user already asked
    /// to carry out the plan; use `request_user_review` only when another review
    /// is actually wanted or required.
    pub execution_intent: PlanExecutionIntent,
    /// Optional node currently receiving the coordinator's attention.
    pub coordinator_node_id: Option<PlanNodeId>,
    /// Optional scheduler concurrency preference within the runtime ceiling.
    pub max_concurrency_hint: Option<usize>,
    /// Tagged JSON object with `type: define_plan`, `type: replace_subtree`, or
    /// `type: use_current_plan`. Never pass this field as a string.
    pub change: PlanChangeInput,
}

fn update_plan_define_example() -> serde_json::Value {
    serde_json::json!({
        "reason": "The user asked to implement the change using this plan",
        "execution_intent": "execute_if_authorized",
        "coordinator_node_id": null,
        "max_concurrency_hint": 2,
        "change": {
            "type": "define_plan",
            "expected_plan_revision": 0,
            "root": {
                "client_key": "root",
                "objective": "Implement the requested change",
                "acceptance": ["Focused tests pass"],
                "executor_policy": "auto",
                "harness": {
                    "model_role": null,
                    "reasoning_effort": null,
                    "checkpoint_turn_interval": null,
                    "provider_request_timeout_ms": null,
                    "tool_timeout_ms": null,
                    "allowed_tools": ["run_process"],
                    "read_scope": ["crates/merry-runtime"],
                    "write_scope": ["crates/merry-runtime"],
                    "forbidden_paths": [".git"]
                },
                "recovery_policy": {
                    "max_transient_attempts": 2,
                    "retry_backoff_ms": 0,
                    "retry_only_before_observable_side_effects": true
                },
                "depends_on": [],
                "children": []
            }
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanUpdateOutput {
    pub snapshot: PlanSnapshot,
    pub client_key_ids: BTreeMap<String, PlanNodeId>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlanDirectiveToolOutput {
    pub(crate) plan_id: PlanId,
    pub(crate) revision: u64,
    pub(crate) directive: merry_core::CoordinatorDirectiveSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlanProgressToolOutput {
    pub(crate) plan_id: PlanId,
    pub(crate) revision: u64,
    pub(crate) progress: merry_core::PlanAttemptProgressSnapshot,
    pub(crate) updated_directives: Vec<merry_core::CoordinatorDirectiveSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlanAttemptToolOutput {
    pub(crate) plan_id: PlanId,
    pub(crate) revision: u64,
    pub(crate) phase: PlanPhase,
    pub(crate) attempt: merry_core::PlanAttemptSnapshot,
    pub(crate) ready_node_ids: Vec<PlanNodeId>,
    pub(crate) client_key_ids: BTreeMap<String, PlanNodeId>,
}
