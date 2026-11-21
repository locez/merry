//! Terminal settlement, activity publication, and Plan-link updates for child loops.

use super::{
    super::{
        SubagentActivityHub, SubagentStatusLabel,
        activity::SubagentActivityReducer,
        manager::SubagentManagerState,
        manager_helpers::{plan_link_status_for_agent, terminal_activity_for_agent},
        scheduler::{
            ChildScheduler, ReservedChildStart, reserve_queued_starts_locked,
            start_reserved_children_iteratively,
        },
    },
    ChildLoopLaunch,
};
use merry_core::{
    ErrorInfo, PlanLinkSnapshot, PlanLinkStatus, SubagentActivityPhase, SubagentId, SubagentTaskId,
};
use std::sync::Arc;
use tokio::sync::Mutex;

struct ChildErrorTransition {
    claimed: bool,
    to_start: Vec<ReservedChildStart>,
}

pub(in crate::subagent) async fn finish_child_with_status(
    scheduler: ChildScheduler,
    launch: &ChildLoopLaunch,
    reducer: &mut SubagentActivityReducer,
    status: SubagentStatusLabel,
    summary: &str,
    diagnostics: ErrorInfo,
) {
    let transition = update_child_after_error(
        &scheduler.state,
        &launch.agent_id,
        status,
        summary,
        diagnostics,
        scheduler.effective_max_threads(),
    )
    .await;
    if transition.claimed {
        settle_child_terminal(
            scheduler,
            &launch.agent_id,
            reducer,
            launch.activity_hub.clone(),
            transition.to_start,
        )
        .await;
    } else {
        scheduler.notify.notify_waiters();
        start_reserved_children_iteratively(scheduler, transition.to_start).await;
    }
}

async fn settle_child_terminal(
    scheduler: ChildScheduler,
    agent_id: &SubagentId,
    reducer: &mut SubagentActivityReducer,
    activity_hub: Option<Arc<SubagentActivityHub>>,
    to_start: Vec<ReservedChildStart>,
) {
    let (link, link_status, terminal_activity, terminal_task, terminal_notification) = scheduler
        .state
        .lock()
        .await
        .agents
        .get(agent_id)
        .map(|agent| {
            (
                agent.plan_link.clone(),
                plan_link_status_for_agent(agent),
                terminal_activity_for_agent(agent),
                Some(agent.task.task().to_owned()),
                agent.status.is_terminal().then(|| agent.status_view()),
            )
        })
        .unwrap_or((None, PlanLinkStatus::Failed, None, None, None));
    update_plan_link_with_scheduler(&scheduler, link, link_status).await;
    if let Some((_task_id, phase, summary)) = terminal_activity {
        publish_terminal_activity(activity_hub.as_deref(), reducer, phase, &summary);
    }
    if let Some(status) = terminal_notification {
        let sink = scheduler.attached_event_sink();
        super::super::events::emit_terminal_event_and_persist(
            sink.as_ref(),
            terminal_task.unwrap_or_default(),
            &status,
        )
        .await;
        scheduler.enqueue_completion_notification(status).await;
    }
    scheduler.notify.notify_waiters();
    start_reserved_children_iteratively(scheduler, to_start).await;
}

pub(super) fn publish_terminal_activity(
    hub: Option<&SubagentActivityHub>,
    reducer: &mut SubagentActivityReducer,
    phase: SubagentActivityPhase,
    summary: &str,
) {
    if let Some(hub) = hub {
        hub.publish(reducer.terminal(phase, summary, crate::plan::unix_time_ms()));
    }
}

pub(in crate::subagent) fn publish_terminal_activity_snapshot(
    hub: Option<Arc<SubagentActivityHub>>,
    agent_id: SubagentId,
    task_id: SubagentTaskId,
    phase: SubagentActivityPhase,
    summary: &str,
) {
    let Some(hub) = hub else {
        return;
    };
    let mut reducer = SubagentActivityReducer::new(agent_id, task_id);
    publish_terminal_activity(Some(&hub), &mut reducer, phase, summary);
}

pub(in crate::subagent) async fn update_plan_link_with_scheduler(
    scheduler: &ChildScheduler,
    link: Option<PlanLinkSnapshot>,
    status: PlanLinkStatus,
) {
    let Some(link) = link else {
        return;
    };
    let runtime = scheduler
        .plan_link_runtime
        .lock()
        .expect("subagent plan link runtime mutex is not poisoned")
        .clone();
    let Some(runtime) = runtime else {
        return;
    };
    if let Err(error) = runtime
        .update_subagent_link(link.binding_id.clone(), status, crate::plan::unix_time_ms())
        .await
    {
        tracing::warn!(
            binding_id = %link.binding_id,
            ?status,
            %error,
            "failed to persist subagent Plan link state"
        );
    }
}

async fn update_child_after_error(
    state: &Mutex<SubagentManagerState>,
    agent_id: &SubagentId,
    status: SubagentStatusLabel,
    summary: &str,
    diagnostics: ErrorInfo,
    max_threads: usize,
) -> ChildErrorTransition {
    let mut state = state.lock().await;
    let mut claimed = false;
    if let Some(agent) = state.agents.get_mut(agent_id)
        && !agent.status.is_terminal()
    {
        agent.status = status;
        agent.summary = summary.to_owned();
        agent.diagnostics = Some(diagnostics);
        claimed = true;
    }
    ChildErrorTransition {
        claimed,
        to_start: reserve_queued_starts_locked(&mut state, max_threads),
    }
}
