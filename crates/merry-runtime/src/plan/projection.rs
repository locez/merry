use merry_core::{PlanNodeStatus, PlanPhase, PlanSnapshot};
use serde::Serialize;

#[derive(Serialize)]
struct CoordinatorPlanProjection<'a> {
    plan_id: &'a merry_core::PlanId,
    revision: u64,
    phase: PlanPhase,
    root_node_id: Option<&'a merry_core::PlanNodeId>,
    scheduler_status: merry_core::PlanSchedulerStatus,
    nodes: Vec<CoordinatorNodeProjection<'a>>,
    approval_requirements: &'a [merry_core::PlanApprovalRequirementSnapshot],
}

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
}

pub(crate) fn coordinator_plan_control_message(snapshot: &PlanSnapshot) -> String {
    let projection = CoordinatorPlanProjection {
        plan_id: &snapshot.plan_id,
        revision: snapshot.revision,
        phase: snapshot.phase,
        root_node_id: snapshot.root_node_id.as_ref(),
        scheduler_status: snapshot.scheduler_status,
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
            })
            .collect(),
        approval_requirements: &snapshot.approval_requirements,
    };
    format!(
        "<plan_context>\n{}\n</plan_context>",
        serde_json::to_string(&projection).expect("validated plan projection serializes")
    )
}
