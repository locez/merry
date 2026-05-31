//! Runtime-owned parallel subagent tool contracts.

use crate::{
    AgentLoopConfig, AgentLoopStatus, RegisteredTool, Runtime, RuntimeError, StepContext,
    StepInput, TaskAnchor, ToolActionKind, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutionResult, ToolExecutor, ToolExecutorFuture,
};
use merry_core::{
    ErrorInfo, PendingToolCall, SubagentId, SubagentTaskId, ToolInputSchema, ToolName, ToolSpec,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

/// Provider-visible tool name for spawning bounded child agents.
pub(crate) const SPAWN_SUBAGENTS_TOOL_NAME: &str = "spawn_subagents";
/// Provider-visible tool name for waiting on child agent statuses/results.
pub(crate) const WAIT_SUBAGENTS_TOOL_NAME: &str = "wait_subagents";
/// Provider-visible tool name for cancelling child agents.
pub(crate) const CANCEL_SUBAGENTS_TOOL_NAME: &str = "cancel_subagents";

/// Maximum UTF-8 task text size accepted for one child task.
pub(crate) const MAX_TASK_BYTES: usize = 16 * 1024;
/// Default bounded child loop step count.
pub const DEFAULT_MAX_STEPS: u32 = 8;
/// Default maximum number of concurrent child agents.
pub(crate) const DEFAULT_MAX_THREADS: usize = 6;
/// Default child delegation depth.
pub(crate) const DEFAULT_MAX_DEPTH: u8 = 1;

/// Validation errors for subagent task contracts and configuration.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SubagentError {
    /// The delegated task text was blank.
    #[error("task must not be blank")]
    BlankTask,
    /// The delegated task text exceeded [`MAX_TASK_BYTES`].
    #[error("task is longer than the allowed maximum")]
    TaskTooLong,
    /// The delegated task has no allowed runtime steps.
    #[error("max_steps must be greater than zero")]
    ZeroMaxSteps,
    /// A path scope was not a normalized workspace-relative path.
    #[error("scope path must be relative and normalized: {path}")]
    InvalidScopePath {
        /// Rejected path display string.
        path: String,
    },
    /// Two child tasks may write the same path tree.
    #[error("overlapping write scope between task {first_index} and task {second_index}: {path}")]
    OverlappingWriteScope {
        /// Index of the first conflicting task.
        first_index: usize,
        /// Index of the second conflicting task.
        second_index: usize,
        /// Conflicting write path from the first task.
        path: String,
    },
    /// The subagent runtime was configured with no possible concurrency.
    #[error("subagent max_threads must be greater than zero")]
    ZeroMaxThreads,
    /// The subagent runtime was configured with no child depth.
    #[error("subagent max_depth must be greater than zero")]
    ZeroMaxDepth,
}

/// Runtime configuration for subagent delegation limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentConfig {
    max_threads: usize,
    max_depth: u8,
}

impl SubagentConfig {
    /// Creates validated subagent limit configuration.
    pub fn new(max_threads: usize, max_depth: u8) -> Result<Self, SubagentError> {
        if max_threads == 0 {
            return Err(SubagentError::ZeroMaxThreads);
        }
        if max_depth == 0 {
            return Err(SubagentError::ZeroMaxDepth);
        }
        Ok(Self {
            max_threads,
            max_depth,
        })
    }

    /// Returns the maximum number of child agents allowed to run concurrently.
    #[must_use]
    pub fn max_threads(self) -> usize {
        self.max_threads
    }

    /// Returns the maximum allowed child delegation depth.
    #[must_use]
    pub fn max_depth(self) -> u8 {
        self.max_depth
    }
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            max_threads: DEFAULT_MAX_THREADS,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

/// Parent-authored specification for a bounded child agent task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentTaskSpec {
    display_name: Option<String>,
    task: String,
    max_steps: u32,
    allowed_tools: Vec<ToolName>,
    read_scope: Vec<PathBuf>,
    write_scope: Vec<PathBuf>,
    forbidden_paths: Vec<PathBuf>,
    expected_output: Option<String>,
}

impl SubagentTaskSpec {
    /// Creates a child task specification from validated task text and step bound.
    pub fn new(task: impl Into<String>, max_steps: u32) -> Result<Self, SubagentError> {
        let task = task.into();
        validate_task_text(&task)?;
        if max_steps == 0 {
            return Err(SubagentError::ZeroMaxSteps);
        }
        Ok(Self {
            display_name: None,
            task,
            max_steps,
            allowed_tools: Vec::new(),
            read_scope: Vec::new(),
            write_scope: Vec::new(),
            forbidden_paths: Vec::new(),
            expected_output: None,
        })
    }

    /// Returns the child task prompt.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    /// Returns the maximum number of bounded child runtime steps.
    #[must_use]
    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }

    /// Returns the provider/tool names the child may use.
    #[must_use]
    pub fn allowed_tools(&self) -> &[ToolName] {
        &self.allowed_tools
    }

    /// Returns the optional compact display name.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    /// Returns the workspace-relative read scope.
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

    /// Returns the optional expected-output instruction.
    #[must_use]
    pub fn expected_output(&self) -> Option<&str> {
        self.expected_output.as_deref()
    }

    /// Replaces the child's allowed tool list.
    #[must_use]
    pub fn with_allowed_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = ToolName>,
    {
        self.allowed_tools = tools.into_iter().collect();
        self
    }

    /// Replaces the optional display name, treating blank names as absent.
    #[must_use]
    pub fn with_display_name(mut self, display_name: Option<String>) -> Self {
        self.display_name = display_name.filter(|value| !value.trim().is_empty());
        self
    }

    /// Replaces the read scope after validating each path.
    pub fn with_read_scope<I, P>(mut self, paths: I) -> Result<Self, SubagentError>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.read_scope = validate_scope_paths(paths)?;
        Ok(self)
    }

    /// Replaces the write scope after validating each path.
    pub fn with_write_scope<I, P>(mut self, paths: I) -> Result<Self, SubagentError>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.write_scope = validate_scope_paths(paths)?;
        Ok(self)
    }

    /// Replaces the forbidden path scope after validating each path.
    pub fn with_forbidden_paths<I, P>(mut self, paths: I) -> Result<Self, SubagentError>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.forbidden_paths = validate_scope_paths(paths)?;
        Ok(self)
    }

    /// Replaces the optional expected-output instruction, treating blank text as absent.
    #[must_use]
    pub fn with_expected_output(mut self, expected_output: Option<String>) -> Self {
        self.expected_output = expected_output.filter(|value| !value.trim().is_empty());
        self
    }
}

fn validate_task_text(task: &str) -> Result<(), SubagentError> {
    if task.trim().is_empty() {
        return Err(SubagentError::BlankTask);
    }
    if task.len() > MAX_TASK_BYTES {
        return Err(SubagentError::TaskTooLong);
    }
    Ok(())
}

fn validate_scope_paths<I, P>(paths: I) -> Result<Vec<PathBuf>, SubagentError>
where
    I: IntoIterator<Item = P>,
    P: Into<PathBuf>,
{
    let paths = paths
        .into_iter()
        .map(|path| validate_scope_path(path.into()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(paths.into_iter().collect())
}

fn validate_scope_path(path: PathBuf) -> Result<PathBuf, SubagentError> {
    let path_label = path.display().to_string();
    let Some(path_text) = path.to_str() else {
        return Err(SubagentError::InvalidScopePath { path: path_label });
    };

    if path_text.is_empty()
        || path.is_absolute()
        || path_text.contains('\\')
        || path_text.chars().any(char::is_control)
    {
        return Err(SubagentError::InvalidScopePath { path: path_label });
    }

    if path_text
        .split('/')
        .any(|segment| matches!(segment, "" | "." | ".."))
    {
        return Err(SubagentError::InvalidScopePath { path: path_label });
    }

    let mut saw_component = false;
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if value.to_str().is_none() {
                    return Err(SubagentError::InvalidScopePath { path: path_label });
                }
                saw_component = true;
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(SubagentError::InvalidScopePath { path: path_label });
            }
        }
    }

    if !saw_component {
        return Err(SubagentError::InvalidScopePath { path: path_label });
    }

    Ok(path)
}

/// Rejects child batches where two write scopes overlap.
pub fn validate_no_write_scope_conflicts(tasks: &[SubagentTaskSpec]) -> Result<(), SubagentError> {
    for (first_index, first) in tasks.iter().enumerate() {
        for (second_offset, second) in tasks[first_index + 1..].iter().enumerate() {
            let second_index = first_index + 1 + second_offset;
            for first_path in first.write_scope() {
                for second_path in second.write_scope() {
                    if paths_overlap(first_path, second_path) {
                        return Err(SubagentError::OverlappingWriteScope {
                            first_index,
                            second_index,
                            path: first_path.display().to_string(),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Returns true when two relative paths name the same path or parent/child trees.
#[must_use]
fn paths_overlap(first: &Path, second: &Path) -> bool {
    first == second || first.starts_with(second) || second.starts_with(first)
}

/// Provider-visible input for `spawn_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentsInput {
    /// Child tasks to spawn.
    pub tasks: Vec<SpawnSubagentTaskInput>,
    /// Optional caller-specified concurrency cap for this batch.
    pub max_concurrency: Option<usize>,
}

/// Provider-visible task item for `spawn_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentTaskInput {
    /// Delegated task prompt.
    pub task: String,
    /// Optional compact display name.
    pub display_name: Option<String>,
    /// Optional maximum child runtime steps.
    pub max_steps: Option<u32>,
    /// Optional provider/tool names the child may use.
    pub allowed_tools: Option<Vec<ToolName>>,
    /// Optional workspace-relative read scope.
    pub read_scope: Option<Vec<String>>,
    /// Optional workspace-relative write scope.
    pub write_scope: Option<Vec<String>>,
    /// Optional workspace-relative paths the child must not access.
    pub forbidden_paths: Option<Vec<String>>,
    /// Optional expected output instruction.
    pub expected_output: Option<String>,
}

/// Provider-visible output for `spawn_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentsOutput {
    /// Accepted child tasks.
    pub spawned: Vec<SpawnedSubagentView>,
    /// Rejected child tasks with compact reasons.
    pub rejected: Vec<RejectedSubagentView>,
}

/// Compact provider-visible view of an accepted child task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnedSubagentView {
    /// Runtime-owned child agent id.
    pub agent_id: SubagentId,
    /// Runtime-owned child task id.
    pub task_id: SubagentTaskId,
    /// Optional compact display name.
    pub display_name: Option<String>,
    /// Compact status label.
    pub status: SpawnedSubagentStatusLabel,
    /// Compact task anchor assigned to the child runtime.
    pub task_anchor: String,
    /// Workspace-relative paths the child may read.
    pub read_scope: Vec<String>,
    /// Workspace-relative paths the child may write.
    pub write_scope: Vec<String>,
}

/// Provider-visible status labels possible immediately after spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpawnedSubagentStatusLabel {
    /// Child task is accepted but has not begun running.
    Queued,
    /// Child task is currently running.
    Running,
}

/// Compact provider-visible view of a rejected child task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RejectedSubagentView {
    /// Index into the requested task batch.
    pub task_index: usize,
    /// Stable human-readable rejection reason.
    pub reason: String,
}

/// Wait behavior for `wait_subagents`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitMode {
    /// Return after any selected child reaches a terminal status.
    Any,
    /// Return after all selected children reach terminal statuses.
    All,
}

/// Provider-visible input for `wait_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitSubagentsInput {
    /// Child agent ids to inspect or wait on.
    pub agent_ids: Vec<SubagentId>,
    /// Optional wait completion mode.
    pub mode: Option<WaitMode>,
    /// Optional wait timeout in milliseconds.
    pub timeout_ms: Option<u64>,
}

/// Provider-visible input for `cancel_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelSubagentsInput {
    /// Child agent ids to cancel.
    pub agent_ids: Vec<SubagentId>,
}

/// Provider-visible compact status output for `wait_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitSubagentsOutput {
    /// Compact status views for selected child agents.
    pub agents: Vec<SubagentStatusView>,
}

impl WaitSubagentsOutput {
    /// Creates wait output from compact child status views.
    #[must_use]
    pub fn new(agents: Vec<SubagentStatusView>) -> Self {
        Self { agents }
    }
}

/// Compact child lifecycle status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatusLabel {
    /// Child task is accepted but has not begun running.
    Queued,
    /// Child task is currently running.
    Running,
    /// Child task completed successfully.
    Completed,
    /// Child task failed.
    Failed,
    /// Child task was cancelled.
    Cancelled,
}

impl SubagentStatusLabel {
    /// Returns the stable provider-visible status string.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Provider-visible compact child status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubagentStatusView {
    /// Runtime-owned child agent id.
    pub agent_id: SubagentId,
    /// Runtime-owned child task id.
    pub task_id: SubagentTaskId,
    /// Compact lifecycle status.
    pub status: SubagentStatusLabel,
    /// Compact result or progress summary.
    pub summary: String,
    /// Shared-workspace output paths for exact follow-up reads.
    pub output_paths: Vec<String>,
    /// Shared-workspace paths changed by the child.
    pub changed_paths: Vec<String>,
    /// Optional compact failure/cancellation diagnostics.
    pub diagnostics: Option<ErrorInfo>,
}

impl SubagentStatusView {
    /// Creates a completed child status with compact paths and no transcript.
    #[must_use]
    pub fn completed(
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        summary: impl Into<String>,
        output_paths: Vec<String>,
        changed_paths: Vec<String>,
    ) -> Self {
        Self {
            agent_id,
            task_id,
            status: SubagentStatusLabel::Completed,
            summary: summary.into(),
            output_paths,
            changed_paths,
            diagnostics: None,
        }
    }
}

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
    /// Delegation depth assigned to this child.
    pub depth: u8,
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
            output_paths: self.output_paths.clone(),
            changed_paths: self.changed_paths.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }
}

impl SubagentStatusView {
    fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

impl SubagentStatusLabel {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Runtime-owned executor for the provider-visible `spawn_subagents` tool.
#[derive(Clone)]
struct SpawnSubagentsExecutor {
    manager: SubagentManager,
}

impl SpawnSubagentsExecutor {
    /// Creates a spawn executor backed by the shared subagent manager.
    #[must_use]
    pub fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for SpawnSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<SpawnSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            let tasks = match input
                .tasks
                .into_iter()
                .map(task_spec_from_input)
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(tasks) => tasks,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            let output = self
                .manager
                .spawn(
                    tasks,
                    input.max_concurrency,
                    context.cancellation_token().clone(),
                )
                .await
                .map_err(infrastructure_error)?;

            succeeded_json_output(SPAWN_SUBAGENTS_TOOL_NAME, &output)
        })
    }
}

/// Runtime-owned executor for the provider-visible `wait_subagents` tool.
#[derive(Clone)]
struct WaitSubagentsExecutor {
    manager: SubagentManager,
}

impl WaitSubagentsExecutor {
    /// Creates a wait executor backed by the shared subagent manager.
    #[must_use]
    pub fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for WaitSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<WaitSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            let timeout = input.timeout_ms.map(Duration::from_millis);
            let wait = self.manager.wait(
                &input.agent_ids,
                input.mode.unwrap_or(WaitMode::All),
                timeout,
            );
            let output = tokio::select! {
                biased;
                () = context.cancellation_token().cancelled() => {
                    return Err(ToolExecutionError::Cancelled);
                }
                output = wait => output.map_err(infrastructure_error)?,
            };

            succeeded_json_output(WAIT_SUBAGENTS_TOOL_NAME, &output)
        })
    }
}

/// Runtime-owned executor for the provider-visible `cancel_subagents` tool.
#[derive(Clone)]
struct CancelSubagentsExecutor {
    manager: SubagentManager,
}

impl CancelSubagentsExecutor {
    /// Creates a cancel executor backed by the shared subagent manager.
    #[must_use]
    pub fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for CancelSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input = match input_from_call::<CancelSubagentsInput>(&call) {
                Ok(input) => input,
                Err(error) => {
                    return Ok(invalid_subagent_arguments_outcome(
                        call.name().as_str(),
                        error,
                    ));
                }
            };
            let output = self
                .manager
                .cancel(&input.agent_ids)
                .await
                .map_err(infrastructure_error)?;

            succeeded_json_output(CANCEL_SUBAGENTS_TOOL_NAME, &output)
        })
    }
}

/// Returns provider-visible subagent tool specs.
pub fn subagent_tool_specs() -> Result<[ToolSpec; 3], merry_core::CoreError> {
    Ok([
        tool_spec::<SpawnSubagentsInput>(
            SPAWN_SUBAGENTS_TOOL_NAME,
            "Spawn bounded child agents for parallel delegated tasks.",
        )?,
        tool_spec::<WaitSubagentsInput>(
            WAIT_SUBAGENTS_TOOL_NAME,
            "Inspect or wait for child agent statuses and compact results.",
        )?,
        tool_spec::<CancelSubagentsInput>(
            CANCEL_SUBAGENTS_TOOL_NAME,
            "Cancel selected child agents.",
        )?,
    ])
}

/// Returns provider-visible subagent tool specs with runtime-owned executors.
pub fn subagent_registered_tools(
    manager: SubagentManager,
) -> Result<[RegisteredTool; 3], merry_core::CoreError> {
    let [spawn_spec, wait_spec, cancel_spec] = subagent_tool_specs()?;
    Ok([
        RegisteredTool::new(
            spawn_spec,
            Arc::new(SpawnSubagentsExecutor::new(manager.clone())),
            ToolActionKind::RuntimeControl,
        ),
        RegisteredTool::read_only(
            wait_spec,
            Arc::new(WaitSubagentsExecutor::new(manager.clone())),
        ),
        RegisteredTool::new(
            cancel_spec,
            Arc::new(CancelSubagentsExecutor::new(manager)),
            ToolActionKind::RuntimeControl,
        ),
    ])
}

fn tool_spec<T>(name: &str, description: &str) -> Result<ToolSpec, merry_core::CoreError>
where
    T: JsonSchema,
{
    ToolSpec::new(
        ToolName::new(name)?,
        description,
        ToolInputSchema::new(schemars::schema_for!(T))?,
    )
}

fn input_from_call<T>(call: &PendingToolCall) -> Result<T, InvalidSubagentToolArguments>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::Object(
        call.arguments().as_object().clone(),
    ))
    .map_err(|error| InvalidSubagentToolArguments::new(format!("invalid tool input: {error}")))
}

fn task_spec_from_input(
    input: SpawnSubagentTaskInput,
) -> Result<SubagentTaskSpec, InvalidSubagentToolArguments> {
    let SpawnSubagentTaskInput {
        task,
        display_name,
        max_steps,
        allowed_tools,
        read_scope,
        write_scope,
        forbidden_paths,
        expected_output,
    } = input;

    SubagentTaskSpec::new(task, max_steps.unwrap_or(DEFAULT_MAX_STEPS))
        .map_err(InvalidSubagentToolArguments::from)?
        .with_display_name(display_name)
        .with_allowed_tools(allowed_tools.unwrap_or_default())
        .with_read_scope(read_scope.unwrap_or_default())
        .map_err(InvalidSubagentToolArguments::from)?
        .with_write_scope(write_scope.unwrap_or_default())
        .map_err(InvalidSubagentToolArguments::from)?
        .with_forbidden_paths(forbidden_paths.unwrap_or_default())
        .map_err(InvalidSubagentToolArguments::from)
        .map(|task| task.with_expected_output(expected_output))
}

fn succeeded_json_output<T>(tool_name: &str, output: &T) -> ToolExecutionResult
where
    T: Serialize,
{
    let content = serde_json::to_string(output).map_err(|error| {
        ToolExecutionError::infrastructure(format!(
            "failed to serialize {tool_name} output: {error}"
        ))
    })?;
    Ok(ToolExecutionOutcome::succeeded_json(content))
}

fn infrastructure_error(error: impl std::fmt::Display) -> ToolExecutionError {
    ToolExecutionError::infrastructure(error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InvalidSubagentToolArguments {
    message: String,
}

impl InvalidSubagentToolArguments {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: sanitize_diagnostic_message(message.into()),
        }
    }

    fn message(&self) -> &str {
        &self.message
    }
}

impl From<SubagentError> for InvalidSubagentToolArguments {
    fn from(error: SubagentError) -> Self {
        Self::new(error.to_string())
    }
}

const SUBAGENT_INVALID_ARGUMENTS_CODE: &str = "subagent_invalid_arguments";

fn invalid_subagent_arguments_outcome(
    tool_name: &str,
    error: InvalidSubagentToolArguments,
) -> ToolExecutionOutcome {
    let payload = serde_json::json!({
        "ok": false,
        "tool": tool_name,
        "error": {
            "code": SUBAGENT_INVALID_ARGUMENTS_CODE,
            "message": error.message(),
        },
        "recovery": {
            "input_contract": "Provide arguments matching the subagent tool input schema.",
            "scope_contract": "Paths must be normalized workspace-relative paths.",
        }
    });

    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new(SUBAGENT_INVALID_ARGUMENTS_CODE, error.message())
            .expect("static subagent diagnostic code is valid"),
    )
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
        let config = match AgentLoopConfig::new(launch.task.max_steps() as usize) {
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
                Ok(result) => apply_loop_status(agent, result.status()),
                Err(error) => {
                    agent.status = SubagentStatusLabel::Failed;
                    agent.summary = "child runtime error".to_owned();
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

fn apply_loop_status(agent: &mut ManagedSubagent, status: &AgentLoopStatus) {
    match status {
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
                .contains("max_steps must be greater than zero")
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
mod tool_tests {
    use super::*;
    use crate::{ArtifactContent, Runtime, ToolActionKind, ToolExecutionContext, ToolExecutor};
    use merry_core::{PendingToolCall, SessionId, ToolCallArguments, ToolCallId};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex as StdMutex};

    #[derive(Clone, Default)]
    struct CapturingChildFactory {
        inputs: Arc<StdMutex<Vec<ChildRuntimeInput>>>,
    }

    impl CapturingChildFactory {
        fn inputs(&self) -> Vec<ChildRuntimeInput> {
            self.inputs
                .lock()
                .expect("inputs mutex is not poisoned")
                .clone()
        }
    }

    impl ChildRuntimeFactory for CapturingChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            self.inputs
                .lock()
                .expect("inputs mutex is not poisoned")
                .push(input.clone());

            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .build()
        }
    }

    fn manager(factory: Arc<dyn ChildRuntimeFactory>) -> SubagentManager {
        SubagentManager::new(
            SessionId::new("parent").expect("valid session id"),
            SubagentConfig::default(),
            factory,
        )
    }

    fn pending_call(name: &str, arguments: Value) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new("call-1").expect("valid call id"),
            ToolName::new(name).expect("valid tool name"),
            ToolCallArguments::try_from(arguments).expect("object arguments"),
        )
    }

    fn outcome_json(outcome: &crate::ToolExecutionOutcome) -> Value {
        let ArtifactContent::Json(content) = outcome.content() else {
            panic!("expected JSON tool outcome");
        };
        serde_json::from_str(content).expect("outcome contains valid JSON")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_returns_structured_output_and_preserves_task_input() {
        let factory = Arc::new(CapturingChildFactory::default());
        let executor = SpawnSubagentsExecutor::new(manager(factory.clone()));
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({
                "max_concurrency": 1,
                "tasks": [{
                    "task": "Review the runtime module.",
                    "display_name": "Runtime review",
                    "max_steps": 3,
                    "allowed_tools": ["workspace_read_file"],
                    "read_scope": ["crates/merry-runtime/src"],
                    "write_scope": ["tmp/subagent-output"],
                    "forbidden_paths": ["target", ".git"],
                    "expected_output": "Return a compact findings list."
                }]
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("spawn execution should succeed");
        let output: SpawnSubagentsOutput =
            serde_json::from_value(outcome_json(&outcome)).expect("spawn output is structured");
        let captured = factory.inputs();

        assert_eq!(output.spawned.len(), 1);
        assert!(output.rejected.is_empty());
        assert_eq!(
            output.spawned[0].display_name.as_deref(),
            Some("Runtime review")
        );
        assert_eq!(
            output.spawned[0].read_scope,
            vec!["crates/merry-runtime/src".to_owned()]
        );
        assert_eq!(
            output.spawned[0].write_scope,
            vec!["tmp/subagent-output".to_owned()]
        );
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].task.display_name(), Some("Runtime review"));
        assert_eq!(captured[0].task.max_steps(), 3);
        assert_eq!(
            captured[0].task.allowed_tools(),
            &[ToolName::new("workspace_read_file").expect("valid tool name")]
        );
        assert_eq!(
            captured[0].allowed_tools,
            vec![ToolName::new("workspace_read_file").expect("valid tool name")]
        );
        assert_eq!(
            captured[0].task.read_scope(),
            &[PathBuf::from("crates/merry-runtime/src")]
        );
        assert_eq!(
            captured[0].task.write_scope(),
            &[PathBuf::from("tmp/subagent-output")]
        );
        assert_eq!(
            captured[0].task.forbidden_paths(),
            &[PathBuf::from(".git"), PathBuf::from("target")]
        );
        assert_eq!(
            captured[0].task.expected_output(),
            Some("Return a compact findings list.")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_defaults_max_steps_when_omitted() {
        let factory = Arc::new(CapturingChildFactory::default());
        let executor = SpawnSubagentsExecutor::new(manager(factory.clone()));
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({
                "tasks": [{
                    "task": "Use the default step limit."
                }]
            }),
        );

        executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("spawn execution should succeed");
        let captured = factory.inputs();

        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].task.max_steps(), DEFAULT_MAX_STEPS);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_tool_returns_status_output() {
        let manager = manager(Arc::new(CapturingChildFactory::default()));
        let spawn = manager
            .spawn(
                vec![SubagentTaskSpec::new("Stay queued for wait.", 2).expect("valid task")],
                Some(0),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let executor = WaitSubagentsExecutor::new(manager);
        let call = pending_call(
            WAIT_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [spawn.spawned[0].agent_id],
                "timeout_ms": 0
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("wait execution should succeed");
        let output: WaitSubagentsOutput =
            serde_json::from_value(outcome_json(&outcome)).expect("wait output is structured");

        assert_eq!(output.agents.len(), 1);
        assert_eq!(output.agents[0].status, SubagentStatusLabel::Queued);
        assert_eq!(output.agents[0].summary, "child queued");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_tool_returns_cancelled_status_output() {
        let manager = manager(Arc::new(CapturingChildFactory::default()));
        let spawn = manager
            .spawn(
                vec![SubagentTaskSpec::new("Cancel queued child.", 2).expect("valid task")],
                Some(0),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let executor = CancelSubagentsExecutor::new(manager);
        let call = pending_call(
            CANCEL_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [spawn.spawned[0].agent_id]
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("cancel execution should succeed");
        let output: WaitSubagentsOutput =
            serde_json::from_value(outcome_json(&outcome)).expect("cancel output is structured");

        assert_eq!(output.agents.len(), 1);
        assert_eq!(output.agents[0].status, SubagentStatusLabel::Cancelled);
        assert_eq!(output.agents[0].summary, "child cancelled by parent");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_invalid_task_or_path_returns_failed_outcome() {
        let executor =
            SpawnSubagentsExecutor::new(manager(Arc::new(CapturingChildFactory::default())));

        for arguments in [
            json!({ "tasks": [{ "task": " " }] }),
            json!({ "tasks": [{ "task": "Bad path.", "read_scope": ["../secret"] }] }),
            json!({ "tasks": [{ "task": "Bad control path.", "read_scope": ["bad\npath"] }] }),
        ] {
            let call = pending_call(SPAWN_SUBAGENTS_TOOL_NAME, arguments);
            let outcome = executor
                .execute(call, ToolExecutionContext::default())
                .await
                .expect("invalid input should resolve as failed tool outcome");
            let diagnostic = outcome
                .diagnostic()
                .expect("failed subagent arguments should include diagnostic");
            let payload = outcome_json(&outcome);

            assert_eq!(outcome.status(), merry_core::ToolCallResultStatus::Failed);
            assert_eq!(diagnostic.code(), SUBAGENT_INVALID_ARGUMENTS_CODE);
            assert_eq!(payload["ok"], false);
            assert_eq!(payload["error"]["code"], SUBAGENT_INVALID_ARGUMENTS_CODE);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_input_shape_errors_return_failed_outcome() {
        let executor =
            WaitSubagentsExecutor::new(manager(Arc::new(CapturingChildFactory::default())));
        let call = pending_call(
            WAIT_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [],
                "unexpected": true
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("invalid provider-visible input should resolve as failed outcome");
        let diagnostic = outcome
            .diagnostic()
            .expect("failed input should include diagnostic");
        let payload = outcome_json(&outcome);

        assert_eq!(outcome.status(), merry_core::ToolCallResultStatus::Failed);
        assert_eq!(diagnostic.code(), SUBAGENT_INVALID_ARGUMENTS_CODE);
        assert_eq!(payload["tool"], WAIT_SUBAGENTS_TOOL_NAME);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_tool_without_timeout_honors_cancellation() {
        let manager = manager(Arc::new(CapturingChildFactory::default()));
        let spawn = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Stay queued until cancellation.", 2)
                        .expect("valid task"),
                ],
                Some(0),
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");
        let executor = WaitSubagentsExecutor::new(manager);
        let call = pending_call(
            WAIT_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [spawn.spawned[0].agent_id],
                "mode": "all"
            }),
        );
        let token = CancellationToken::new();
        token.cancel();

        let error = executor
            .execute(call, ToolExecutionContext::new(token))
            .await
            .expect_err("pre-cancelled wait should not resolve");

        assert!(matches!(error, ToolExecutionError::Cancelled));
    }

    #[test]
    fn registered_subagent_tools_are_named_with_control_policy() {
        let tools = subagent_registered_tools(manager(Arc::new(CapturingChildFactory::default())))
            .expect("registered subagent tools should build");

        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].spec().name().as_str(), SPAWN_SUBAGENTS_TOOL_NAME);
        assert_eq!(tools[1].spec().name().as_str(), WAIT_SUBAGENTS_TOOL_NAME);
        assert_eq!(tools[2].spec().name().as_str(), CANCEL_SUBAGENTS_TOOL_NAME);
        assert_eq!(tools[0].action_kind(), ToolActionKind::RuntimeControl);
        assert_eq!(tools[1].action_kind(), ToolActionKind::ReadOnly);
        assert_eq!(tools[2].action_kind(), ToolActionKind::RuntimeControl);
    }
}

#[cfg(test)]
mod manager_tests {
    use super::*;
    use crate::Runtime;
    use merry_core::SessionId;
    use std::{
        sync::{
            Arc,
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
