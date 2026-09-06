use crate::{
    FileSessionStore,
    plan::{
        PlanError,
        controller::{
            PlanCommandResult, PlanControllerError,
            transactions::{
                persistence::{SessionBase, persist_and_install, prepare_plan_commit},
                plan_updated_payload,
            },
        },
        protocol::{PlanUpdateOutput, SubagentPlanUpdateInput},
    },
    session::SessionState,
};
use merry_core::{
    PlanBindingId, PlanId, PlanLinkSnapshot, PlanLinkStatus, PlanNodeId, PlanNodeStatus, PlanPhase,
    PlanRevisionSummary, PlanSchedulerStatus, PlanSnapshot, RuntimeJournalEvent,
    RuntimeJournalPayload, SubagentId, SubagentTaskId,
};
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

pub(in crate::plan::controller) async fn update_subagent(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    plan_id: PlanId,
    root_node_id: PlanNodeId,
    binding_id: PlanBindingId,
    input: SubagentPlanUpdateInput,
) -> Result<PlanCommandResult<PlanUpdateOutput>, PlanControllerError> {
    let (base, output, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let output = candidate.update_subagent(plan_id, root_node_id, binding_id, input)?;
        let payloads = vec![plan_updated_payload(&output.snapshot)];
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, output, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output,
        events: committed_events,
    })
}

pub(in crate::plan::controller) async fn bind_subagent(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    client_key: String,
    agent_id: SubagentId,
    task_id: SubagentTaskId,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanLinkSnapshot>, PlanControllerError> {
    let (base, link, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let revision = candidate.advance_revision("runtime linked a subagent to this Plan task")?;
        let plan_id = candidate.snapshot.plan_id.clone();
        let node_id = candidate
            .snapshot
            .nodes
            .iter()
            .find(|node| node.client_key.as_deref() == Some(client_key.as_str()))
            .map(|node| node.id.clone())
            .ok_or(PlanControllerError::Plan {
                source: PlanError::UnknownClientKey { client_key },
            })?;
        let mut ancestor_ids = Vec::new();
        let mut ancestor_id = candidate
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .and_then(|node| node.parent_id.clone());
        while let Some(current_id) = ancestor_id {
            ancestor_id = candidate
                .snapshot
                .nodes
                .iter()
                .find(|candidate| candidate.id == current_id)
                .and_then(|candidate| candidate.parent_id.clone());
            ancestor_ids.push(current_id);
        }
        let node = candidate
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
            .expect("plan node remains present");
        // The plan revision is monotonic across the whole plan, unlike the
        // per-node link count. Use it to keep binding identities unique when
        // several nodes each receive their first delegated child.
        let binding_id = PlanBindingId::new(&format!("plan-binding-{revision}"))
            .expect("runtime-generated plan binding id is valid");
        let link = PlanLinkSnapshot {
            plan_id,
            node_id: node_id.clone(),
            binding_id: binding_id.clone(),
            subagent_id: agent_id,
            task_id,
            status: PlanLinkStatus::Active,
            linked_at_ms: now_ms,
            terminal_at_ms: None,
            superseded_by: None,
        };
        // A new binding after a terminal link is a retry/replacement for this
        // Plan node. Keep the old lifecycle record for auditability, but stop
        // it from contributing a stale failure to the live projection.
        for existing in &mut node.links {
            if existing.superseded_by.is_none()
                && matches!(
                    existing.status,
                    PlanLinkStatus::Completed
                        | PlanLinkStatus::Failed
                        | PlanLinkStatus::Cancelled
                        | PlanLinkStatus::Blocked
                )
            {
                existing.status = PlanLinkStatus::Superseded;
                existing.superseded_by = Some(binding_id.clone());
            }
        }
        node.links.push(link.clone());
        node.updated_revision = revision;
        recompute_link_projection(node);
        for ancestor_id in ancestor_ids {
            let ancestor = candidate
                .snapshot
                .nodes
                .iter_mut()
                .find(|candidate| candidate.id == ancestor_id)
                .expect("plan ancestor remains present");
            if matches!(
                ancestor.status,
                PlanNodeStatus::Blocked | PlanNodeStatus::Failed
            ) {
                ancestor.status = PlanNodeStatus::Expanded;
                ancestor.updated_revision = revision;
            }
        }
        if candidate.snapshot.phase == PlanPhase::Blocked {
            candidate.snapshot.phase = PlanPhase::Executing;
            candidate.snapshot.scheduler_status = PlanSchedulerStatus::Active;
        }
        candidate.refresh_parent_states(revision);
        let summary =
            PlanRevisionSummary::new(revision, "runtime linked a subagent to this Plan task")
                .map_err(|_| PlanControllerError::Plan {
                    source: PlanError::InvalidText {
                        field: "summary",
                        reason: "is invalid",
                    },
                })?;
        let payloads = vec![RuntimeJournalPayload::PlanUpdated {
            snapshot: candidate.snapshot.clone(),
            summary,
        }];
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, link, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output: link,
        events: committed_events,
    })
}

pub(in crate::plan::controller) async fn update_subagent_link(
    session: &Arc<Mutex<SessionState>>,
    store: Option<&FileSessionStore>,
    events: &broadcast::Sender<RuntimeJournalEvent>,
    binding_id: PlanBindingId,
    status: PlanLinkStatus,
    now_ms: u64,
) -> Result<PlanCommandResult<PlanSnapshot>, PlanControllerError> {
    let (base, snapshot, prepared) = {
        let session = session.lock().await;
        let mut candidate = session
            .active_plan()
            .ok_or(PlanControllerError::NoActivePlan)?
            .clone();
        let revision = candidate.advance_revision("runtime updated linked subagent state")?;
        let node = candidate
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.links.iter().any(|link| link.binding_id == binding_id))
            .ok_or_else(|| PlanControllerError::Plan {
                source: PlanError::UnknownNode {
                    node_id: PlanNodeId::new("linked-node-not-found")
                        .expect("valid diagnostic node id"),
                },
            })?;
        let link = node
            .links
            .iter_mut()
            .find(|link| link.binding_id == binding_id)
            .expect("link lookup above succeeded");
        link.status = status;
        link.terminal_at_ms = matches!(
            status,
            PlanLinkStatus::Completed
                | PlanLinkStatus::Failed
                | PlanLinkStatus::Cancelled
                | PlanLinkStatus::Blocked
                | PlanLinkStatus::Superseded
        )
        .then_some(now_ms);
        node.updated_revision = revision;
        recompute_link_projection(node);
        candidate.refresh_parent_states(revision);
        candidate.refresh_terminal_phase();
        let summary = PlanRevisionSummary::new(revision, "runtime updated linked subagent state")
            .map_err(|_| PlanControllerError::Plan {
            source: PlanError::InvalidText {
                field: "summary",
                reason: "is invalid",
            },
        })?;
        let payloads = vec![RuntimeJournalPayload::PlanUpdated {
            snapshot: candidate.snapshot.clone(),
            summary,
        }];
        let snapshot = candidate.snapshot.clone();
        let base = SessionBase::capture(&session);
        let prepared = prepare_plan_commit(
            &session,
            candidate,
            session.terminal_plans().to_vec(),
            payloads,
            None,
        )?;
        (base, snapshot, prepared)
    };
    let committed_events = persist_and_install(session, store, events, base, prepared).await?;
    Ok(PlanCommandResult {
        output: snapshot,
        events: committed_events,
    })
}

pub(super) fn recompute_link_projection(node: &mut merry_core::PlanNodeSnapshot) {
    let mut summary = merry_core::PlanExecutionSummary::default();
    for link in node
        .links
        .iter()
        .filter(|link| link.status != PlanLinkStatus::Superseded && link.superseded_by.is_none())
    {
        match link.status {
            PlanLinkStatus::Active => summary.active += 1,
            PlanLinkStatus::Completed => summary.completed += 1,
            PlanLinkStatus::Failed => summary.failed += 1,
            PlanLinkStatus::Cancelled => summary.cancelled += 1,
            PlanLinkStatus::Blocked => summary.blocked += 1,
            PlanLinkStatus::Superseded => {}
        }
    }
    node.execution_summary = summary.clone();
    node.status = if summary.active > 0 {
        PlanNodeStatus::InProgress
    } else if summary.failed > 0 || summary.cancelled > 0 {
        PlanNodeStatus::Failed
    } else if summary.blocked > 0 {
        PlanNodeStatus::Blocked
    } else if summary.completed > 0 {
        PlanNodeStatus::Completed
    } else {
        node.declared_status
    };
}
