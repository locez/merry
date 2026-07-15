use merry_core::{PlanNodeStatus, PlanPhase, PlanSnapshot};
use serde::Serialize;

#[derive(Serialize)]
struct CoordinatorPlanProjection<'a> {
    plan_id: &'a merry_core::PlanId,
    revision: u64,
    phase: PlanPhase,
    root_node_id: Option<&'a merry_core::PlanNodeId>,
    nodes: Vec<CoordinatorNodeProjection<'a>>,
    approval_requirements: &'a [merry_core::PlanApprovalRequirementSnapshot],
    coordinator_guidance: CoordinatorPlanGuidance,
}

#[derive(Serialize)]
struct CoordinatorPlanGuidance {
    phase_action: &'static str,
    instruction: &'static str,
    rules: &'static [&'static str],
}

pub(crate) const COORDINATOR_ROOT_SCOPE_GUIDANCE: &str = "The coordinator authors the root and direct work items for the request; do not pre-create descendants under a delegated linked node.";
pub(crate) const LINKED_CHILD_DECOMPOSITION_GUIDANCE: &str =
    "Once a node is linked, its child owns decomposition below that binding.";
pub(crate) const COORDINATOR_LINKED_SUMMARIES_GUIDANCE: &str = "The coordinator observes linked child summaries via read_plan and does not mirror the child subtree.";
pub(crate) const COORDINATOR_ACTIVE_LINK_MUTATION_GUIDANCE: &str = "A node with an active linked child, or any subtree containing one, is read-only to the coordinator. Continue unrelated work or wait for a terminal result; if the assignment must stop, cancel it through the runtime and create a new assignment instead of rewriting this node.";
pub(crate) const CHILD_LINKED_SCOPE_GUIDANCE: &str = "Work only within the linked node and its subtree; do not author or inspect the coordinator or sibling work.";
pub(crate) const CHILD_SCOPED_UPDATE_GUIDANCE: &str = "Scoped update_plan automatically attaches authored children below the linked binding; do not create a separate root or parent binding.";
pub(crate) const RUNTIME_OWNED_EXECUTION_GUIDANCE: &str = "Runtime owns execution statuses and summaries, including attempts, leases, and progress; do not author or mirror those fields.";
pub(crate) const PLAN_SEMANTIC_CHECKPOINT_GUIDANCE: &str = "Plan updates are semantic checkpoints, not heartbeats; do not emit high-frequency progress updates.";
const COORDINATOR_RULES: &[&str] = &[
    COORDINATOR_ROOT_SCOPE_GUIDANCE,
    LINKED_CHILD_DECOMPOSITION_GUIDANCE,
    COORDINATOR_LINKED_SUMMARIES_GUIDANCE,
    COORDINATOR_ACTIVE_LINK_MUTATION_GUIDANCE,
    RUNTIME_OWNED_EXECUTION_GUIDANCE,
    "update_plan authors objectives, acceptance, dependencies, and future structure. Linked execution summaries and statuses are runtime-owned.",
    "Plan is an auxiliary projection: it does not grant or restrict ordinary tools and it is not the execution result.",
    "When delegating work, bind the child explicitly with plan_task; omitted plan_task keeps the child unbound.",
    "After a successful read_plan, use the returned snapshot; do not repeat read_plan unless runtime state has changed.",
    "Use ordinary run_process for a read-only check that fits the active process profile; request_permissions is only for an exact action rejected for a missing capability.",
];

#[derive(Serialize)]
struct CoordinatorNodeProjection<'a> {
    id: &'a merry_core::PlanNodeId,
    parent_id: Option<&'a merry_core::PlanNodeId>,
    sibling_order: u16,
    objective: &'a str,
    acceptance: &'a [String],
    status: PlanNodeStatus,
    executor_policy: merry_core::PlanExecutorPolicy,
    depends_on: &'a [merry_core::PlanNodeId],
    updated_revision: u64,
    execution_summary: &'a merry_core::PlanExecutionSummary,
    links: &'a [merry_core::PlanLinkSnapshot],
}

pub(crate) fn coordinator_plan_control_message(snapshot: &PlanSnapshot) -> String {
    let projection = CoordinatorPlanProjection {
        plan_id: &snapshot.plan_id,
        revision: snapshot.revision,
        phase: snapshot.phase,
        root_node_id: snapshot.root_node_id.as_ref(),
        nodes: snapshot
            .nodes
            .iter()
            .filter(|node| node.status != PlanNodeStatus::Superseded)
            .map(|node| CoordinatorNodeProjection {
                id: &node.id,
                parent_id: node.parent_id.as_ref(),
                sibling_order: node.sibling_order,
                objective: &node.objective,
                acceptance: &node.acceptance,
                status: node.status,
                executor_policy: node.executor_policy,
                depends_on: &node.depends_on,
                updated_revision: node.updated_revision,
                execution_summary: &node.execution_summary,
                links: &node.links,
            })
            .collect(),
        approval_requirements: &snapshot.approval_requirements,
        coordinator_guidance: coordinator_guidance(snapshot.phase),
    };
    format!(
        "<plan_context>\n{}\n</plan_context>",
        serde_json::to_string(&projection).expect("validated plan projection serializes")
    )
}

pub(crate) fn coordinator_plan_inactive_control_message() -> String {
    format!(
        "<plan_context>\n{}\n</plan_context>",
        serde_json::json!({
            "phase": "inactive",
            "plan_id": null,
            "revision": 0,
            "root_node_id": null,
            "nodes": [],
            "approval_requirements": [],
            "coordinator_guidance": CoordinatorPlanGuidance {
                phase_action: "choose_whether_to_plan",
                instruction: "No active Plan exists. Do not call read_plan again. If the task benefits from a durable plan, call update_plan directly; otherwise continue with ordinary registered tools.",
                rules: COORDINATOR_RULES,
            }
        })
    )
}

fn coordinator_guidance(phase: PlanPhase) -> CoordinatorPlanGuidance {
    match phase {
        PlanPhase::Planning => CoordinatorPlanGuidance {
            phase_action: "define_or_refine_plan",
            instruction: "Use read_plan for exact current state and update_plan to define or refine authored intent. The first valid update creates the plan; ordinary work remains available throughout.",
            rules: COORDINATOR_RULES,
        },
        PlanPhase::AwaitingApproval => CoordinatorPlanGuidance {
            phase_action: "wait_for_user_approval",
            instruction: "Explain the pending approval requirement and wait for user resolution when one exists. This phase does not disable ordinary tools; update_plan remains available for an explicit revision.",
            rules: COORDINATOR_RULES,
        },
        PlanPhase::Executing => CoordinatorPlanGuidance {
            phase_action: "coordinate_active_execution",
            instruction: "Inspect linked child lifecycle with read_plan, revise future authored structure with update_plan, and use spawn_subagents for actual delegated work. Runtime derives completion from child lifecycle; no model report is required.",
            rules: COORDINATOR_RULES,
        },
        PlanPhase::Blocked => CoordinatorPlanGuidance {
            phase_action: "explain_blocker_or_request_revision",
            instruction: "Inspect the linked execution summary with read_plan and explain the blocker. Revise authored future work with update_plan or delegate a replacement child explicitly.",
            rules: COORDINATOR_RULES,
        },
        PlanPhase::Completed | PlanPhase::Cancelled => CoordinatorPlanGuidance {
            phase_action: "summarize_terminal_plan",
            instruction: "The plan projection is terminal. Read exact state if needed; a later update can define new authored work without replaying old attempts.",
            rules: COORDINATOR_RULES,
        },
    }
}

#[derive(Serialize)]
struct SubagentPlanProjection<'a> {
    plan_id: &'a merry_core::PlanId,
    revision: u64,
    phase: PlanPhase,
    node: &'a merry_core::PlanNodeSnapshot,
    ancestor_path: Vec<&'a merry_core::PlanNodeSnapshot>,
    dependency_results: Vec<&'a merry_core::PlanNodeSnapshot>,
    attempt: &'a merry_core::PlanAttemptSnapshot,
    lease: &'a merry_core::PlanLeaseSnapshot,
    progress: Option<&'a merry_core::PlanAttemptProgressSnapshot>,
    unresolved_directives: Vec<&'a merry_core::CoordinatorDirectiveSnapshot>,
    child_guidance: SubagentPlanGuidance,
}

#[derive(Serialize)]
struct SubagentPlanGuidance {
    instruction: &'static str,
    rules: &'static [&'static str],
}

const CHILD_GUIDANCE_RULES: &[&str] = &[
    CHILD_SCOPED_UPDATE_GUIDANCE,
    RUNTIME_OWNED_EXECUTION_GUIDANCE,
    PLAN_SEMANTIC_CHECKPOINT_GUIDANCE,
];

fn child_guidance() -> SubagentPlanGuidance {
    SubagentPlanGuidance {
        instruction: CHILD_LINKED_SCOPE_GUIDANCE,
        rules: CHILD_GUIDANCE_RULES,
    }
}

pub(crate) fn plan_subagent_control_message(
    snapshot: &PlanSnapshot,
    node_id: &merry_core::PlanNodeId,
    attempt_id: &merry_core::PlanAttemptId,
    lease_id: &merry_core::PlanLeaseId,
) -> Option<String> {
    let node = snapshot.nodes.iter().find(|node| &node.id == node_id)?;
    let attempt = snapshot
        .attempts
        .iter()
        .find(|attempt| &attempt.attempt_id == attempt_id)?;
    let lease = snapshot
        .leases
        .iter()
        .find(|lease| &lease.lease_id == lease_id)?;
    let mut ancestor_path = Vec::new();
    let mut parent_id = node.parent_id.as_ref();
    while let Some(id) = parent_id {
        let parent = snapshot.nodes.iter().find(|node| &node.id == id)?;
        ancestor_path.push(parent);
        parent_id = parent.parent_id.as_ref();
    }
    ancestor_path.reverse();
    let dependency_results = node
        .depends_on
        .iter()
        .filter_map(|id| snapshot.nodes.iter().find(|node| &node.id == id))
        .collect();
    let progress = snapshot
        .attempt_progress
        .iter()
        .find(|progress| &progress.attempt_id == attempt_id);
    let unresolved_directives = snapshot
        .directives
        .iter()
        .filter(|directive| {
            &directive.attempt_id == attempt_id
                && !matches!(
                    directive.status,
                    merry_core::PlanDirectiveStatus::Applied
                        | merry_core::PlanDirectiveStatus::Superseded
                        | merry_core::PlanDirectiveStatus::Expired
                )
        })
        .collect();
    let projection = SubagentPlanProjection {
        plan_id: &snapshot.plan_id,
        revision: snapshot.revision,
        phase: snapshot.phase,
        node,
        ancestor_path,
        dependency_results,
        attempt,
        lease,
        progress,
        unresolved_directives,
        child_guidance: child_guidance(),
    };
    Some(format!(
        "<plan_subagent_context>\n{}\n</plan_subagent_context>",
        serde_json::to_string(&projection).expect("validated subagent plan projection serializes")
    ))
}
