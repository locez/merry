use merry_core::{
    ArtifactRef, ErrorInfo, EvidenceRef, PlanApprovalRequirementId,
    PlanApprovalRequirementSnapshot, PlanAttemptId, PlanAttemptOutcome,
    PlanCapabilityEnvelopeSnapshot, PlanDirectiveConstraints, PlanDirectiveId, PlanDirectiveKind,
    PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanNodeId, PlanNodeResult, PlanNodeStatus,
    PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, PlanSnapshot, SkillId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{
    PLAN_READ_MAX_DEPTH,
    validation::{
        MAX_ACCEPTANCE_BYTES, MAX_ACCEPTANCE_ITEMS, MAX_CLIENT_KEY_BYTES, MAX_DEPENDENCIES,
        MAX_DIRECT_CHILDREN, MAX_OBJECTIVE_BYTES, MAX_REASON_BYTES,
    },
};

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
    #[schemars(description = "Optional durable plan id to read. Omit it to read the active plan.")]
    pub plan_id: Option<merry_core::PlanId>,
    #[schemars(
        description = "Optional node id whose direct subtree should be returned. Omit it to read from the plan root."
    )]
    pub node_id: Option<PlanNodeId>,
    #[schemars(
        description = "Maximum child depth to include in the returned subtree. Zero returns only the selected node; omit it to use the runtime maximum.",
        range(min = 0, max = PLAN_READ_MAX_DEPTH)
    )]
    pub max_depth: Option<u8>,
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

/// Child-owned update to the subtree below one linked Plan node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubagentPlanUpdateInput {
    pub reason: String,
    pub change: SubagentPlanChangeInput,
}

/// Scoped tree changes accepted from a linked child runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
pub enum SubagentPlanChangeInput {
    DefineChildren {
        expected_plan_revision: u64,
        children: Vec<PlanNodeInput>,
    },
    ReplaceSubtree {
        target_node_id: PlanNodeId,
        expected_node_revision: u64,
        subtree: PlanNodeInput,
    },
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
    Id {
        #[schemars(
            description = "Runtime-owned plan node id returned by an earlier plan operation."
        )]
        id: PlanNodeId,
    },
    ClientKey {
        #[schemars(
            description = "Request-local client key declared by another new node in the same update.",
            length(min = 1, max = MAX_CLIENT_KEY_BYTES)
        )]
        client_key: String,
    },
}

/// Provider-visible authored node input. Effective runtime state is separate;
/// an optional declared status can be supplied for the authored projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "One authored plan node. New nodes use client_key and omit id. Existing mutable nodes use id and omit client_key. Effective status, attempts, leases, progress, results, parent ids, and sibling order are runtime-owned. The optional declared status accepts pending, in_progress, completed, or failed.",
    extend("oneOf" = [
        {
            "title": "New node",
            "required": ["client_key"],
            "properties": {
                "id": {
                    "type": "null",
                    "description": "Runtime-owned node id to retain when replacing an existing mutable node. Omit it for a new node."
                },
                "client_key": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_CLIENT_KEY_BYTES,
                    "description": "Unique request-local key for a new node. Provide it when id is omitted; omit it when retaining an existing node."
                }
            }
        },
        {
            "title": "Existing mutable node",
            "required": ["id"],
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Runtime-owned node id to retain when replacing an existing mutable node. Omit it for a new node."
                },
                "client_key": {
                    "type": "null",
                    "description": "Unique request-local key for a new node. Provide it when id is omitted; omit it when retaining an existing node."
                }
            }
        }
    ])
)]
pub struct PlanNodeInput {
    /// Runtime-owned id from a prior plan result or read. Set this only when
    /// retaining an existing mutable node; otherwise omit it or use null.
    #[schemars(
        description = "Runtime-owned node id to retain when replacing an existing mutable node. Omit it for a new node."
    )]
    pub id: Option<PlanNodeId>,
    /// Unique request-local key for a new node. Set this for every new node and
    /// omit `id`; the update result maps this key to its runtime-owned id.
    #[schemars(
        description = "Unique request-local key for a new node. Provide it when id is omitted; omit it when retaining an existing node.",
        length(min = 1, max = MAX_CLIENT_KEY_BYTES)
    )]
    pub client_key: Option<String>,
    /// Concrete task objective for this node.
    #[schemars(
        description = "Concrete objective for this node. It must be non-blank and at most 2048 UTF-8 bytes.",
        length(min = 1, max = MAX_OBJECTIVE_BYTES)
    )]
    pub objective: String,
    /// Observable checks that determine whether this node is complete.
    #[schemars(
        description = "Observable completion checks for this node. Provide at most 16 checks; each must be non-blank and at most 1024 UTF-8 bytes.",
        length(max = MAX_ACCEPTANCE_ITEMS),
        inner(length(min = 1, max = MAX_ACCEPTANCE_BYTES))
    )]
    pub acceptance: Vec<String>,
    /// Optional authored declaration. When omitted for an existing node, the
    /// current declared status is retained.
    #[serde(default)]
    #[schemars(schema_with = "authored_status_schema")]
    pub status: Option<PlanNodeStatus>,
    /// Runtime-owned execution preference retained for internal snapshots. It
    /// is not provider-authored and is omitted from the generated schema.
    #[serde(default, skip_serializing)]
    #[schemars(skip)]
    pub executor_policy: PlanExecutorPolicy,
    /// Runtime-owned capability data retained for internal snapshots.
    #[serde(default, skip_serializing)]
    #[schemars(skip)]
    pub harness: PlanHarnessSnapshot,
    /// Runtime-owned retry policy retained for internal snapshots.
    #[serde(default, skip_serializing)]
    #[schemars(skip)]
    pub recovery_policy: PlanRecoveryPolicySnapshot,
    /// Dependencies expressed as existing runtime ids or client keys declared in
    /// the same update request.
    #[schemars(
        description = "Dependencies expressed as runtime node ids or client keys declared in this update. Provide at most 16 dependencies.",
        length(max = MAX_DEPENDENCIES)
    )]
    pub depends_on: Vec<PlanNodeReferenceInput>,
    /// Recursive direct children authored as part of the Plan tree.
    #[schemars(
        description = "Direct child nodes in the plan tree. Provide at most 16 children per node.",
        length(max = MAX_DIRECT_CHILDREN)
    )]
    pub children: Vec<PlanNodeInput>,
}

fn authored_status_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::Schema::try_from(serde_json::json!({
        "anyOf": [
            {
                "type": "string",
                "enum": ["pending", "in_progress", "completed", "failed"]
            },
            { "type": "null" }
        ]
    }))
    .expect("static authored status schema is valid")
}

/// Stable tagged change shape for complete planning and execution-time revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanChangeInput {
    DefinePlan {
        #[schemars(
            description = "Expected current plan revision. Use 0 when defining the first plan."
        )]
        expected_plan_revision: u64,
        #[schemars(description = "Root node of the complete authored plan tree.")]
        root: PlanNodeInput,
    },
    ReplaceSubtree {
        #[schemars(
            description = "Runtime-owned id of the mutable node whose subtree should be replaced."
        )]
        target_node_id: PlanNodeId,
        #[schemars(
            description = "Expected revision of target_node_id before applying the replacement."
        )]
        expected_node_revision: u64,
        #[schemars(description = "Replacement subtree. Its root must retain target_node_id.")]
        subtree: PlanNodeInput,
    },
    /// Preserve the exact current tree while changing lifecycle intent, such as
    /// starting execution after the user approves an already-authored plan.
    UseCurrentPlan {
        #[schemars(
            description = "Expected current plan revision before changing lifecycle intent."
        )]
        expected_plan_revision: u64,
    },
}

/// Coordinator-authored plan update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "Create or update the authored Plan tree. The first valid update creates the Plan. New nodes use client_key and omit id; existing nodes use id and omit client_key. Runtime-owned execution state, capabilities, attempts, leases, progress, and results are not authored here.",
    example = update_plan_define_example()
)]
pub struct UpdatePlanInput {
    /// Short explanation recorded in the durable plan revision history.
    #[schemars(
        description = "Short non-blank explanation recorded in durable plan revision history. Maximum 2048 UTF-8 bytes.",
        length(min = 1, max = MAX_REASON_BYTES)
    )]
    pub reason: String,
    /// Lifecycle choice. Use `execute_if_authorized` when the user already asked
    /// to carry out the plan; use `request_user_review` only when another review
    /// is actually wanted or required.
    #[schemars(
        description = "Lifecycle choice for this update. Use continue_planning for more authoring, execute_if_authorized when execution is already authorized, or request_user_review for an explicit review boundary."
    )]
    pub execution_intent: PlanExecutionIntent,
    /// Optional node currently receiving the coordinator's attention.
    #[schemars(
        description = "Optional runtime node id currently receiving coordinator attention."
    )]
    pub coordinator_node_id: Option<PlanNodeId>,
    /// Optional hint for how much linked delegation the coordinator expects.
    /// Runtime admission remains owned by the subagent manager.
    #[schemars(
        description = "Optional concurrency hint for linked delegated work. The runtime accepts values from 1 through the default maximum of 6.",
        range(min = 1, max = PlanResourcePolicySnapshot::DEFAULT_MAX_CONCURRENCY)
    )]
    pub max_concurrency_hint: Option<usize>,
    /// Tagged JSON object with `type: define_plan`, `type: replace_subtree`, or
    /// `type: use_current_plan`. Never pass this field as a string.
    #[schemars(
        description = "Tagged change object with type define_plan, replace_subtree, or use_current_plan."
    )]
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
    pub(crate) approval_requirements: Vec<PlanApprovalRequirementSnapshot>,
}

impl From<&PlanUpdateOutput> for PlanUpdateToolOutput {
    fn from(output: &PlanUpdateOutput) -> Self {
        Self {
            plan_id: output.snapshot.plan_id.clone(),
            revision: output.snapshot.revision,
            phase: output.snapshot.phase,
            client_key_ids: output.client_key_ids.clone(),
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
