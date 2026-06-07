//! Runtime-owned parallel subagent tool contracts.

use crate::{
    AgentLoopConfig, AgentLoopResult, AgentLoopStatus, ArtifactContent, Runtime, RuntimeError,
    StepContext, StepInput, TaskAnchor,
};
use merry_core::{
    ErrorInfo, RuntimeEvent, RuntimeEventKind, SubagentId, SubagentTaskId, ToolCallResultStatus,
    ToolName,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

mod protocol;
mod spec;
mod tools;

pub use protocol::{
    CancelSubagentsInput, RejectedSubagentView, SpawnSubagentTaskInput, SpawnSubagentsInput,
    SpawnSubagentsOutput, SpawnedSubagentStatusLabel, SpawnedSubagentView, SubagentResultView,
    SubagentStatusLabel, SubagentStatusView, WaitMode, WaitSubagentsInput, WaitSubagentsOutput,
};
#[cfg(test)]
use spec::MAX_TASK_BYTES;
use spec::validate_scope_path;
pub use spec::{
    DEFAULT_MAX_MODEL_TURNS, SubagentConfig, SubagentError, SubagentTaskSpec,
    validate_no_write_scope_conflicts,
};
pub use tools::{subagent_registered_tools, subagent_tool_specs};

/// Provider-visible tool name for spawning bounded child agents.
pub(crate) const SPAWN_SUBAGENTS_TOOL_NAME: &str = "spawn_subagents";
/// Provider-visible tool name for waiting on child agent statuses/results.
pub(crate) const WAIT_SUBAGENTS_TOOL_NAME: &str = "wait_subagents";
/// Provider-visible tool name for cancelling child agents.
pub(crate) const CANCEL_SUBAGENTS_TOOL_NAME: &str = "cancel_subagents";
const WORKSPACE_PATCH_TOOL_NAME: &str = "workspace_patch";

/// Runtime construction input for one bounded child agent.
#[derive(Debug, Clone)]
pub struct ChildRuntimeInput {
    /// Session id owned by the child runtime.
    pub session_id: merry_core::SessionId,
    /// Control-plane task anchor installed in the child runtime.
    pub task_anchor: TaskAnchor,
    /// Parent-authored child task contract.
    pub task: SubagentTaskSpec,
    /// Tool names allowed for this child.
    pub allowed_tools: Vec<ToolName>,
    /// Workspace scope declared for this child.
    pub workspace_scope: ChildWorkspaceScope,
    /// Delegation depth assigned to this child.
    pub depth: u8,
}

/// Parent-authored workspace scope carried into child runtime construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildWorkspaceScope {
    read_scope: Vec<PathBuf>,
    write_scope: Vec<PathBuf>,
    forbidden_paths: Vec<PathBuf>,
}

impl ChildWorkspaceScope {
    /// Creates a workspace scope snapshot from a validated subagent task spec.
    #[must_use]
    pub fn from_task(task: &SubagentTaskSpec) -> Self {
        Self {
            read_scope: task.read_scope().to_vec(),
            write_scope: task.write_scope().to_vec(),
            forbidden_paths: task.forbidden_paths().to_vec(),
        }
    }

    /// Returns the advisory workspace-relative read scope.
    #[must_use]
    pub fn read_scope(&self) -> &[PathBuf] {
        &self.read_scope
    }

    /// Returns the workspace-relative write scope.
    #[must_use]
    pub fn write_scope(&self) -> &[PathBuf] {
        &self.write_scope
    }

    /// Returns workspace-relative paths the child must not access.
    #[must_use]
    pub fn forbidden_paths(&self) -> &[PathBuf] {
        &self.forbidden_paths
    }
}

/// Object-safe factory for constructing bounded child runtimes.
pub trait ChildRuntimeFactory: Send + Sync {
    /// Builds a child runtime from runtime-owned delegation input.
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError>;
}

/// Runtime-owned manager for bounded child agent execution.
#[derive(Clone)]
pub struct SubagentManager {
    parent_session_id: merry_core::SessionId,
    config: SubagentConfig,
    factory: Arc<dyn ChildRuntimeFactory>,
    state: Arc<Mutex<SubagentManagerState>>,
    notify: Arc<Notify>,
    next_id: Arc<AtomicU64>,
    next_batch_id: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct SubagentManagerState {
    agents: BTreeMap<SubagentId, ManagedSubagent>,
    batches: BTreeMap<u64, SubagentBatch>,
}

#[derive(Debug, Clone)]
struct SubagentBatch {
    max_concurrency: usize,
}

#[derive(Debug)]
struct ReservedChildStart {
    agent_id: SubagentId,
    task: SubagentTaskSpec,
    task_anchor: TaskAnchor,
    cancellation_token: CancellationToken,
}

#[derive(Clone)]
struct ChildScheduler {
    parent_session_id: merry_core::SessionId,
    factory: Arc<dyn ChildRuntimeFactory>,
    state: Arc<Mutex<SubagentManagerState>>,
    notify: Arc<Notify>,
    max_threads: usize,
}

struct ChildLoopLaunch {
    agent_id: SubagentId,
    task: SubagentTaskSpec,
    token: CancellationToken,
    runtime: Runtime,
}

#[derive(Debug, Clone)]
struct ManagedSubagent {
    batch_id: u64,
    agent_id: SubagentId,
    task_id: SubagentTaskId,
    task: SubagentTaskSpec,
    task_anchor: TaskAnchor,
    status: SubagentStatusLabel,
    summary: String,
    result: Option<SubagentResultView>,
    output_paths: Vec<String>,
    changed_paths: Vec<String>,
    diagnostics: Option<ErrorInfo>,
    cancellation_token: CancellationToken,
}

impl SubagentManager {
    /// Creates a subagent manager for one parent session.
    #[must_use]
    pub fn new(
        parent_session_id: merry_core::SessionId,
        config: SubagentConfig,
        factory: Arc<dyn ChildRuntimeFactory>,
    ) -> Self {
        Self {
            parent_session_id,
            config,
            factory,
            state: Arc::new(Mutex::new(SubagentManagerState::default())),
            notify: Arc::new(Notify::new()),
            next_id: Arc::new(AtomicU64::new(1)),
            next_batch_id: Arc::new(AtomicU64::new(1)),
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

    /// Accepts a batch of child tasks, starting only the initial bounded slice.
    pub async fn spawn(
        &self,
        tasks: Vec<SubagentTaskSpec>,
        max_concurrency: Option<usize>,
        parent_token: CancellationToken,
    ) -> Result<SpawnSubagentsOutput, RuntimeError> {
        if let Err(error) = validate_no_write_scope_conflicts(&tasks) {
            return Ok(SpawnSubagentsOutput {
                spawned: Vec::new(),
                rejected: (0..tasks.len())
                    .map(|task_index| RejectedSubagentView {
                        task_index,
                        reason: error.to_string(),
                    })
                    .collect(),
            });
        }

        let task_inputs = tasks
            .into_iter()
            .map(|task| {
                TaskAnchor::new(task.task())
                    .map(|task_anchor| (task, task_anchor))
                    .map_err(RuntimeError::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut spawned = Vec::with_capacity(task_inputs.len());
        let mut to_start = Vec::new();
        let batch_id = self.next_batch_id.fetch_add(1, Ordering::SeqCst);
        let batch_max_concurrency = max_concurrency
            .unwrap_or(task_inputs.len())
            .min(self.config.max_threads());
        let mut state = self.state.lock().await;
        state.batches.insert(
            batch_id,
            SubagentBatch {
                max_concurrency: batch_max_concurrency,
            },
        );

        for (task, task_anchor) in task_inputs {
            let number = self.next_id.fetch_add(1, Ordering::SeqCst);
            let agent_id = SubagentId::new(&format!("agent-{number}"))?;
            let task_id = SubagentTaskId::new(&format!("task-{number}"))?;
            let child_token = parent_token.child_token();
            let starts_now = running_child_count(&state) < self.config.max_threads()
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
                to_start.push((agent_id, task, task_anchor, child_token));
            }
        }
        drop(state);

        for (agent_id, task, task_anchor, child_token) in to_start {
            self.start_child(agent_id, task, task_anchor, child_token)
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
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let output = self.status_for(agent_ids).await;
            let ready = match mode {
                WaitMode::Any => output.agents.iter().any(SubagentStatusView::is_terminal),
                WaitMode::All => output.agents.iter().all(SubagentStatusView::is_terminal),
            };
            if ready {
                return Ok(output);
            }

            match deadline {
                Some(deadline) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Ok(output);
                    }
                    if tokio::time::timeout_at(deadline, notified.as_mut())
                        .await
                        .is_err()
                    {
                        return Ok(self.status_for(agent_ids).await);
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
        let mut state = self.state.lock().await;
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
            }
        }
        let agents = selected_statuses(&state, agent_ids);
        let to_start = self.reserve_queued_starts_locked(&mut state);
        drop(state);
        self.notify.notify_waiters();
        self.start_reserved_children(to_start).await?;
        Ok(WaitSubagentsOutput { agents })
    }

    async fn start_child(
        &self,
        agent_id: SubagentId,
        task: SubagentTaskSpec,
        task_anchor: TaskAnchor,
        token: CancellationToken,
    ) {
        let child_session_id = match child_session_id(&self.parent_session_id, agent_id.as_str()) {
            Ok(session_id) => session_id,
            Err(error) => {
                self.mark_failed_and_schedule(
                    &agent_id,
                    "child runtime session id was rejected",
                    error_info("subagent_start_error", error.to_string()),
                )
                .await;
                return;
            }
        };
        let runtime = match self.factory.build_child(ChildRuntimeInput {
            session_id: child_session_id,
            task_anchor,
            task: task.clone(),
            allowed_tools: task.allowed_tools().to_vec(),
            workspace_scope: ChildWorkspaceScope::from_task(&task),
            depth: 1,
        }) {
            Ok(runtime) => runtime,
            Err(error) => {
                self.mark_failed_and_schedule(
                    &agent_id,
                    "child runtime factory failed",
                    error_info("subagent_factory_error", error.to_string()),
                )
                .await;
                return;
            }
        };

        spawn_child_loop(
            self.child_scheduler(),
            ChildLoopLaunch {
                agent_id,
                task,
                token,
                runtime,
            },
        );
    }

    async fn mark_failed_and_schedule(
        &self,
        agent_id: &SubagentId,
        summary: &str,
        diagnostics: ErrorInfo,
    ) {
        let mut state = self.state.lock().await;
        if let Some(agent) = state.agents.get_mut(agent_id) {
            if agent.status.is_terminal() {
                return;
            }
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = summary.to_owned();
            agent.diagnostics = Some(diagnostics);
        }
        let to_start = self.reserve_queued_starts_locked(&mut state);
        drop(state);
        self.notify.notify_waiters();
        let _ = self.start_reserved_children(to_start).await;
    }

    fn reserve_queued_starts_locked(
        &self,
        state: &mut SubagentManagerState,
    ) -> Vec<ReservedChildStart> {
        reserve_queued_starts_locked(state, self.config.max_threads())
    }

    fn child_scheduler(&self) -> ChildScheduler {
        ChildScheduler {
            parent_session_id: self.parent_session_id.clone(),
            factory: Arc::clone(&self.factory),
            state: Arc::clone(&self.state),
            notify: Arc::clone(&self.notify),
            max_threads: self.config.max_threads(),
        }
    }

    async fn start_reserved_children(
        &self,
        starts: Vec<ReservedChildStart>,
    ) -> Result<(), RuntimeError> {
        start_reserved_children_iteratively(self.child_scheduler(), starts).await;
        Ok(())
    }

    async fn status_for(&self, agent_ids: &[SubagentId]) -> WaitSubagentsOutput {
        let state = self.state.lock().await;
        WaitSubagentsOutput {
            agents: selected_statuses(&state, agent_ids),
        }
    }

    #[cfg(test)]
    async fn cancellation_token_for_test(
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

impl ManagedSubagent {
    fn status_view(&self) -> SubagentStatusView {
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

fn selected_statuses(
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

fn reserve_queued_starts_locked(
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
            task: agent.task.clone(),
            task_anchor: agent.task_anchor.clone(),
            cancellation_token: agent.cancellation_token.clone(),
        });
        global_available -= 1;
    }

    starts
}

fn running_child_count(state: &SubagentManagerState) -> usize {
    state
        .agents
        .values()
        .filter(|agent| agent.status == SubagentStatusLabel::Running)
        .count()
}

fn batch_running_child_count(state: &SubagentManagerState, batch_id: u64) -> usize {
    state
        .agents
        .values()
        .filter(|agent| agent.batch_id == batch_id && agent.status == SubagentStatusLabel::Running)
        .count()
}

async fn start_reserved_children_iteratively(
    scheduler: ChildScheduler,
    starts: Vec<ReservedChildStart>,
) {
    let mut pending = VecDeque::from(starts);
    while let Some(start) = pending.pop_front() {
        match spawn_reserved_child(scheduler.clone(), &start) {
            Ok(()) => {}
            Err(error) => {
                let mut state_guard = scheduler.state.lock().await;
                if let Some(agent) = state_guard.agents.get_mut(&start.agent_id) {
                    if !agent.status.is_terminal() {
                        agent.status = SubagentStatusLabel::Failed;
                        agent.summary = "child runtime start failed".to_owned();
                        agent.diagnostics =
                            Some(error_info("subagent_start_error", error.to_string()));
                    }
                }
                pending.extend(reserve_queued_starts_locked(
                    &mut state_guard,
                    scheduler.max_threads,
                ));
                drop(state_guard);
                scheduler.notify.notify_waiters();
            }
        }
    }
}

fn spawn_reserved_child(
    scheduler: ChildScheduler,
    start: &ReservedChildStart,
) -> Result<(), RuntimeError> {
    let child_session_id = child_session_id(&scheduler.parent_session_id, start.agent_id.as_str())?;
    let runtime = scheduler.factory.build_child(ChildRuntimeInput {
        session_id: child_session_id,
        task_anchor: start.task_anchor.clone(),
        task: start.task.clone(),
        allowed_tools: start.task.allowed_tools().to_vec(),
        workspace_scope: ChildWorkspaceScope::from_task(&start.task),
        depth: 1,
    })?;

    spawn_child_loop(
        scheduler,
        ChildLoopLaunch {
            agent_id: start.agent_id.clone(),
            task: start.task.clone(),
            token: start.cancellation_token.clone(),
            runtime,
        },
    );

    Ok(())
}

fn spawn_child_loop(scheduler: ChildScheduler, launch: ChildLoopLaunch) {
    tokio::spawn(async move {
        let input = match StepInput::user_text(launch.task.task()) {
            Ok(input) => input,
            Err(error) => {
                let to_start = update_child_after_error(
                    &scheduler.state,
                    &launch.agent_id,
                    "child task input was rejected",
                    error_info("subagent_input_error", error.to_string()),
                    scheduler.max_threads,
                )
                .await;
                scheduler.notify.notify_waiters();
                start_reserved_children_iteratively(scheduler, to_start).await;
                return;
            }
        };
        let config = match AgentLoopConfig::new(launch.task.max_model_turns() as usize) {
            Ok(config) => config,
            Err(error) => {
                let to_start = update_child_after_error(
                    &scheduler.state,
                    &launch.agent_id,
                    "child loop configuration was rejected",
                    error_info("subagent_config_error", error.to_string()),
                    scheduler.max_threads,
                )
                .await;
                scheduler.notify.notify_waiters();
                start_reserved_children_iteratively(scheduler, to_start).await;
                return;
            }
        };

        let loop_result = launch
            .runtime
            .run_agent_loop(input, StepContext::new(launch.token), config)
            .await;
        let child_projection = match &loop_result {
            Ok(result) => ChildLoopProjection::from_result(&launch.runtime, result).await,
            Err(_) => ChildLoopProjection::default(),
        };

        let mut state_guard = scheduler.state.lock().await;
        if let Some(agent) = state_guard.agents.get_mut(&launch.agent_id) {
            if agent.status == SubagentStatusLabel::Cancelled {
                let to_start =
                    reserve_queued_starts_locked(&mut state_guard, scheduler.max_threads);
                drop(state_guard);
                scheduler.notify.notify_waiters();
                start_reserved_children_iteratively(scheduler, to_start).await;
                return;
            }
            match loop_result {
                Ok(result) => apply_loop_result(agent, &result, child_projection),
                Err(error) => {
                    agent.status = SubagentStatusLabel::Failed;
                    agent.summary = "child runtime error".to_owned();
                    agent.result = None;
                    agent.diagnostics =
                        Some(error_info("subagent_runtime_error", error.to_string()));
                }
            }
        }
        let to_start = reserve_queued_starts_locked(&mut state_guard, scheduler.max_threads);
        drop(state_guard);
        scheduler.notify.notify_waiters();
        start_reserved_children_iteratively(scheduler, to_start).await;
    });
}

fn paths_to_strings(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn child_session_id(
    parent_session_id: &merry_core::SessionId,
    agent_id: &str,
) -> Result<merry_core::SessionId, RuntimeError> {
    Ok(merry_core::SessionId::new(&format!(
        "{}-{agent_id}",
        parent_session_id.as_str()
    ))?)
}

fn initial_summary(status: SubagentStatusLabel) -> String {
    match status {
        SubagentStatusLabel::Queued => "child queued".to_owned(),
        SubagentStatusLabel::Running => "child running".to_owned(),
        SubagentStatusLabel::Completed => "child completed".to_owned(),
        SubagentStatusLabel::Failed => "child failed".to_owned(),
        SubagentStatusLabel::Cancelled => "child cancelled".to_owned(),
    }
}

fn fallback_summary(status: SubagentStatusLabel, task: &SubagentTaskSpec) -> String {
    match task.display_name() {
        Some(display_name) => format!("{}: {display_name}", initial_summary(status)),
        None => initial_summary(status),
    }
}

#[derive(Debug, Default)]
struct ChildLoopProjection {
    result: Option<SubagentResultView>,
    changed_paths: Vec<String>,
}

impl ChildLoopProjection {
    async fn from_result(runtime: &Runtime, result: &AgentLoopResult) -> Self {
        let explicit_result = match result.status() {
            AgentLoopStatus::Completed => result
                .final_output()
                .and_then(SubagentResultView::from_conclusion),
            AgentLoopStatus::Failed { .. }
            | AgentLoopStatus::Cancelled { .. }
            | AgentLoopStatus::Blocked { .. } => None,
        };

        Self {
            result: explicit_result,
            changed_paths: changed_paths_from_child_events(runtime, result.events()).await,
        }
    }
}

fn apply_loop_result(
    agent: &mut ManagedSubagent,
    result: &AgentLoopResult,
    projection: ChildLoopProjection,
) {
    agent.result = projection.result;
    agent.changed_paths = projection.changed_paths;

    match result.status() {
        AgentLoopStatus::Completed => {
            agent.status = SubagentStatusLabel::Completed;
            agent.summary = "child completed".to_owned();
            agent.output_paths.clear();
        }
        AgentLoopStatus::Failed { diagnostic } => {
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = format!("child failed: {}", diagnostic.message());
            agent.diagnostics = Some(diagnostic.clone());
        }
        AgentLoopStatus::Cancelled { diagnostic } => {
            agent.status = SubagentStatusLabel::Cancelled;
            agent.summary = format!("child cancelled: {}", diagnostic.message());
            agent.diagnostics = Some(diagnostic.clone());
        }
        AgentLoopStatus::Blocked { reason } => {
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = format!("child blocked: {reason:?}");
            agent.diagnostics = Some(error_info("subagent_blocked", format!("{reason:?}")));
        }
    }
}

async fn changed_paths_from_child_events(
    runtime: &Runtime,
    events: &[RuntimeEvent],
) -> Vec<String> {
    let mut pending_tool_names = BTreeMap::new();
    let mut paths = BTreeSet::new();

    for event in events {
        match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => {
                pending_tool_names.insert(call.id().clone(), call.name().clone());
            }
            RuntimeEventKind::ToolCallResolved { result }
                if result.status() == ToolCallResultStatus::Succeeded
                    && pending_tool_names
                        .get(result.call_id())
                        .is_some_and(|tool_name| {
                            tool_name.as_str() == WORKSPACE_PATCH_TOOL_NAME
                        }) =>
            {
                let Ok(content) = runtime.read_artifact_content(result.artifact().id()).await
                else {
                    continue;
                };
                collect_workspace_patch_changed_paths(&content, &mut paths);
            }
            _ => {}
        }
    }

    paths.into_iter().collect()
}

fn collect_workspace_patch_changed_paths(content: &ArtifactContent, paths: &mut BTreeSet<String>) {
    let Some(text) = content.as_text() else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
        || value.get("tool").and_then(serde_json::Value::as_str) != Some(WORKSPACE_PATCH_TOOL_NAME)
    {
        return;
    }

    let Some(changes) = value.get("changes").and_then(serde_json::Value::as_array) else {
        return;
    };
    for change in changes {
        let Some(path) = change.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if validate_scope_path(PathBuf::from(path)).is_ok() {
            paths.insert(path.to_owned());
        }
    }
}

async fn update_child_after_error(
    state: &Mutex<SubagentManagerState>,
    agent_id: &SubagentId,
    summary: &str,
    diagnostics: ErrorInfo,
    max_threads: usize,
) -> Vec<ReservedChildStart> {
    let mut state = state.lock().await;
    if let Some(agent) = state.agents.get_mut(agent_id) {
        if !agent.status.is_terminal() {
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = summary.to_owned();
            agent.diagnostics = Some(diagnostics);
        }
    }
    reserve_queued_starts_locked(&mut state, max_threads)
}

fn error_info(code: &'static str, message: impl ToString) -> ErrorInfo {
    ErrorInfo::new(code, &sanitize_diagnostic_message(message.to_string()))
        .expect("static diagnostic code and sanitized message are valid")
}

fn sanitize_diagnostic_message(message: String) -> String {
    let sanitized = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim();
    let source = if trimmed.is_empty() {
        "child runtime failed without diagnostic detail"
    } else {
        trimmed
    };

    source.chars().take(4096).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn subagent_task_rejects_blank_task_and_zero_steps() {
        let blank = SubagentTaskSpec::new(" ", 4).expect_err("blank task should fail");
        assert!(blank.to_string().contains("task must not be blank"));

        let too_long = "x".repeat(MAX_TASK_BYTES + 1);
        let too_long_error =
            SubagentTaskSpec::new(too_long, 4).expect_err("oversized task should fail");
        assert!(matches!(too_long_error, SubagentError::TaskTooLong));

        let zero =
            SubagentTaskSpec::new("Review src/lib.rs.", 0).expect_err("zero max steps should fail");
        assert!(
            zero.to_string()
                .contains("max_model_turns must be greater than zero")
        );
    }

    #[test]
    fn scope_paths_must_be_relative_and_normalized() {
        for invalid in [
            "",
            ".",
            "..",
            "src/../lib.rs",
            "/tmp/file",
            "src/./lib.rs",
            "src\\runtime.rs",
            "C:\\tmp",
            "bad\npath",
        ] {
            let error = SubagentTaskSpec::new("Read scoped path.", 4)
                .expect("valid task")
                .with_read_scope([invalid])
                .expect_err("invalid scope should fail");
            assert!(
                matches!(error, SubagentError::InvalidScopePath { .. }),
                "{invalid:?} should produce an invalid scope path error"
            );
        }
    }

    #[test]
    fn conflicting_child_write_scopes_are_rejected_before_spawn() {
        let first = SubagentTaskSpec::new("Edit runtime module.", 4)
            .expect("valid task")
            .with_write_scope(["src"])
            .expect("valid scope");
        let second = SubagentTaskSpec::new("Edit nested function.", 4)
            .expect("valid task")
            .with_write_scope(["src/runtime.rs"])
            .expect("valid scope");

        let error = validate_no_write_scope_conflicts(&[first, second])
            .expect_err("parent/child write scope should conflict");
        assert!(error.to_string().contains("overlapping write scope"));
    }

    #[test]
    fn read_only_tasks_may_overlap_read_scope() {
        let first = SubagentTaskSpec::new("Read runtime module.", 4)
            .expect("valid task")
            .with_read_scope(["src/runtime.rs"])
            .expect("valid scope");
        let second = SubagentTaskSpec::new("Read runtime tests.", 4)
            .expect("valid task")
            .with_read_scope(["src/runtime.rs"])
            .expect("valid scope");

        validate_no_write_scope_conflicts(&[first, second])
            .expect("read-only tasks do not conflict");
    }

    #[test]
    fn task_spec_preserves_forbidden_paths_and_expected_output() {
        let task = SubagentTaskSpec::new("Check generated report.", 4)
            .expect("valid task")
            .with_forbidden_paths(["target", ".git"])
            .expect("valid forbidden paths")
            .with_expected_output(Some("Write a compact finding summary.".to_owned()));

        assert_eq!(
            task.forbidden_paths(),
            &[PathBuf::from(".git"), PathBuf::from("target")]
        );
        assert_eq!(
            task.expected_output(),
            Some("Write a compact finding summary.")
        );

        let blank_expected = task.with_expected_output(Some(" ".to_owned()));
        assert_eq!(blank_expected.expected_output(), None);
    }

    #[test]
    fn spawned_status_serializes_as_typed_label() {
        let view = SpawnedSubagentView {
            agent_id: SubagentId::new("agent-1").expect("valid id"),
            task_id: SubagentTaskId::new("task-1").expect("valid id"),
            display_name: None,
            status: SpawnedSubagentStatusLabel::Running,
            task_anchor: "Review runtime.".to_owned(),
            read_scope: vec!["src/runtime.rs".to_owned()],
            write_scope: vec![],
        };

        assert_eq!(
            serde_json::to_value(&view).expect("view serializes"),
            json!({
                "agent_id": "agent-1",
                "task_id": "task-1",
                "display_name": null,
                "status": "running",
                "task_anchor": "Review runtime.",
                "read_scope": ["src/runtime.rs"],
                "write_scope": []
            })
        );
    }

    #[test]
    fn subagent_tool_specs_are_stable_and_schema_backed() {
        let specs = subagent_tool_specs().expect("tool specs should build");
        let names = specs
            .iter()
            .map(|spec| spec.name().as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["spawn_subagents", "wait_subagents", "cancel_subagents"]
        );

        for spec in specs {
            let value = serde_json::to_value(spec.input_schema()).expect("schema serializes");
            assert!(matches!(value, Value::Object(_)));
        }
    }

    #[test]
    fn wait_output_serializes_compact_status_and_paths() {
        let output = WaitSubagentsOutput::new(vec![SubagentStatusView::completed(
            SubagentId::new("agent-1").expect("valid id"),
            SubagentTaskId::new("task-1").expect("valid id"),
            "Done.",
            vec!["shared/subagents/agent-1/result.md".to_owned()],
            vec![],
        )]);

        assert_eq!(
            serde_json::to_value(&output).expect("output serializes"),
            json!({
                "agents": [{
                    "agent_id": "agent-1",
                    "task_id": "task-1",
                    "status": "completed",
                    "summary": "Done.",
                    "result": null,
                    "output_paths": ["shared/subagents/agent-1/result.md"],
                    "changed_paths": [],
                    "diagnostics": null
                }]
            })
        );
    }

    #[test]
    fn allowed_tools_are_validated_as_tool_names() {
        let task = SubagentTaskSpec::new("Read files.", 4)
            .expect("valid task")
            .with_allowed_tools([ToolName::new("workspace_read_file").expect("valid tool name")]);

        assert_eq!(
            task.allowed_tools(),
            &[ToolName::new("workspace_read_file").expect("valid tool name")]
        );
    }
}

#[cfg(test)]
mod manager_tests {
    use super::*;
    use crate::{
        RegisteredTool, Runtime, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor,
        ToolExecutorFuture,
    };
    use merry_core::{PendingToolCall, SessionId, ToolInputSchema, ToolName, ToolSpec};
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
        ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
        ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
    };
    use schemars::Schema;
    use serde_json::{Map, json};
    use std::{
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct FakeChildFactory {
        started: Arc<AtomicUsize>,
    }

    impl FakeChildFactory {
        fn new() -> Self {
            Self {
                started: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl ChildRuntimeFactory for FakeChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .build()
        }
    }

    #[derive(Clone)]
    struct FailsFirstChildFactory {
        calls: Arc<AtomicUsize>,
    }

    impl FailsFirstChildFactory {
        fn new() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ChildRuntimeFactory for FailsFirstChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 1 {
                return Err(crate::RuntimeError::InvalidStepInput {
                    reason: "test child factory failure",
                });
            }

            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .build()
        }
    }

    #[derive(Clone)]
    struct PendingChildFactory {
        started: Arc<AtomicUsize>,
    }

    impl PendingChildFactory {
        fn new() -> Self {
            Self {
                started: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl ChildRuntimeFactory for PendingChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            let started = self.started.fetch_add(1, Ordering::SeqCst);
            if started == 0 {
                return Runtime::builder(input.session_id)
                    .task_anchor(input.task_anchor)
                    .build();
            }

            let provider = PendingModelProvider::new();
            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .model_provider(
                    Arc::new(provider),
                    merry_llm::ModelName::new("fake/pending").expect("valid model name"),
                )
                .build()
        }
    }

    struct PendingModelProvider {
        name: merry_core::ProviderName,
        capabilities: merry_llm::ModelCapabilities,
    }

    impl PendingModelProvider {
        fn new() -> Self {
            Self {
                name: merry_core::ProviderName::new("pending-model-provider")
                    .expect("valid provider name"),
                capabilities: merry_llm::ModelCapabilities::new(
                    true, false, false, false, None, None,
                )
                .expect("valid capabilities"),
            }
        }
    }

    impl merry_llm::ModelProvider for PendingModelProvider {
        fn name(&self) -> &merry_core::ProviderName {
            &self.name
        }

        fn capabilities(&self) -> &merry_llm::ModelCapabilities {
            &self.capabilities
        }

        fn stream_model<'a>(
            &'a self,
            _request: merry_llm::ModelRequest,
            _context: merry_llm::ModelStreamContext,
        ) -> merry_llm::ModelProviderFuture<
            'a,
            Result<merry_llm::ModelEventStream, merry_llm::ModelError>,
        > {
            Box::pin(async move {
                let stream = futures_util::stream::pending::<
                    Result<merry_llm::ModelEvent, merry_llm::ModelError>,
                >();
                Ok(Box::pin(stream) as merry_llm::ModelEventStream)
            })
        }
    }

    struct ReportingChildFactory;

    impl ChildRuntimeFactory for ReportingChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            let provider = ScriptedStepProvider::new(vec![
                vec![Ok(ModelEvent::Completed {
                    response: ModelResponse::new(
                        vec![ModelOutput::tool_call(ModelToolCall::new(
                            ModelToolCallId::new("call-child-patch").expect("valid call id"),
                            ToolName::new("workspace_patch").expect("valid tool name"),
                            ToolArguments::new(Map::new()),
                        ))],
                        FinishReason::ToolCalls,
                        None,
                    ),
                })],
                vec![Ok(ModelEvent::Completed {
                    response: ModelResponse::new(
                        vec![ModelOutput::text(
                            "Patched subagent-output.txt to status: done.",
                        )],
                        FinishReason::Stop,
                        None,
                    ),
                })],
            ]);

            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .model_provider(
                    Arc::new(provider),
                    ModelName::new("fake/reporting-child").expect("valid model name"),
                )
                .register_tool(RegisteredTool::read_only(
                    workspace_patch_tool_spec(),
                    Arc::new(FakeWorkspacePatchExecutor),
                ))
                .build()
        }
    }

    type ScriptedStepEvents = Vec<Result<ModelEvent, ModelError>>;
    type ScriptedStepResponses = Vec<ScriptedStepEvents>;

    struct ScriptedStepProvider {
        name: merry_core::ProviderName,
        capabilities: ModelCapabilities,
        responses: Arc<StdMutex<ScriptedStepResponses>>,
    }

    impl ScriptedStepProvider {
        fn new(responses: ScriptedStepResponses) -> Self {
            Self {
                name: merry_core::ProviderName::new("scripted-step-provider")
                    .expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities"),
                responses: Arc::new(StdMutex::new(responses.into_iter().rev().collect())),
            }
        }
    }

    impl ModelProvider for ScriptedStepProvider {
        fn name(&self) -> &merry_core::ProviderName {
            &self.name
        }

        fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }

        fn stream_model<'a>(
            &'a self,
            request: ModelRequest,
            _context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            Box::pin(async move {
                let _ = request;
                let events = self
                    .responses
                    .lock()
                    .expect("scripted provider response mutex should not be poisoned")
                    .pop()
                    .expect("scripted child provider should have a response for each step");
                Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
            })
        }
    }

    struct FakeWorkspacePatchExecutor;

    impl ToolExecutor for FakeWorkspacePatchExecutor {
        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            _context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async {
                Ok(ToolExecutionOutcome::succeeded_json(
                    json!({
                        "ok": true,
                        "tool": "workspace_patch",
                        "changes": [{
                            "path": "subagent-output.txt",
                            "hunks": 1
                        }]
                    })
                    .to_string(),
                ))
            })
        }
    }

    fn workspace_patch_tool_spec() -> ToolSpec {
        let schema = Schema::try_from(json!({ "type": "object" }))
            .expect("test schema should be a JSON schema");
        ToolSpec::new(
            ToolName::new("workspace_patch").expect("valid tool name"),
            "Apply a workspace patch.",
            ToolInputSchema::new(schema).expect("valid schema"),
        )
        .expect("valid tool spec")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_rejects_overlapping_write_scopes_before_spawn() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            factory.clone(),
        );
        let first = SubagentTaskSpec::new("Edit one.", 2)
            .expect("valid")
            .with_write_scope(["src/lib.rs"])
            .expect("valid scope");
        let second = SubagentTaskSpec::new("Edit two.", 2)
            .expect("valid")
            .with_write_scope(["src"])
            .expect("valid scope");

        let output = manager
            .spawn(vec![first, second], Some(2), CancellationToken::new())
            .await
            .expect("spawn tool should return a structured result");

        assert!(output.spawned.is_empty());
        assert_eq!(output.rejected.len(), 2);
        assert_eq!(factory.started.load(Ordering::SeqCst), 0);
        assert_eq!(manager.snapshot().await.len(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_starts_children_under_max_concurrency() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory.clone(),
        );
        let first = SubagentTaskSpec::new("First task.", 2).expect("valid");
        let second = SubagentTaskSpec::new("Second task.", 2).expect("valid");

        let output = manager
            .spawn(vec![first, second], Some(2), CancellationToken::new())
            .await
            .expect("spawn should succeed");

        assert_eq!(output.spawned.len(), 2);
        assert_eq!(
            output.spawned[0].status,
            SpawnedSubagentStatusLabel::Running
        );
        assert_eq!(output.spawned[1].status, SpawnedSubagentStatusLabel::Queued);
        assert_eq!(factory.started.load(Ordering::SeqCst), 1);
        assert_eq!(manager.snapshot().await.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_counts_existing_open_children_against_global_thread_limit() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory.clone(),
        );

        manager
            .spawn(
                vec![SubagentTaskSpec::new("First open task.", 2).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("first spawn should succeed");
        let second = manager
            .spawn(
                vec![SubagentTaskSpec::new("Second queued task.", 2).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("second spawn should succeed");

        assert_eq!(second.spawned[0].status, SpawnedSubagentStatusLabel::Queued);
        assert_eq!(factory.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_zero_concurrency_leaves_spawned_children_queued() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            factory.clone(),
        );

        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Queued task.", 2).expect("valid")],
                Some(0),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");

        assert_eq!(output.spawned[0].status, SpawnedSubagentStatusLabel::Queued);
        assert_eq!(factory.started.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_child_is_promoted_after_running_child_completes() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory.clone(),
        );

        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Complete first.", 1).expect("valid"),
                    SubagentTaskSpec::new("Promote second.", 1).expect("valid"),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let second_id = output.spawned[1].agent_id.clone();

        let second = manager
            .wait(
                std::slice::from_ref(&second_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("wait should return promoted child");

        assert_eq!(factory.started.load(Ordering::SeqCst), 2);
        assert_eq!(second.agents.len(), 1);
        assert_ne!(second.agents[0].status, SubagentStatusLabel::Queued);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_promotion_respects_spawn_batch_max_concurrency() {
        let factory = Arc::new(PendingChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(2, 1).expect("valid config"),
            factory.clone(),
        );

        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Run first until max steps.", 1).expect("valid"),
                    SubagentTaskSpec::new("Promote second and keep it pending.", 2).expect("valid"),
                    SubagentTaskSpec::new("Remain queued by batch cap.", 2).expect("valid"),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let third_id = output.spawned[2].agent_id.clone();

        tokio::time::timeout(Duration::from_millis(100), async {
            while factory.started.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second child should start before timeout");
        tokio::task::yield_now().await;
        let snapshot = manager.snapshot().await;

        assert_eq!(factory.started.load(Ordering::SeqCst), 2);
        assert_eq!(
            snapshot
                .iter()
                .find(|agent| agent.agent_id == third_id)
                .expect("third child remains tracked")
                .status,
            SubagentStatusLabel::Queued
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn zero_concurrency_batch_is_not_promoted_by_unrelated_completion() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(2, 1).expect("valid config"),
            factory.clone(),
        );
        let zero_batch = manager
            .spawn(
                vec![SubagentTaskSpec::new("Stay queued.", 1).expect("valid")],
                Some(0),
                CancellationToken::new(),
            )
            .await
            .expect("zero concurrency spawn should succeed");
        let queued_id = zero_batch.spawned[0].agent_id.clone();
        let running_batch = manager
            .spawn(
                vec![SubagentTaskSpec::new("Complete unrelated.", 1).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("running spawn should succeed");
        let running_id = running_batch.spawned[0].agent_id.clone();

        manager
            .wait(
                std::slice::from_ref(&running_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("unrelated running child should complete");
        let snapshot = manager.snapshot().await;

        assert_eq!(
            snapshot
                .iter()
                .find(|agent| agent.agent_id == queued_id)
                .expect("zero concurrency child remains tracked")
                .status,
            SubagentStatusLabel::Queued
        );
        assert_eq!(factory.started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_child_is_promoted_after_running_child_is_cancelled() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory.clone(),
        );

        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Cancel first.", 2).expect("valid"),
                    SubagentTaskSpec::new("Promote after cancel.", 1).expect("valid"),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let first_id = output.spawned[0].agent_id.clone();
        let second_id = output.spawned[1].agent_id.clone();

        manager
            .cancel(std::slice::from_ref(&first_id))
            .await
            .expect("cancel should succeed");
        let second = manager
            .wait(
                std::slice::from_ref(&second_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("wait should return promoted child");

        assert_eq!(factory.started.load(Ordering::SeqCst), 2);
        assert_eq!(second.agents.len(), 1);
        assert_ne!(second.agents[0].status, SubagentStatusLabel::Queued);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_child_is_promoted_after_factory_failure() {
        let factory = Arc::new(FailsFirstChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory.clone(),
        );

        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Fail factory.", 1).expect("valid"),
                    SubagentTaskSpec::new("Promote after failure.", 1).expect("valid"),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let second_id = output.spawned[1].agent_id.clone();

        let second = manager
            .wait(
                std::slice::from_ref(&second_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("wait should return promoted child");

        assert_eq!(factory.calls(), 2);
        assert_eq!(second.agents.len(), 1);
        assert_ne!(second.agents[0].status, SubagentStatusLabel::Queued);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_session_id_failure_marks_failed_and_promotes_queue() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new(&"p".repeat(128)).expect("max length parent id is valid"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory.clone(),
        );

        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Fail child id.", 1).expect("valid"),
                    SubagentTaskSpec::new("Promote after child id failure.", 1).expect("valid"),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should return structured statuses");
        let first_id = output.spawned[0].agent_id.clone();
        let second_id = output.spawned[1].agent_id.clone();

        let first = manager
            .wait(
                std::slice::from_ref(&first_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("wait should return failed child");
        let second = manager
            .wait(
                std::slice::from_ref(&second_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("wait should return promoted child");

        assert_eq!(first.agents[0].status, SubagentStatusLabel::Failed);
        assert_ne!(second.agents[0].status, SubagentStatusLabel::Queued);
        assert_eq!(factory.started.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_wait_returns_compact_statuses() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            factory,
        );
        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Complete task.", 1).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let agent_id = output.spawned[0].agent_id.clone();

        let wait = manager
            .wait(&[agent_id], WaitMode::All, Some(Duration::from_millis(10)))
            .await
            .expect("wait should return status");

        assert_eq!(wait.agents.len(), 1);
        assert!(matches!(
            wait.agents[0].status,
            SubagentStatusLabel::Completed
                | SubagentStatusLabel::Failed
                | SubagentStatusLabel::Running
        ));
        assert!(wait.agents[0].summary.len() < 256);
        assert!(wait.agents[0].output_paths.len() <= 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_child_has_no_output_paths_until_artifact_handoff_exists() {
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            Arc::new(FakeChildFactory::new()),
        );
        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Complete without artifact.", 1).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let agent_id = output.spawned[0].agent_id.clone();

        let wait = manager
            .wait(
                std::slice::from_ref(&agent_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("wait should return terminal child");

        assert_eq!(wait.agents[0].status, SubagentStatusLabel::Completed);
        assert!(wait.agents[0].output_paths.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completed_child_reports_explicit_result_and_changed_paths() {
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            Arc::new(ReportingChildFactory),
        );
        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Patch the status file.", 4).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let agent_id = output.spawned[0].agent_id.clone();

        let wait = manager
            .wait(
                std::slice::from_ref(&agent_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("wait should return terminal child");

        assert_eq!(wait.agents[0].status, SubagentStatusLabel::Completed);
        assert_eq!(wait.agents[0].summary, "child completed");
        assert_eq!(
            wait.agents[0]
                .result
                .as_ref()
                .map(|result| result.conclusion.as_str()),
            Some("Patched subagent-output.txt to status: done.")
        );
        assert_eq!(
            wait.agents[0].changed_paths,
            vec!["subagent-output.txt".to_owned()]
        );
        assert!(wait.agents[0].output_paths.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_cancel_marks_selected_children_cancelled() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory,
        );
        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Cancel this task.", 2).expect("valid"),
                    SubagentTaskSpec::new("Leave this task queued.", 2).expect("valid"),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");

        let cancelled_id = output.spawned[0].agent_id.clone();
        let untouched_id = output.spawned[1].agent_id.clone();
        let cancelled = manager
            .cancel(std::slice::from_ref(&cancelled_id))
            .await
            .expect("cancel should return selected status");
        let snapshot = manager.snapshot().await;

        assert_eq!(cancelled.agents.len(), 1);
        assert_eq!(cancelled.agents[0].status, SubagentStatusLabel::Cancelled);
        assert_eq!(
            snapshot
                .iter()
                .find(|agent| agent.agent_id == cancelled_id)
                .expect("cancelled child remains in snapshot")
                .status,
            SubagentStatusLabel::Cancelled
        );
        assert_ne!(
            snapshot
                .iter()
                .find(|agent| agent.agent_id == untouched_id)
                .expect("untouched child remains in snapshot")
                .status,
            SubagentStatusLabel::Cancelled
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_cancel_cancels_selected_child_token() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            factory,
        );
        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Cancel token.", 2).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let agent_id = output.spawned[0].agent_id.clone();
        let token = manager
            .cancellation_token_for_test(&agent_id)
            .await
            .expect("managed token exists");

        manager
            .cancel(&[agent_id])
            .await
            .expect("cancel should succeed");

        assert!(token.is_cancelled());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_cancel_does_not_rewrite_terminal_children() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            factory,
        );
        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Complete then cancel.", 1).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let agent_id = output.spawned[0].agent_id.clone();

        manager
            .wait(
                std::slice::from_ref(&agent_id),
                WaitMode::All,
                Some(Duration::from_millis(100)),
            )
            .await
            .expect("wait should complete");
        let cancelled = manager
            .cancel(std::slice::from_ref(&agent_id))
            .await
            .expect("cancel should return selected terminal status");

        assert_eq!(cancelled.agents.len(), 1);
        assert_ne!(cancelled.agents[0].status, SubagentStatusLabel::Cancelled);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_child_completion_does_not_overwrite_parent_cancellation() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            factory,
        );
        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Cancel before loop settles.", 2).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let agent_id = output.spawned[0].agent_id.clone();

        manager
            .cancel(std::slice::from_ref(&agent_id))
            .await
            .expect("cancel should succeed");
        tokio::task::yield_now().await;
        let snapshot = manager.snapshot().await;

        assert_eq!(
            snapshot
                .iter()
                .find(|agent| agent.agent_id == agent_id)
                .expect("child remains in snapshot")
                .status,
            SubagentStatusLabel::Cancelled
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_child_start_failure_handler_does_not_overwrite_parent_cancellation() {
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            Arc::new(FakeChildFactory::new()),
        );
        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Cancel before start failure handler.", 2)
                        .expect("valid"),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let agent_id = output.spawned[0].agent_id.clone();

        manager
            .cancel(std::slice::from_ref(&agent_id))
            .await
            .expect("cancel should succeed");
        manager
            .mark_failed_and_schedule(
                &agent_id,
                "child runtime start failed",
                error_info(
                    "subagent_start_error",
                    "synthetic failure after cancellation",
                ),
            )
            .await;
        let snapshot = manager.snapshot().await;

        assert_eq!(
            snapshot
                .iter()
                .find(|agent| agent.agent_id == agent_id)
                .expect("child remains in snapshot")
                .status,
            SubagentStatusLabel::Cancelled
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_wait_without_timeout_observes_status_changed_before_await() {
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            Arc::new(FakeChildFactory::new()),
        );
        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Already complete before wait.", 1).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let agent_id = output.spawned[0].agent_id.clone();

        loop {
            let snapshot = manager.snapshot().await;
            if snapshot
                .iter()
                .any(|agent| agent.agent_id == agent_id && agent.status.is_terminal())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let wait = tokio::time::timeout(
            Duration::from_millis(100),
            manager.wait(std::slice::from_ref(&agent_id), WaitMode::All, None),
        )
        .await
        .expect("wait should not hang after prior completion")
        .expect("wait should succeed");

        assert_eq!(wait.agents.len(), 1);
        assert!(wait.agents[0].status.is_terminal());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_builder_stores_manager_and_runtime_returns_subagent_snapshot() {
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            Arc::new(FakeChildFactory::new()),
        );
        manager
            .spawn(
                vec![SubagentTaskSpec::new("Snapshot task.", 1).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");

        let runtime = Runtime::builder(SessionId::new("parent").expect("valid id"))
            .subagent_manager(manager)
            .build()
            .expect("runtime builds");
        let snapshot = runtime
            .subagent_snapshot()
            .await
            .expect("manager snapshot is present");

        assert_eq!(snapshot.len(), 1);
        assert!(matches!(
            snapshot[0].status,
            SubagentStatusLabel::Running | SubagentStatusLabel::Completed
        ));
    }
}
