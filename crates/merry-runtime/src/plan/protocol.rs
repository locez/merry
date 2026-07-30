use merry_core::{
    ArtifactRef, ErrorInfo, EvidenceRef, PlanApprovalRequirementId,
    PlanApprovalRequirementSnapshot, PlanAttemptId, PlanAttemptOutcome,
    PlanCapabilityEnvelopeSnapshot, PlanDirectiveConstraints, PlanDirectiveId, PlanDirectiveKind,
    PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanNodeId, PlanNodeResult, PlanNodeStatus,
    PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, PlanSnapshot, SkillId,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::BTreeMap;

use super::{
    PLAN_READ_MAX_DEPTH,
    validation::{
        MAX_ACCEPTANCE_BYTES, MAX_ACCEPTANCE_ITEMS, MAX_CLIENT_KEY_BYTES, MAX_DEPENDENCIES,
        MAX_DIRECT_CHILDREN, MAX_OBJECTIVE_BYTES, MAX_PAYLOAD_ITEMS, MAX_PAYLOAD_TEXT_BYTES,
        MAX_REASON_BYTES,
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

/// Runtime-owned steering request for one live attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlPlanAttemptInput {
    pub attempt_id: PlanAttemptId,
    pub kind: PlanDirectiveKind,
    pub reason: String,
    pub instruction: Option<String>,
    pub constraints: Option<PlanDirectiveConstraints>,
    #[schemars(
        description = "Requested output items. Each item is at most 1024 UTF-8 bytes.",
        length(max = MAX_PAYLOAD_ITEMS),
        inner(length(min = 1, max = MAX_PAYLOAD_TEXT_BYTES))
    )]
    pub requested_output: Vec<String>,
}

/// Non-terminal semantic progress for the attempt bound to this runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportPlanProgressInput {
    pub summary: String,
    #[schemars(length(max = MAX_PAYLOAD_ITEMS))]
    pub evidence_refs: Vec<EvidenceRef>,
    #[schemars(length(max = MAX_PAYLOAD_ITEMS))]
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
    #[schemars(description = "Short non-blank reason for adding direct child work.")]
    pub reason: String,
    #[schemars(schema_with = "direct_child_nodes_schema")]
    pub children: Vec<PlanNodeInput>,
}

/// Child-owned update to the subtree below one linked Plan node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubagentPlanUpdateInput {
    #[schemars(description = "Short non-blank reason for this scoped authored update.")]
    pub reason: String,
    #[schemars(
        description = "Scoped tagged change. Define direct children or replace one mutable scoped node with its direct children."
    )]
    pub change: SubagentPlanChangeInput,
}

/// Scoped tree changes accepted from a linked child runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
pub enum SubagentPlanChangeInput {
    DefineChildren {
        #[schemars(
            description = "Expected current active Plan revision. Use the exact revision returned by read_plan."
        )]
        expected_plan_revision: u64,
        #[schemars(schema_with = "direct_child_nodes_schema")]
        children: Vec<PlanNodeInput>,
    },
    ReplaceSubtree {
        #[schemars(description = "Runtime-owned id of the mutable scoped node to replace.")]
        target_node_id: PlanNodeId,
        #[schemars(
            description = "Expected revision of target_node_id before applying the replacement."
        )]
        expected_node_revision: u64,
        #[schemars(
            description = "Replacement node with direct children only; deeper work is authored by a later scoped update."
        )]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Provider input remains intentionally permissive about identity. Runtime
/// validation enforces exactly one of `id` and `client_key`, while the schema
/// explains the two forms without forcing the model through an identity
/// `oneOf` branch.
pub struct PlanNodeInput {
    /// Runtime-owned id from a prior plan result or read. Set this only when
    /// retaining an existing mutable node; otherwise omit it or use null.
    pub id: Option<PlanNodeId>,
    /// Unique request-local key for a new node. Set this for every new node and
    /// omit `id`; a successful update may include this value in
    /// `bindable_plan_client_keys` for subagent binding. It is not a runtime
    /// node id.
    pub client_key: Option<String>,
    /// Concrete task objective for this node.
    pub objective: String,
    /// Observable checks that determine whether this node is complete.
    pub acceptance: Vec<String>,
    /// Optional authored declaration. When omitted for an existing node, the
    /// current declared status is retained.
    #[serde(default)]
    pub status: Option<PlanNodeStatus>,
    /// Runtime-owned execution preference retained for internal snapshots. It
    /// is not provider-authored and is omitted from the generated schema.
    #[serde(default, skip_serializing)]
    pub executor_policy: PlanExecutorPolicy,
    /// Runtime-owned capability data retained for internal snapshots.
    #[serde(default, skip_serializing)]
    pub harness: PlanHarnessSnapshot,
    /// Runtime-owned retry policy retained for internal snapshots.
    #[serde(default, skip_serializing)]
    pub recovery_policy: PlanRecoveryPolicySnapshot,
    /// Dependencies expressed as existing runtime ids or client keys declared in
    /// the same update request.
    #[serde(default)]
    pub depends_on: Vec<PlanNodeReferenceInput>,
    /// Direct children authored as part of the Plan tree. Provider-visible
    /// schemas expose only one level; runtime-owned child scopes author deeper
    /// work after an explicit binding.
    #[serde(default)]
    pub children: Vec<PlanNodeInput>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "One authored plan node with at most one level of direct children. Provide exactly one identity field: client_key for a new node or id for an existing mutable node. Runtime validates this choice; effective status, execution state, links, attempts, leases, progress, results, parent ids, and sibling order are runtime-owned."
)]
struct PlanNodeInputSchema {
    #[schemars(
        description = "Runtime-owned node id for retaining an existing mutable node. Omit it for a new node; provide exactly one of id or client_key."
    )]
    id: Option<PlanNodeId>,
    #[schemars(
        description = "Stable authored client key for a new node. A successful update may return it in bindable_plan_client_keys for use as spawn_subagents.tasks[].plan_client_key. Omit it when retaining an existing node by id; never put a runtime node id here.",
        length(min = 1, max = MAX_CLIENT_KEY_BYTES)
    )]
    client_key: Option<String>,
    #[schemars(
        description = "Concrete objective for this node. It must be non-blank and at most 2048 UTF-8 bytes.",
        length(min = 1, max = MAX_OBJECTIVE_BYTES)
    )]
    objective: String,
    #[schemars(
        description = "Observable completion checks for this node. Provide at most 16 checks; each must be non-blank and at most 1024 UTF-8 bytes.",
        length(max = MAX_ACCEPTANCE_ITEMS),
        inner(length(min = 1, max = MAX_ACCEPTANCE_BYTES))
    )]
    acceptance: Vec<String>,
    #[schemars(
        description = "Optional authored status for local or unbound work. Runtime-owned linked execution status is derived separately.",
        schema_with = "authored_status_schema"
    )]
    #[serde(default)]
    status: Option<PlanNodeStatus>,
    #[schemars(
        description = "Dependencies expressed as existing runtime ids or client keys declared in this update. Provide at most 16 dependencies.",
        length(max = MAX_DEPENDENCIES)
    )]
    #[serde(default)]
    depends_on: Vec<PlanNodeReferenceInput>,
    #[schemars(
        description = "Direct child nodes only. Each child is a leaf in this request and must not contain its own children.",
        length(max = MAX_DIRECT_CHILDREN)
    )]
    #[serde(default)]
    children: Vec<PlanNodeShallowSchema>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "A direct child authored below one plan node. It cannot contain nested children; a linked child scope owns any deeper decomposition."
)]
struct PlanNodeShallowSchema {
    #[schemars(
        description = "Runtime-owned node id for retaining an existing mutable node. Omit it for a new node; provide exactly one of id or client_key."
    )]
    id: Option<PlanNodeId>,
    #[schemars(
        description = "Stable authored client key for a new node. A successful update may return it in bindable_plan_client_keys for use as spawn_subagents.tasks[].plan_client_key. Omit it when retaining an existing node by id; never put a runtime node id here.",
        length(min = 1, max = MAX_CLIENT_KEY_BYTES)
    )]
    client_key: Option<String>,
    #[schemars(
        description = "Concrete objective for this node. It must be non-blank and at most 2048 UTF-8 bytes.",
        length(min = 1, max = MAX_OBJECTIVE_BYTES)
    )]
    objective: String,
    #[schemars(
        description = "Observable completion checks for this node. Provide at most 16 checks; each must be non-blank and at most 1024 UTF-8 bytes.",
        length(max = MAX_ACCEPTANCE_ITEMS),
        inner(length(min = 1, max = MAX_ACCEPTANCE_BYTES))
    )]
    acceptance: Vec<String>,
    #[schemars(
        description = "Optional authored status for local or unbound work. Runtime-owned linked execution status is derived separately.",
        schema_with = "authored_status_schema"
    )]
    #[serde(default)]
    status: Option<PlanNodeStatus>,
    #[schemars(
        description = "Dependencies expressed as existing runtime ids or client keys declared in this update. Provide at most 16 dependencies.",
        length(max = MAX_DEPENDENCIES)
    )]
    #[serde(default)]
    depends_on: Vec<PlanNodeReferenceInput>,
}

impl JsonSchema for PlanNodeInput {
    fn schema_name() -> Cow<'static, str> {
        "PlanNodeInput".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::PlanNodeInput").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        PlanNodeInputSchema::json_schema(generator)
    }
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

fn direct_child_nodes_schema(generator: &mut SchemaGenerator) -> Schema {
    let mut schema = <Vec<PlanNodeShallowSchema>>::json_schema(generator);
    schema.insert(
        "description".into(),
        "Direct child nodes only. Each node must be a leaf in this request; deeper decomposition belongs to an explicitly linked child scope.".into(),
    );
    schema
}

/// Stable tagged change shape for complete planning and execution-time revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(
    description = "Tagged change object. The required nested string discriminator is type; valid coordinator values are define_plan, replace_subtree, and use_current_plan."
)]
pub enum PlanChangeInput {
    DefinePlan {
        #[schemars(
            description = "Expected current plan revision. Use 0 when defining the first plan."
        )]
        expected_plan_revision: u64,
        #[schemars(
            description = "Root authored node. Its direct children are the coordinator's work items; do not nest implementation descendants under delegated nodes."
        )]
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
        #[schemars(
            description = "Replacement root retaining target_node_id, with direct children only. Deeper work belongs to the linked child scope."
        )]
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
    description = "Create or update the authored Plan tree. The first valid update creates the Plan. New nodes use client_key and omit id; existing nodes use id and omit client_key. Runtime-owned execution state, capabilities, attempts, leases, progress, and results are not authored here. The nested change.type discriminator is required.",
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
        description = "Required tagged change object. It must contain a nested string field type inside the change object; the field example shows a complete valid define_plan object. Valid values are define_plan, replace_subtree, and use_current_plan; do not omit type, put it on the outer update object, or pass change as a string.",
        example = update_plan_define_change_example()
    )]
    pub change: PlanChangeInput,
}

pub(crate) fn update_plan_define_example() -> serde_json::Value {
    serde_json::json!({
        "reason": "The user asked to implement the change using this plan",
        "execution_intent": "execute_if_authorized",
        "coordinator_node_id": null,
        "max_concurrency_hint": 2,
        "change": update_plan_define_change_example()
    })
}

fn update_plan_define_change_example() -> serde_json::Value {
    serde_json::json!({
        "type": "define_plan",
        "expected_plan_revision": 0,
        "root": {
            "client_key": "root",
            "objective": "Implement the requested change",
            "acceptance": ["Focused tests pass"],
            "depends_on": [],
            "children": []
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanUpdateOutput {
    pub snapshot: PlanSnapshot,
    /// Runtime-owned ids allocated for the authored client keys in this update.
    /// This mapping is for internal Plan control and is intentionally not
    /// returned in the provider-visible `update_plan` result.
    pub client_key_to_runtime_node_id: BTreeMap<String, PlanNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlanUpdateToolOutput {
    pub(crate) plan_id: PlanId,
    pub(crate) revision: u64,
    pub(crate) phase: PlanPhase,
    /// Authored keys accepted by `spawn_subagents.plan_client_key`.
    /// Runtime node ids are intentionally omitted from this provider-visible
    /// result; `read_plan` supplies them when an exact Plan target is needed.
    pub(crate) bindable_plan_client_keys: Vec<String>,
    pub(crate) approval_requirements: Vec<PlanApprovalRequirementSnapshot>,
}

impl From<&PlanUpdateOutput> for PlanUpdateToolOutput {
    fn from(output: &PlanUpdateOutput) -> Self {
        Self {
            plan_id: output.snapshot.plan_id.clone(),
            revision: output.snapshot.revision,
            phase: output.snapshot.phase,
            bindable_plan_client_keys: output
                .client_key_to_runtime_node_id
                .keys()
                .cloned()
                .collect(),
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
    pub(crate) client_key_to_runtime_node_id: BTreeMap<String, PlanNodeId>,
}

#[cfg(test)]
mod tests {
    use super::{PlanUpdateOutput, PlanUpdateToolOutput};
    use merry_core::{
        PlanActivationSource, PlanId, PlanNodeId, PlanPhase, PlanResourcePolicySnapshot,
        PlanSchedulerStatus, PlanSnapshot,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn provider_update_output_exposes_bindable_keys_without_runtime_node_ids() {
        let runtime_node_id = PlanNodeId::new("plan-node-2").expect("valid runtime node id");
        let output = PlanUpdateOutput {
            snapshot: PlanSnapshot {
                plan_id: PlanId::new("plan-output").expect("valid plan id"),
                revision: 1,
                phase: PlanPhase::Planning,
                activation_source: PlanActivationSource::User,
                root_node_id: None,
                coordinator_node_id: None,
                execution_contract_fingerprint: None,
                execution_authorization_refs: Vec::new(),
                authorized_capability_envelope: None,
                approval_requirements: Vec::new(),
                nodes: Vec::new(),
                attempts: Vec::new(),
                leases: Vec::new(),
                attempt_progress: Vec::new(),
                directives: Vec::new(),
                resource_policy_snapshot: PlanResourcePolicySnapshot::default(),
                max_concurrency_hint: None,
                scheduler_status: PlanSchedulerStatus::Active,
                revision_summaries: Vec::new(),
            },
            client_key_to_runtime_node_id: BTreeMap::from([(
                "agent1_task".to_owned(),
                runtime_node_id,
            )]),
        };

        let json = serde_json::to_value(PlanUpdateToolOutput::from(&output))
            .expect("provider output serializes");
        assert_eq!(json["bindable_plan_client_keys"], json!(["agent1_task"]));
        assert!(json.get("client_key_to_runtime_node_id").is_none());
        assert!(!json.to_string().contains("plan-node-2"));
    }
}
