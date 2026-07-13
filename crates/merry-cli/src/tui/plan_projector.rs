use super::state::TimelineItem;
use merry_core::{PlanNodeStatus, PlanPhase, PlanSnapshot};

pub(super) fn plan_timeline_item(
    previous: Option<&PlanSnapshot>,
    snapshot: &PlanSnapshot,
    summary: &str,
) -> Option<TimelineItem> {
    let active_node_count = snapshot
        .nodes
        .iter()
        .filter(|node| node.status != PlanNodeStatus::Superseded)
        .count();
    let Some(previous) = previous else {
        return Some(TimelineItem::Muted {
            title: "Plan mode".to_owned(),
            detail: format!(
                "{} · revision {} · {} nodes",
                plan_phase_label(snapshot.phase),
                snapshot.revision,
                active_node_count
            ),
        });
    };

    if previous.phase != snapshot.phase {
        let title = match snapshot.phase {
            PlanPhase::Planning => "Plan revision requested",
            PlanPhase::AwaitingApproval => "Plan awaiting approval",
            PlanPhase::Executing => "Plan execution started",
            PlanPhase::Completed => "Plan completed",
            PlanPhase::Blocked => "Plan blocked",
            PlanPhase::Cancelled => "Plan cancelled",
        };
        return Some(TimelineItem::Muted {
            title: title.to_owned(),
            detail: summary.to_owned(),
        });
    }

    if previous.scheduler_status != snapshot.scheduler_status {
        return Some(TimelineItem::Muted {
            title: format!("Plan scheduling {:?}", snapshot.scheduler_status).to_ascii_lowercase(),
            detail: summary.to_owned(),
        });
    }

    if previous.approval_requirements != snapshot.approval_requirements {
        return Some(TimelineItem::Muted {
            title: "Plan approval updated".to_owned(),
            detail: summary.to_owned(),
        });
    }

    let previous_active_node_count = previous
        .nodes
        .iter()
        .filter(|node| node.status != PlanNodeStatus::Superseded)
        .count();
    if active_node_count > previous_active_node_count {
        return Some(TimelineItem::Muted {
            title: "Plan expanded".to_owned(),
            detail: format!("{} · {} nodes", summary, active_node_count),
        });
    }

    if plan_definition_changed(previous, snapshot) {
        return Some(TimelineItem::Muted {
            title: "Plan revised".to_owned(),
            detail: summary.to_owned(),
        });
    }

    let newly_unhealthy = snapshot.nodes.iter().find(|node| {
        matches!(
            node.status,
            PlanNodeStatus::Blocked | PlanNodeStatus::Failed
        ) && previous
            .nodes
            .iter()
            .find(|candidate| candidate.id == node.id)
            .is_none_or(|candidate| candidate.status != node.status)
    });
    newly_unhealthy.map(|node| TimelineItem::Diagnostic {
        title: format!("Plan node {}", plan_node_status_label(node.status)),
        body: node.objective.clone(),
    })
}

fn plan_definition_changed(previous: &PlanSnapshot, snapshot: &PlanSnapshot) -> bool {
    previous.root_node_id != snapshot.root_node_id
        || previous.coordinator_node_id != snapshot.coordinator_node_id
        || previous.max_concurrency_hint != snapshot.max_concurrency_hint
        || snapshot.nodes.iter().any(|node| {
            previous
                .nodes
                .iter()
                .find(|candidate| candidate.id == node.id)
                .is_none_or(|candidate| {
                    candidate.parent_id != node.parent_id
                        || candidate.sibling_order != node.sibling_order
                        || candidate.objective != node.objective
                        || candidate.acceptance != node.acceptance
                        || candidate.executor_policy != node.executor_policy
                        || candidate.harness != node.harness
                        || candidate.recovery_policy != node.recovery_policy
                        || candidate.depends_on != node.depends_on
                })
        })
}

fn plan_phase_label(phase: PlanPhase) -> &'static str {
    match phase {
        PlanPhase::Planning => "planning",
        PlanPhase::AwaitingApproval => "awaiting approval",
        PlanPhase::Executing => "executing",
        PlanPhase::Completed => "completed",
        PlanPhase::Blocked => "blocked",
        PlanPhase::Cancelled => "cancelled",
    }
}

fn plan_node_status_label(status: PlanNodeStatus) -> &'static str {
    match status {
        PlanNodeStatus::Blocked => "blocked",
        PlanNodeStatus::Failed => "failed",
        _ => "updated",
    }
}
