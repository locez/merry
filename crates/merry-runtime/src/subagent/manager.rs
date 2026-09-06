use super::child::{error_info, publish_terminal_activity_snapshot};
use super::protocol::{
    RejectedSubagentView, SpawnSubagentsOutput, SpawnedSubagentStatusLabel, SpawnedSubagentView,
    WaitMode, WaitSubagentsOutput,
};
use super::scheduler::{self, ChildScheduler, ReservedChildStart};
use super::scheduler::{
    batch_running_child_count, reserve_queued_starts_locked, running_child_count,
};
use super::scope::ParentCapabilities;
use super::spec::{validate_no_write_scope_conflicts, validate_task_max_model_turns};
use super::{
    ChildRuntimeFactory, ChildWorkspaceScope, PlanLinkRuntime, SubagentActivityHub, SubagentConfig,
    SubagentError, SubagentResultView, SubagentStatusLabel, SubagentStatusView, SubagentTaskSpec,
};
use crate::{RuntimeError, TaskAnchor};
use merry_core::{
    ErrorInfo, PlanLinkSnapshot, PlanLinkStatus, SubagentActivityPhase, SubagentId, SubagentTaskId,
    ToolName,
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

/// Runtime-owned manager for bounded child agent execution.
#[derive(Clone)]
pub struct SubagentManager {
    pub(super) enabled: Arc<AtomicBool>,
    pub(super) max_threads: Arc<AtomicUsize>,
    pub(super) min_model_turns: Arc<AtomicU32>,
    pub(super) max_model_turns: Arc<AtomicU32>,
    pub(super) has_agents: Arc<AtomicBool>,
    pub(super) factory: Arc<dyn ChildRuntimeFactory>,
    pub(super) state: Arc<Mutex<SubagentManagerState>>,
    pub(super) notify: Arc<Notify>,
    pub(super) completion_notifications: Arc<Mutex<VecDeque<SubagentStatusView>>>,
    pub(super) completion_notify: Arc<Notify>,
    pub(super) next_id: Arc<AtomicU64>,
    pub(super) next_batch_id: Arc<AtomicU64>,
    pub(super) depth: u8,
    pub(super) max_depth: u8,
    pub(super) plan_link_runtime: Arc<StdMutex<Option<Arc<dyn PlanLinkRuntime>>>>,
    pub(super) parent_capabilities: Arc<StdMutex<Option<ParentCapabilities>>>,
    pub(super) activity_hub: Arc<StdMutex<Option<Arc<SubagentActivityHub>>>>,
}

#[derive(Debug, Default)]
pub(super) struct SubagentManagerState {
    pub(super) agents: BTreeMap<SubagentId, ManagedSubagent>,
    pub(super) batches: BTreeMap<u64, SubagentBatch>,
}

#[derive(Debug, Clone)]
pub(super) struct SubagentBatch {
    pub(super) max_concurrency: usize,
}
pub(super) async fn enqueue_completion_notification_if_needed(
    state: &Arc<Mutex<SubagentManagerState>>,
    completion_notifications: &Arc<Mutex<VecDeque<SubagentStatusView>>>,
    completion_notify: &Arc<Notify>,
    status: SubagentStatusView,
) {
    // Queue and acknowledgement paths all take these locks in this order. The
    // state check closes the race where wait() observes a terminal child just
    // before its completion notification is enqueued.
    let mut notifications = completion_notifications.lock().await;
    let state_guard = state.lock().await;
    if state_guard
        .agents
        .get(&status.agent_id)
        .is_some_and(|agent| agent.completion_notification_acknowledged)
    {
        return;
    }
    notifications.push_back(status);
    drop(state_guard);
    drop(notifications);
    completion_notify.notify_one();
}
#[derive(Debug, Clone)]
pub(super) struct ManagedSubagent {
    pub(super) batch_id: u64,
    pub(super) agent_id: SubagentId,
    pub(super) task_id: SubagentTaskId,
    pub(super) task: SubagentTaskSpec,
    pub(super) task_anchor: TaskAnchor,
    pub(super) status: SubagentStatusLabel,
    pub(super) summary: String,
    pub(super) result: Option<SubagentResultView>,
    pub(super) output_paths: Vec<String>,
    pub(super) changed_paths: Vec<String>,
    pub(super) diagnostics: Option<ErrorInfo>,
    pub(super) cancellation_token: CancellationToken,
    pub(super) plan_link: Option<PlanLinkSnapshot>,
    pub(super) completion_notification_acknowledged: bool,
}

impl SubagentManager {
    /// Creates a subagent manager for one parent session.
    #[must_use]
    pub fn new(
        parent_session_id: merry_core::SessionId,
        config: SubagentConfig,
        factory: Arc<dyn ChildRuntimeFactory>,
    ) -> Self {
        Self::runtime_controlled(parent_session_id, config, factory, true)
    }

    /// Creates a manager whose spawn policy can be changed by interactive runtime control.
    #[must_use]
    pub fn runtime_controlled(
        parent_session_id: merry_core::SessionId,
        config: SubagentConfig,
        factory: Arc<dyn ChildRuntimeFactory>,
        enabled: bool,
    ) -> Self {
        Self::runtime_controlled_at_depth(parent_session_id, config, factory, enabled, 0)
    }

    /// Creates a manager for a child runtime at a known delegation depth.
    #[must_use]
    pub fn runtime_controlled_at_depth(
        _parent_session_id: merry_core::SessionId,
        config: SubagentConfig,
        factory: Arc<dyn ChildRuntimeFactory>,
        enabled: bool,
        depth: u8,
    ) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            max_threads: Arc::new(AtomicUsize::new(config.max_threads())),
            min_model_turns: Arc::new(AtomicU32::new(config.min_model_turns())),
            max_model_turns: Arc::new(AtomicU32::new(config.max_model_turns())),
            has_agents: Arc::new(AtomicBool::new(false)),
            factory,
            state: Arc::new(Mutex::new(SubagentManagerState::default())),
            notify: Arc::new(Notify::new()),
            completion_notifications: Arc::new(Mutex::new(VecDeque::new())),
            completion_notify: Arc::new(Notify::new()),
            next_id: Arc::new(AtomicU64::new(1)),
            next_batch_id: Arc::new(AtomicU64::new(1)),
            depth,
            max_depth: config.max_depth(),
            plan_link_runtime: Arc::new(StdMutex::new(None)),
            parent_capabilities: Arc::new(StdMutex::new(None)),
            activity_hub: Arc::new(StdMutex::new(None)),
        }
    }

    pub(crate) fn attach_parent_capabilities(
        &self,
        allowed_tools: Vec<ToolName>,
        workspace_scope: ChildWorkspaceScope,
    ) {
        *self
            .parent_capabilities
            .lock()
            .expect("subagent parent capabilities mutex is not poisoned") =
            Some(ParentCapabilities {
                allowed_tools,
                workspace_scope,
            });
    }

    pub(crate) fn attach_plan_link_runtime(&self, runtime: Arc<dyn PlanLinkRuntime>) {
        *self
            .plan_link_runtime
            .lock()
            .expect("subagent plan link runtime mutex is not poisoned") = Some(runtime);
    }

    pub(super) fn attached_plan_link_runtime(&self) -> Option<Arc<dyn PlanLinkRuntime>> {
        self.plan_link_runtime
            .lock()
            .expect("subagent plan link runtime mutex is not poisoned")
            .clone()
    }

    pub(crate) fn attach_activity_hub(&self, hub: Arc<SubagentActivityHub>) {
        *self
            .activity_hub
            .lock()
            .expect("subagent activity hub mutex is not poisoned") = Some(hub);
    }

    pub(super) fn attached_activity_hub(&self) -> Option<Arc<SubagentActivityHub>> {
        self.activity_hub
            .lock()
            .expect("subagent activity hub mutex is not poisoned")
            .clone()
    }

    pub(super) fn apply_parent_capabilities(
        &self,
        mut task: SubagentTaskSpec,
    ) -> Result<SubagentTaskSpec, SubagentError> {
        let capabilities = self
            .parent_capabilities
            .lock()
            .expect("subagent parent capabilities mutex is not poisoned")
            .clone();
        let Some(capabilities) = capabilities else {
            return Ok(task);
        };

        if task.allowed_tools_are_explicit() {
            for tool in task.allowed_tools() {
                if !capabilities.allowed_tools.contains(tool) {
                    return Err(SubagentError::CapabilityExpansion {
                        field: "allowed_tools",
                        value: tool.to_string(),
                    });
                }
            }
        } else {
            task = task.with_allowed_tools(capabilities.allowed_tools);
        }

        if task.read_scope_is_explicit() {
            ensure_scope_within_parent(
                "read_scope",
                task.read_scope(),
                capabilities.workspace_scope.read_scope(),
            )?;
        } else {
            task = task.with_read_scope(capabilities.workspace_scope.read_scope().to_vec())?;
        }

        if task.write_scope_is_explicit() {
            ensure_scope_within_parent(
                "write_scope",
                task.write_scope(),
                capabilities.workspace_scope.write_scope(),
            )?;
        } else {
            task = task.with_write_scope(capabilities.workspace_scope.write_scope().to_vec())?;
        }

        let inherited_forbidden = capabilities.workspace_scope.forbidden_paths();
        if task.forbidden_paths_are_explicit() {
            let mut forbidden = inherited_forbidden.to_vec();
            forbidden.extend(task.forbidden_paths().iter().cloned());
            forbidden.sort();
            forbidden.dedup();
            task = task.with_forbidden_paths(forbidden)?;
        } else {
            task = task.with_forbidden_paths(inherited_forbidden.to_vec())?;
        }

        Ok(task)
    }

    pub(crate) async fn update_policy(
        &self,
        enabled: bool,
        config: SubagentConfig,
    ) -> Result<(), RuntimeError> {
        self.max_threads
            .store(config.max_threads(), Ordering::Release);
        self.min_model_turns
            .store(config.min_model_turns(), Ordering::Release);
        self.max_model_turns
            .store(config.max_model_turns(), Ordering::Release);
        self.enabled.store(enabled, Ordering::Release);
        if enabled {
            let mut state = self.state.lock().await;
            let starts = self.reserve_queued_starts_locked(&mut state);
            drop(state);
            self.start_reserved_children(starts).await?;
        }
        Ok(())
    }

    fn effective_max_threads(&self) -> usize {
        if self.enabled.load(Ordering::Acquire) {
            self.max_threads.load(Ordering::Acquire)
        } else {
            0
        }
    }

    /// Returns compact status views for all managed children.
    pub async fn snapshot(&self) -> Vec<SubagentStatusView> {
        let state = self.state.lock().await;
        state
            .agents
            .values()
            .map(ManagedSubagent::status_view)
            .collect()
    }

    /// Returns the configured minimum model-turn budget for explicit child task
    /// inputs.
    pub(crate) fn min_model_turns(&self) -> u32 {
        self.min_model_turns.load(Ordering::Acquire)
    }

    /// Returns the configured maximum model-turn budget for explicit child task
    /// inputs and the default for omitted budgets.
    pub(crate) fn max_model_turns(&self) -> u32 {
        self.max_model_turns.load(Ordering::Acquire)
    }

    /// Returns the notification primitive used to wake a parent continuation
    /// when a child reaches a terminal state.
    pub(crate) fn completion_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.completion_notify)
    }

    /// Drains terminal child notifications for delivery to the parent model.
    pub(crate) async fn take_completion_notifications(&self) -> Vec<SubagentStatusView> {
        let mut notifications = self.completion_notifications.lock().await;
        let statuses = notifications.drain(..).collect::<Vec<_>>();
        if statuses.is_empty() {
            return statuses;
        }

        let mut state = self.state.lock().await;
        for status in &statuses {
            if let Some(agent) = state.agents.get_mut(&status.agent_id) {
                agent.completion_notification_acknowledged = true;
            }
        }
        statuses
    }

    pub(crate) async fn has_completion_notifications(&self) -> bool {
        !self.completion_notifications.lock().await.is_empty()
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

    async fn acknowledge_completion_notifications(&self, agent_ids: &[SubagentId]) {
        let mut notifications = self.completion_notifications.lock().await;
        notifications.retain(|status| !agent_ids.contains(&status.agent_id));

        let mut state = self.state.lock().await;
        for agent_id in agent_ids {
            if let Some(agent) = state.agents.get_mut(agent_id) {
                agent.completion_notification_acknowledged = true;
            }
        }
    }

    /// Accepts a batch of child tasks, starting only the initial bounded slice.
    pub async fn spawn(
        &self,
        tasks: Vec<SubagentTaskSpec>,
        max_concurrency: Option<usize>,
        parent_token: CancellationToken,
    ) -> Result<SpawnSubagentsOutput, RuntimeError> {
        if self.depth >= self.max_depth {
            return Ok(SpawnSubagentsOutput {
                spawned: Vec::new(),
                rejected: (0..tasks.len())
                    .map(|task_index| RejectedSubagentView {
                        task_index,
                        reason: "maximum subagent delegation depth reached".to_owned(),
                    })
                    .collect(),
            });
        }
        if !self.enabled.load(Ordering::Acquire) {
            return Ok(SpawnSubagentsOutput {
                spawned: Vec::new(),
                rejected: (0..tasks.len())
                    .map(|task_index| RejectedSubagentView {
                        task_index,
                        reason: "subagent spawning is disabled".to_owned(),
                    })
                    .collect(),
            });
        }
        let mut inherited_tasks = Vec::with_capacity(tasks.len());
        let mut rejected = Vec::new();
        for (task_index, task) in tasks.into_iter().enumerate() {
            if let Err(error) = validate_task_max_model_turns(
                task.max_model_turns(),
                self.min_model_turns(),
                self.max_model_turns(),
            ) {
                rejected.push(RejectedSubagentView {
                    task_index,
                    reason: error.to_string(),
                });
                continue;
            }
            match self.apply_parent_capabilities(task) {
                Ok(task) => inherited_tasks.push((task_index, task)),
                Err(error) => rejected.push(RejectedSubagentView {
                    task_index,
                    reason: error.to_string(),
                }),
            }
        }
        let conflict_tasks = inherited_tasks
            .iter()
            .map(|(_, task)| task.clone())
            .collect::<Vec<_>>();
        if let Err(error) = validate_no_write_scope_conflicts(&conflict_tasks) {
            let reason = error.to_string();
            rejected.extend(
                inherited_tasks
                    .iter()
                    .map(|(task_index, _)| RejectedSubagentView {
                        task_index: *task_index,
                        reason: reason.clone(),
                    }),
            );
            return Ok(SpawnSubagentsOutput {
                spawned: Vec::new(),
                rejected,
            });
        }

        let task_inputs = inherited_tasks
            .into_iter()
            .map(|(task_index, task)| {
                TaskAnchor::new(task.task())
                    .map(|task_anchor| (task_index, task, task_anchor))
                    .map_err(RuntimeError::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut accepted_tasks = Vec::with_capacity(task_inputs.len());
        let batch_id = self.next_batch_id.fetch_add(1, Ordering::SeqCst);
        for (task_index, task, task_anchor) in task_inputs {
            let number = self.next_id.fetch_add(1, Ordering::SeqCst);
            let agent_id = SubagentId::new(&format!("agent-{number}"))?;
            let task_id = SubagentTaskId::new(&format!("task-{number}"))?;
            let plan_link = match (task.plan_client_key(), self.attached_plan_link_runtime()) {
                (Some(client_key), Some(runtime)) => match runtime
                    .bind_subagent(
                        client_key.to_owned(),
                        agent_id.clone(),
                        task_id.clone(),
                        crate::plan::unix_time_ms(),
                    )
                    .await
                {
                    Ok(link) => Some(link),
                    Err(error) => {
                        rejected.push(RejectedSubagentView {
                            task_index,
                            reason: format!("Plan task binding failed: {error}"),
                        });
                        continue;
                    }
                },
                (Some(_), None) => {
                    rejected.push(RejectedSubagentView {
                        task_index,
                        reason: "Plan task binding is unavailable for this runtime".to_owned(),
                    });
                    continue;
                }
                (None, _) => None,
            };
            accepted_tasks.push((agent_id, task_id, task, task_anchor, plan_link));
        }

        if accepted_tasks.is_empty() {
            return Ok(SpawnSubagentsOutput {
                spawned: Vec::new(),
                rejected,
            });
        }

        self.has_agents.store(true, Ordering::Release);
        let batch_max_concurrency = max_concurrency
            .unwrap_or(accepted_tasks.len())
            .min(accepted_tasks.len());
        let mut spawned = Vec::with_capacity(accepted_tasks.len());
        let mut to_start = Vec::new();
        let mut state = self.state.lock().await;
        state.batches.insert(
            batch_id,
            SubagentBatch {
                max_concurrency: batch_max_concurrency,
            },
        );

        for (agent_id, task_id, task, task_anchor, plan_link) in accepted_tasks {
            let child_token = parent_token.child_token();
            let starts_now = running_child_count(&state) < self.effective_max_threads()
                && batch_running_child_count(&state, batch_id) < batch_max_concurrency;
            let managed_status = if starts_now {
                SubagentStatusLabel::Running
            } else {
                SubagentStatusLabel::Queued
            };

            let managed = ManagedSubagent {
                batch_id,
                agent_id: agent_id.clone(),
                task_id: task_id.clone(),
                task: task.clone(),
                task_anchor: task_anchor.clone(),
                status: managed_status.clone(),
                summary: initial_summary(managed_status),
                result: None,
                output_paths: Vec::new(),
                changed_paths: Vec::new(),
                diagnostics: None,
                cancellation_token: child_token.clone(),
                plan_link: plan_link.clone(),
                completion_notification_acknowledged: false,
            };
            state.agents.insert(agent_id.clone(), managed);

            spawned.push(SpawnedSubagentView {
                agent_id: agent_id.clone(),
                task_id: task_id.clone(),
                display_name: task.display_name().map(str::to_owned),
                status: if starts_now {
                    SpawnedSubagentStatusLabel::Running
                } else {
                    SpawnedSubagentStatusLabel::Queued
                },
                task_anchor: task.task().to_owned(),
                read_scope: paths_to_strings(task.read_scope()),
                write_scope: paths_to_strings(task.write_scope()),
            });

            if starts_now {
                to_start.push((agent_id, task_id, task, task_anchor, child_token, plan_link));
            }
        }
        drop(state);

        for (agent_id, task_id, task, task_anchor, child_token, plan_link) in to_start {
            self.start_child(agent_id, task_id, task, task_anchor, child_token, plan_link)
                .await;
        }

        Ok(SpawnSubagentsOutput {
            spawned,
            rejected: Vec::new(),
        })
    }

    /// Waits for selected children or returns their latest compact statuses on timeout.
    pub async fn wait(
        &self,
        agent_ids: &[SubagentId],
        mode: WaitMode,
        timeout: Option<Duration>,
    ) -> Result<WaitSubagentsOutput, RuntimeError> {
        if agent_ids.is_empty() {
            return Err(RuntimeError::InvalidSubagentSelection {
                operation: "wait_subagents",
            });
        }
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let agents = self.status_for(agent_ids).await;
            let ready = wait_mode_satisfied(mode, &agents);
            if ready {
                self.acknowledge_waited_statuses(&agents).await;
                return Ok(WaitSubagentsOutput::with_wait_state(agents, true, false));
            }

            match deadline {
                Some(deadline) => {
                    if tokio::time::Instant::now() >= deadline {
                        self.acknowledge_waited_statuses(&agents).await;
                        return Ok(WaitSubagentsOutput::with_wait_state(agents, false, true));
                    }
                    if tokio::time::timeout_at(deadline, notified.as_mut())
                        .await
                        .is_err()
                    {
                        let agents = self.status_for(agent_ids).await;
                        self.acknowledge_waited_statuses(&agents).await;
                        return Ok(WaitSubagentsOutput::with_wait_state(agents, false, true));
                    }
                }
                None => notified.as_mut().await,
            }
        }
    }

    /// Cancels selected children and returns their compact statuses.
    pub async fn cancel(
        &self,
        agent_ids: &[SubagentId],
    ) -> Result<WaitSubagentsOutput, RuntimeError> {
        if agent_ids.is_empty() {
            return Err(RuntimeError::InvalidSubagentSelection {
                operation: "cancel_subagents",
            });
        }
        let mut state = self.state.lock().await;
        let mut links_to_update = Vec::new();
        let mut terminal_activities = Vec::new();
        for agent_id in agent_ids {
            if let Some(agent) = state.agents.get_mut(agent_id) {
                if agent.status.is_terminal() {
                    continue;
                }
                agent.cancellation_token.cancel();
                agent.status = SubagentStatusLabel::Cancelled;
                agent.summary = "child cancelled by parent".to_owned();
                agent.diagnostics = Some(error_info(
                    "subagent_cancelled",
                    "child cancellation requested by parent",
                ));
                if let Some(link) = agent.plan_link.clone() {
                    links_to_update.push(link);
                }
                terminal_activities.push((
                    agent.agent_id.clone(),
                    agent.task_id.clone(),
                    SubagentActivityPhase::Cancelled,
                    agent.summary.clone(),
                ));
            }
        }
        let agents = selected_statuses(&state, agent_ids);
        let to_start = self.reserve_queued_starts_locked(&mut state);
        drop(state);
        self.update_plan_links(links_to_update, PlanLinkStatus::Cancelled)
            .await;
        let activity_hub = self.attached_activity_hub();
        for (agent_id, task_id, phase, summary) in terminal_activities {
            publish_terminal_activity_snapshot(
                activity_hub.clone(),
                agent_id,
                task_id,
                phase,
                &summary,
            );
        }
        self.notify.notify_waiters();
        self.start_reserved_children(to_start).await?;
        Ok(WaitSubagentsOutput::new(agents))
    }

    pub(super) fn reserve_queued_starts_locked(
        &self,
        state: &mut SubagentManagerState,
    ) -> Vec<ReservedChildStart> {
        reserve_queued_starts_locked(state, self.effective_max_threads())
    }

    pub(super) fn child_scheduler(&self) -> ChildScheduler {
        ChildScheduler {
            factory: Arc::clone(&self.factory),
            state: Arc::clone(&self.state),
            notify: Arc::clone(&self.notify),
            completion_notifications: Arc::clone(&self.completion_notifications),
            completion_notify: Arc::clone(&self.completion_notify),
            enabled: Arc::clone(&self.enabled),
            max_threads: Arc::clone(&self.max_threads),
            depth: self.depth,
            plan_link_runtime: Arc::clone(&self.plan_link_runtime),
            activity_hub: Arc::clone(&self.activity_hub),
        }
    }

    pub(super) async fn start_reserved_children(
        &self,
        starts: Vec<ReservedChildStart>,
    ) -> Result<(), RuntimeError> {
        scheduler::start_reserved_children_iteratively(self.child_scheduler(), starts).await;
        Ok(())
    }

    pub(super) async fn status_for(&self, agent_ids: &[SubagentId]) -> Vec<SubagentStatusView> {
        let state = self.state.lock().await;
        selected_statuses(&state, agent_ids)
    }

    pub(super) async fn acknowledge_waited_statuses(&self, agents: &[SubagentStatusView]) {
        let agent_ids = agents
            .iter()
            .filter(|agent| agent.is_terminal())
            .map(|agent| agent.agent_id.clone())
            .collect::<Vec<_>>();
        self.acknowledge_completion_notifications(&agent_ids).await;
    }

    pub(super) async fn update_plan_links(
        &self,
        links: Vec<PlanLinkSnapshot>,
        status: PlanLinkStatus,
    ) {
        let Some(runtime) = self.attached_plan_link_runtime() else {
            return;
        };
        for link in links {
            let binding_id = link.binding_id.clone();
            if let Err(error) = runtime
                .update_subagent_link(binding_id.clone(), status, crate::plan::unix_time_ms())
                .await
            {
                tracing::warn!(
                    binding_id = %binding_id,
                    ?status,
                    %error,
                    "failed to persist subagent Plan link state"
                );
            }
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) async fn cancellation_token_for_test(
        &self,
        agent_id: &SubagentId,
    ) -> Option<CancellationToken> {
        let state = self.state.lock().await;
        state
            .agents
            .get(agent_id)
            .map(|agent| agent.cancellation_token.clone())
    }
}

fn wait_mode_satisfied(mode: WaitMode, agents: &[SubagentStatusView]) -> bool {
    match mode {
        WaitMode::Any => agents.iter().any(SubagentStatusView::is_terminal),
        WaitMode::All => agents.iter().all(SubagentStatusView::is_terminal),
    }
}

impl ManagedSubagent {
    pub(super) fn status_view(&self) -> SubagentStatusView {
        let summary = if self.summary.is_empty() {
            fallback_summary(self.status.clone(), &self.task)
        } else {
            self.summary.clone()
        };

        SubagentStatusView {
            agent_id: self.agent_id.clone(),
            task_id: self.task_id.clone(),
            status: self.status.clone(),
            summary,
            result: self.result.clone(),
            output_paths: self.output_paths.clone(),
            changed_paths: self.changed_paths.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }
}

pub(super) fn selected_statuses(
    state: &SubagentManagerState,
    agent_ids: &[SubagentId],
) -> Vec<SubagentStatusView> {
    state
        .agents
        .values()
        .filter(|agent| agent_ids.contains(&agent.agent_id))
        .map(ManagedSubagent::status_view)
        .collect()
}

fn paths_to_strings(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn ensure_scope_within_parent(
    field: &'static str,
    requested: &[PathBuf],
    parent: &[PathBuf],
) -> Result<(), SubagentError> {
    for path in requested {
        if !parent
            .iter()
            .any(|root| root == Path::new(".") || path == root || path.starts_with(root))
        {
            return Err(SubagentError::CapabilityExpansion {
                field,
                value: path.display().to_string(),
            });
        }
    }
    Ok(())
}

pub(super) fn initial_summary(status: SubagentStatusLabel) -> String {
    match status {
        SubagentStatusLabel::Queued => "child queued".to_owned(),
        SubagentStatusLabel::Running => "child running".to_owned(),
        SubagentStatusLabel::Completed => "child completed".to_owned(),
        SubagentStatusLabel::Failed => "child failed".to_owned(),
        SubagentStatusLabel::Blocked => "child blocked".to_owned(),
        SubagentStatusLabel::Cancelled => "child cancelled".to_owned(),
    }
}

pub(super) fn fallback_summary(status: SubagentStatusLabel, task: &SubagentTaskSpec) -> String {
    match task.display_name() {
        Some(display_name) => format!("{}: {display_name}", initial_summary(status)),
        None => initial_summary(status),
    }
}
