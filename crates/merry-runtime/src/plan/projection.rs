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
    live_attempts: Vec<&'a merry_core::PlanAttemptSnapshot>,
    live_leases: Vec<&'a merry_core::PlanLeaseSnapshot>,
    live_progress: Vec<&'a merry_core::PlanAttemptProgressSnapshot>,
    unresolved_directives: Vec<&'a merry_core::CoordinatorDirectiveSnapshot>,
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
        live_attempts: snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.outcome.is_none())
            .collect(),
        live_leases: snapshot
            .leases
            .iter()
            .filter(|lease| lease.status == merry_core::PlanLeaseStatus::Live)
            .collect(),
        live_progress: snapshot
            .attempt_progress
            .iter()
            .filter(|progress| {
                snapshot.attempts.iter().any(|attempt| {
                    attempt.attempt_id == progress.attempt_id && attempt.outcome.is_none()
                })
            })
            .collect(),
        unresolved_directives: snapshot
            .directives
            .iter()
            .filter(|directive| {
                !matches!(
                    directive.status,
                    merry_core::PlanDirectiveStatus::Applied
                        | merry_core::PlanDirectiveStatus::Superseded
                        | merry_core::PlanDirectiveStatus::Expired
                )
            })
            .collect(),
        approval_requirements: &snapshot.approval_requirements,
    };
    format!(
        "<plan_context>\n{}\n</plan_context>",
        serde_json::to_string(&projection).expect("validated plan projection serializes")
    )
}

#[derive(Serialize)]
struct WorkerPlanProjection<'a> {
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
}

pub(crate) fn worker_plan_control_message(
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
    let projection = WorkerPlanProjection {
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
    };
    Some(format!(
        "<plan_worker_context>\n{}\n</plan_worker_context>",
        serde_json::to_string(&projection).expect("validated worker plan projection serializes")
    ))
}
