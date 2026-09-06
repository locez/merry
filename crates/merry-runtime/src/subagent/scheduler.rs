use super::child::{
    error_info, publish_terminal_activity_snapshot, spawn_reserved_child,
    terminal_activity_for_agent, update_plan_link_with_scheduler,
};
use super::manager::{
    SubagentManagerState, enqueue_completion_notification_if_needed, initial_summary,
};
use super::{
    ChildRuntimeFactory, PlanLinkRuntime, SubagentActivityHub, SubagentStatusLabel,
    SubagentStatusView, SubagentTaskSpec,
};
use crate::{RuntimeError, TaskAnchor};
use merry_core::{ErrorInfo, PlanLinkSnapshot, PlanLinkStatus, SubagentId, SubagentTaskId};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct ReservedChildStart {
    pub(super) agent_id: SubagentId,
    pub(super) task_id: SubagentTaskId,
    pub(super) task: SubagentTaskSpec,
    pub(super) task_anchor: TaskAnchor,
    pub(super) cancellation_token: CancellationToken,
    pub(super) plan_link: Option<PlanLinkSnapshot>,
}

#[derive(Clone)]
pub(super) struct ChildScheduler {
    pub(super) factory: Arc<dyn ChildRuntimeFactory>,
    pub(super) state: Arc<Mutex<SubagentManagerState>>,
    pub(super) notify: Arc<Notify>,
    pub(super) completion_notifications: Arc<Mutex<VecDeque<SubagentStatusView>>>,
    pub(super) completion_notify: Arc<Notify>,
    pub(super) enabled: Arc<AtomicBool>,
    pub(super) max_threads: Arc<AtomicUsize>,
    pub(super) depth: u8,
    pub(super) plan_link_runtime: Arc<StdMutex<Option<Arc<dyn PlanLinkRuntime>>>>,
    pub(super) activity_hub: Arc<StdMutex<Option<Arc<SubagentActivityHub>>>>,
}

impl ChildScheduler {
    pub(super) fn effective_max_threads(&self) -> usize {
        if self.enabled.load(Ordering::Acquire) {
            self.max_threads.load(Ordering::Acquire)
        } else {
            0
        }
    }

    pub(super) fn attached_activity_hub(&self) -> Option<Arc<SubagentActivityHub>> {
        self.activity_hub
            .lock()
            .expect("subagent activity hub mutex is not poisoned")
            .clone()
    }

    pub(super) async fn enqueue_completion_notification(&self, status: SubagentStatusView) {
        enqueue_completion_notification_if_needed(
            &self.state,
            &self.completion_notifications,
            &self.completion_notify,
            status,
        )
        .await;
    }
}

pub(super) fn reserve_queued_starts_locked(
    state: &mut SubagentManagerState,
    max_threads: usize,
) -> Vec<ReservedChildStart> {
    let mut global_available = max_threads.saturating_sub(running_child_count(state));
    if global_available == 0 {
        return Vec::new();
    }

    let queued_ids = state
        .agents
        .values()
        .filter(|agent| agent.status == SubagentStatusLabel::Queued)
        .map(|agent| agent.agent_id.clone())
        .collect::<Vec<_>>();
    let mut starts = Vec::new();

    for agent_id in queued_ids {
        if global_available == 0 {
            break;
        }

        let Some(agent) = state.agents.get(&agent_id) else {
            continue;
        };
        let batch_id = agent.batch_id;
        let batch_max_concurrency = state
            .batches
            .get(&batch_id)
            .map(|batch| batch.max_concurrency)
            .unwrap_or(0);
        if batch_running_child_count(state, batch_id) >= batch_max_concurrency {
            continue;
        }

        let Some(agent) = state.agents.get_mut(&agent_id) else {
            continue;
        };
        agent.status = SubagentStatusLabel::Running;
        agent.summary = initial_summary(SubagentStatusLabel::Running);
        starts.push(ReservedChildStart {
            agent_id: agent.agent_id.clone(),
            task_id: agent.task_id.clone(),
            task: agent.task.clone(),
            task_anchor: agent.task_anchor.clone(),
            cancellation_token: agent.cancellation_token.clone(),
            plan_link: agent.plan_link.clone(),
        });
        global_available -= 1;
    }

    starts
}

pub(super) fn running_child_count(state: &SubagentManagerState) -> usize {
    state
        .agents
        .values()
        .filter(|agent| agent.status == SubagentStatusLabel::Running)
        .count()
}

pub(super) fn batch_running_child_count(state: &SubagentManagerState, batch_id: u64) -> usize {
    state
        .agents
        .values()
        .filter(|agent| agent.batch_id == batch_id && agent.status == SubagentStatusLabel::Running)
        .count()
}

pub(super) async fn start_reserved_children_iteratively(
    scheduler: ChildScheduler,
    starts: Vec<ReservedChildStart>,
) {
    let mut pending = VecDeque::from(starts);
    while let Some(start) = pending.pop_front() {
        match spawn_reserved_child(scheduler.clone(), &start).await {
            Ok(ReservedChildStartOutcome::Started) => {}
            Ok(ReservedChildStartOutcome::Cancelled) => {
                finish_reserved_child_start(
                    &scheduler,
                    &start,
                    &mut pending,
                    SubagentStatusLabel::Cancelled,
                    "child cancelled before runtime start",
                    error_info(
                        "subagent_cancelled",
                        "child cancellation requested before runtime start",
                    ),
                    PlanLinkStatus::Cancelled,
                )
                .await;
            }
            Err(error) => {
                let (summary, diagnostics) = error.failure_details();
                finish_reserved_child_start(
                    &scheduler,
                    &start,
                    &mut pending,
                    SubagentStatusLabel::Failed,
                    summary,
                    diagnostics,
                    PlanLinkStatus::Failed,
                )
                .await;
            }
        }
    }
}

pub(super) enum ReservedChildStartOutcome {
    Started,
    Cancelled,
}

pub(super) enum ReservedChildStartError {
    PlanScope(String),
    Factory(RuntimeError),
}

impl ReservedChildStartError {
    fn failure_details(&self) -> (&'static str, ErrorInfo) {
        match self {
            Self::PlanScope(error) => (
                "child Plan scope lookup failed",
                error_info("subagent_plan_scope_error", error),
            ),
            Self::Factory(error) => (
                "child runtime start failed",
                error_info("subagent_start_error", error.to_string()),
            ),
        }
    }
}

async fn finish_reserved_child_start(
    scheduler: &ChildScheduler,
    start: &ReservedChildStart,
    pending: &mut VecDeque<ReservedChildStart>,
    status: SubagentStatusLabel,
    summary: &'static str,
    diagnostics: ErrorInfo,
    link_status: PlanLinkStatus,
) {
    let mut state_guard = scheduler.state.lock().await;
    let mut link = None;
    let mut terminal_activity = None;
    let mut terminal_notification = None;
    if let Some(agent) = state_guard.agents.get_mut(&start.agent_id)
        && !agent.status.is_terminal()
    {
        agent.status = status;
        agent.summary = summary.to_owned();
        agent.diagnostics = Some(diagnostics);
        link = agent.plan_link.clone();
        terminal_activity = terminal_activity_for_agent(agent);
        terminal_notification = Some(agent.status_view());
    }
    pending.extend(reserve_queued_starts_locked(
        &mut state_guard,
        scheduler.effective_max_threads(),
    ));
    drop(state_guard);
    update_plan_link_with_scheduler(scheduler, link, link_status).await;
    if let Some((task_id, phase, summary)) = terminal_activity {
        publish_terminal_activity_snapshot(
            scheduler.attached_activity_hub(),
            start.agent_id.clone(),
            task_id,
            phase,
            &summary,
        );
    }
    if let Some(status) = terminal_notification {
        scheduler.enqueue_completion_notification(status).await;
    }
    scheduler.notify.notify_waiters();
}
