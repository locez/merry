use super::{
    RuntimeInner,
    plan_tool_protocol::{PlanToolRejection, no_active_plan_rejection},
};
use crate::plan::{PLAN_READ_MAX_DEPTH, ReadPlanInput};
use merry_core::{
    PlanActivationSource, PlanApprovalRequirementSnapshot, PlanCapabilityEnvelopeSnapshot,
    PlanExecutionSummary, PlanId, PlanLinkSnapshot, PlanNodeId, PlanNodeResult, PlanNodeStatus,
    PlanPhase, PlanRevisionSummary, PlanSnapshot,
};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize)]
pub(super) struct ReadPlanOutput {
    snapshot: ReadPlanSnapshot,
    selected_node_id: Option<PlanNodeId>,
    next_cursor: Option<String>,
    guidance: ReadPlanGuidance,
}

#[derive(Serialize)]
struct ReadPlanGuidance {
    do_not_repeat_until_state_change: bool,
    instruction: &'static str,
}

#[derive(Serialize)]
struct ReadPlanSnapshot {
    plan_id: PlanId,
    revision: u64,
    phase: PlanPhase,
    activation_source: PlanActivationSource,
    root_node_id: Option<PlanNodeId>,
    coordinator_node_id: Option<PlanNodeId>,
    execution_contract_fingerprint: Option<String>,
    execution_authorization_refs: Vec<String>,
    authorized_capability_envelope: Option<PlanCapabilityEnvelopeSnapshot>,
    approval_requirements: Vec<PlanApprovalRequirementSnapshot>,
    nodes: Vec<ReadPlanNode>,
    max_concurrency_hint: Option<usize>,
    revision_summaries: Vec<PlanRevisionSummary>,
}

#[derive(Serialize)]
struct ReadPlanNode {
    id: PlanNodeId,
    client_key: Option<String>,
    parent_id: Option<PlanNodeId>,
    sibling_order: u16,
    objective: String,
    acceptance: Vec<String>,
    status: PlanNodeStatus,
    depends_on: Vec<PlanNodeId>,
    result: Option<PlanNodeResult>,
    created_revision: u64,
    updated_revision: u64,
    execution_summary: PlanExecutionSummary,
    links: Vec<PlanLinkSnapshot>,
}

impl From<&PlanSnapshot> for ReadPlanSnapshot {
    fn from(snapshot: &PlanSnapshot) -> Self {
        Self {
            plan_id: snapshot.plan_id.clone(),
            revision: snapshot.revision,
            phase: snapshot.phase,
            activation_source: snapshot.activation_source.clone(),
            root_node_id: snapshot.root_node_id.clone(),
            coordinator_node_id: snapshot.coordinator_node_id.clone(),
            execution_contract_fingerprint: snapshot.execution_contract_fingerprint.clone(),
            execution_authorization_refs: snapshot.execution_authorization_refs.clone(),
            authorized_capability_envelope: snapshot.authorized_capability_envelope.clone(),
            approval_requirements: snapshot.approval_requirements.clone(),
            nodes: snapshot
                .nodes
                .iter()
                .map(|node| ReadPlanNode {
                    id: node.id.clone(),
                    client_key: node.client_key.clone(),
                    parent_id: node.parent_id.clone(),
                    sibling_order: node.sibling_order,
                    objective: node.objective.clone(),
                    acceptance: node.acceptance.clone(),
                    status: node.status,
                    depends_on: node.depends_on.clone(),
                    result: node.result.clone(),
                    created_revision: node.created_revision,
                    updated_revision: node.updated_revision,
                    execution_summary: node.execution_summary.clone(),
                    links: node.links.clone(),
                })
                .collect(),
            max_concurrency_hint: snapshot.max_concurrency_hint,
            revision_summaries: snapshot.revision_summaries.clone(),
        }
    }
}

pub(super) async fn read_plan(
    inner: &RuntimeInner,
    input: ReadPlanInput,
) -> Result<ReadPlanOutput, PlanToolRejection> {
    let snapshot = {
        let session = inner.session.lock().await;
        match input.plan_id.as_ref() {
            Some(plan_id) => session
                .active_plan()
                .map(|plan| plan.snapshot())
                .filter(|snapshot| &snapshot.plan_id == plan_id)
                .or_else(|| {
                    session
                        .terminal_plans()
                        .iter()
                        .find(|snapshot| &snapshot.plan_id == plan_id)
                })
                .cloned()
                .ok_or_else(|| {
                    PlanToolRejection::new(
                        "plan_not_found",
                        format!("plan {plan_id} was not found"),
                    )
                })?,
            None => session
                .active_plan()
                .map(|plan| plan.snapshot().clone())
                .ok_or_else(no_active_plan_rejection)?,
        }
    };
    read_plan_snapshot(snapshot, input)
}

pub(super) fn read_plan_snapshot(
    mut snapshot: PlanSnapshot,
    input: ReadPlanInput,
) -> Result<ReadPlanOutput, PlanToolRejection> {
    let max_depth = input.max_depth.unwrap_or(PLAN_READ_MAX_DEPTH);
    if max_depth > PLAN_READ_MAX_DEPTH {
        return Err(PlanToolRejection::new(
            "plan_read_depth_exceeded",
            format!("max_depth must be at most {PLAN_READ_MAX_DEPTH}"),
        ));
    }

    let selected_node_id = input.node_id.as_ref().or(snapshot.root_node_id.as_ref());
    let selected_ids = selected_node_id
        .map(|node_id| subtree_ids(&snapshot, node_id, max_depth))
        .transpose()?;
    if let Some(selected_ids) = selected_ids.as_ref() {
        snapshot
            .nodes
            .retain(|node| selected_ids.contains(&node.id));
    }

    // Attempts, leases, progress, and directives belong to the removed model
    // reporting protocol. Keep them in durable history for migration/debugging,
    // but never put them back into the provider-visible Plan projection.
    snapshot.attempts.clear();
    snapshot.leases.clear();
    snapshot.attempt_progress.clear();
    snapshot.directives.clear();

    Ok(ReadPlanOutput {
        snapshot: ReadPlanSnapshot::from(&snapshot),
        selected_node_id: input.node_id,
        next_cursor: None,
        guidance: ReadPlanGuidance {
            do_not_repeat_until_state_change: true,
            instruction: "Use this exact snapshot for the next decision. Do not call read_plan again unless a runtime event changed the Plan; continue ordinary work or use update_plan for an actual authored revision.",
        },
    })
}

fn subtree_ids(
    snapshot: &PlanSnapshot,
    selected: &PlanNodeId,
    max_depth: u8,
) -> Result<BTreeSet<PlanNodeId>, PlanToolRejection> {
    if !snapshot.nodes.iter().any(|node| &node.id == selected) {
        return Err(PlanToolRejection::new(
            "plan_node_not_found",
            format!("node {selected} was not found in plan {}", snapshot.plan_id),
        ));
    }
    let mut selected_ids = BTreeSet::from([selected.clone()]);
    let mut frontier = vec![selected.clone()];
    for _ in 0..max_depth {
        let mut next = Vec::new();
        for node in &snapshot.nodes {
            if node
                .parent_id
                .as_ref()
                .is_some_and(|parent| frontier.contains(parent))
                && selected_ids.insert(node.id.clone())
            {
                next.push(node.id.clone());
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    Ok(selected_ids)
}
