//! Runtime-owned parallel subagent tool contracts.

use crate::{
    AgentLoopConfig, AgentLoopResult, AgentLoopStatus, AgentLoopStreamMessage, ArtifactContent,
    PlanSubagentControl, Runtime, RuntimeError, StepContext, StepInput, TaskAnchor,
    plan::{PlanController, SubagentPlanUpdateInput},
};
use activity::SubagentActivityReducer;
use futures_util::future::BoxFuture;
use merry_core::{
    ErrorInfo, PlanBindingId, PlanLinkSnapshot, PlanLinkStatus, RuntimeJournalEvent,
    RuntimeJournalPayload, SubagentActivityPhase, SubagentId, SubagentTaskId, ToolCallResultStatus,
    ToolName,
};
use merry_llm::GenerationConfig;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

mod activity;
mod protocol;
mod spec;
mod tools;

pub use activity::{SubagentActivityHub, SubagentActivityReceiver};
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

/// Opaque child capability for reading and updating the subtree below one
/// active Plan link.
///
/// The capability carries no public controller or identity fields. Runtime
/// construction receives it from the parent link adapter, and scoped tool
/// execution is the only consumer-facing behavior.
#[derive(Clone)]
pub struct PlanSubagentScope(crate::plan::PlanSubagentScope);

impl PlanSubagentScope {
    pub(crate) fn from_internal(scope: crate::plan::PlanSubagentScope) -> Self {
        Self(scope)
    }

    pub(crate) async fn read(
        &self,
    ) -> Result<merry_core::PlanSnapshot, crate::PlanControllerError> {
        self.0.read().await
    }

    pub(crate) async fn update_plan(
        &self,
        input: SubagentPlanUpdateInput,
    ) -> Result<crate::PlanUpdateOutput, crate::PlanControllerError> {
        self.0.update_plan(input).await
    }
}

/// Provider-visible tool name for spawning bounded child agents.
pub(crate) const SPAWN_SUBAGENTS_TOOL_NAME: &str = "spawn_subagents";
/// Provider-visible tool name for waiting on child agent statuses/results.
pub(crate) const WAIT_SUBAGENTS_TOOL_NAME: &str = "wait_subagents";
/// Provider-visible tool name for cancelling child agents.
pub(crate) const CANCEL_SUBAGENTS_TOOL_NAME: &str = "cancel_subagents";
const WORKSPACE_PATCH_TOOL_NAME: &str = "workspace_patch";

/// Runtime construction input for one bounded child agent.
#[derive(Clone)]
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
    /// Child-scoped model generation controls selected by the parent agent.
    pub generation_config: GenerationConfig,
    /// Optional compatibility control for an explicitly Plan-bound child.
    pub plan_subagent_control: Option<PlanSubagentControl>,
    /// Optional opaque capability for the subtree below the active Plan link.
    pub plan_subagent_scope: Option<PlanSubagentScope>,
    /// Optional runtime-owned Plan link for this child execution.
    pub plan_link: Option<PlanLinkSnapshot>,
    /// Runtime-owned link adapter used when this child delegates further.
    pub plan_link_runtime: Option<Arc<dyn PlanLinkRuntime>>,
    /// Optional shared runtime-owned activity hub for this child and descendants.
    pub activity_hub: Option<Arc<SubagentActivityHub>>,
}

/// Parent-authored workspace scope carried into child runtime construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildWorkspaceScope {
    read_scope: Vec<PathBuf>,
    write_scope: Vec<PathBuf>,
    forbidden_paths: Vec<PathBuf>,
}

impl ChildWorkspaceScope {
    /// Returns the unrestricted workspace scope inherited by a root agent.
    #[must_use]
    pub fn workspace_root() -> Self {
        Self {
            read_scope: vec![PathBuf::from(".")],
            write_scope: vec![PathBuf::from(".")],
            forbidden_paths: Vec::new(),
        }
    }

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

#[derive(Debug, Clone)]
struct ParentCapabilities {
    allowed_tools: Vec<ToolName>,
    workspace_scope: ChildWorkspaceScope,
}

/// Object-safe factory for constructing bounded child runtimes.
pub trait ChildRuntimeFactory: Send + Sync {
    /// Builds a child runtime from runtime-owned delegation input.
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError>;
}

/// Runtime-owned bridge for updating a Plan link without exposing the Plan
/// controller or its persistence protocol to a subagent runtime.
pub trait PlanLinkRuntime: Send + Sync {
    /// Binds a newly allocated subagent to a Plan node identified by client key.
    fn bind_subagent<'a>(
        &'a self,
        client_key: String,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        now_ms: u64,
    ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>>;

    /// Updates the runtime-derived terminal state for an existing link.
    fn update_subagent_link<'a>(
        &'a self,
        binding_id: PlanBindingId,
        status: PlanLinkStatus,
        now_ms: u64,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// Creates a scoped Plan capability only for an active, non-superseded link.
    fn scope_for_link<'a>(
        &'a self,
        _link: &'a PlanLinkSnapshot,
    ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
        Box::pin(async { Ok(None) })
    }
}

struct PlanControllerLinkRuntime(PlanController);

impl PlanLinkRuntime for PlanControllerLinkRuntime {
    fn bind_subagent<'a>(
        &'a self,
        client_key: String,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        now_ms: u64,
    ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
        Box::pin(async move {
            self.0
                .bind_subagent(client_key, agent_id, task_id, now_ms)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn update_subagent_link<'a>(
        &'a self,
        binding_id: PlanBindingId,
        status: PlanLinkStatus,
        now_ms: u64,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .update_subagent_link(binding_id, status, now_ms)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    fn scope_for_link<'a>(
        &'a self,
        link: &'a PlanLinkSnapshot,
    ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
        Box::pin(async move {
            if link.status != PlanLinkStatus::Active || link.superseded_by.is_some() {
                return Ok(None);
            }
            let Some(snapshot) = self.0.snapshot().await.map_err(|error| error.to_string())? else {
                return Ok(None);
            };
            let Some(current_link) =
                snapshot
                    .nodes
                    .iter()
                    .flat_map(|node| &node.links)
                    .find(|current| {
                        current.plan_id == link.plan_id
                            && current.node_id == link.node_id
                            && current.binding_id == link.binding_id
                            && current.subagent_id == link.subagent_id
                            && current.task_id == link.task_id
                    })
            else {
                return Ok(None);
            };
            if current_link.status != PlanLinkStatus::Active || current_link.superseded_by.is_some()
            {
                return Ok(None);
            }
            Ok(Some(PlanSubagentScope::from_internal(
                self.0.subagent_scope(
                    current_link.plan_id.clone(),
                    current_link.node_id.clone(),
                    current_link.binding_id.clone(),
                ),
            )))
        })
    }
}

pub(crate) fn plan_link_runtime_for_controller(
    controller: PlanController,
) -> Arc<dyn PlanLinkRuntime> {
    Arc::new(PlanControllerLinkRuntime(controller))
}

/// Runtime-owned manager for bounded child agent execution.
#[derive(Clone)]
pub struct SubagentManager {
    enabled: Arc<AtomicBool>,
    max_threads: Arc<AtomicUsize>,
    has_agents: Arc<AtomicBool>,
    factory: Arc<dyn ChildRuntimeFactory>,
    state: Arc<Mutex<SubagentManagerState>>,
    notify: Arc<Notify>,
    next_id: Arc<AtomicU64>,
    next_batch_id: Arc<AtomicU64>,
    depth: u8,
    max_depth: u8,
    plan_link_runtime: Arc<StdMutex<Option<Arc<dyn PlanLinkRuntime>>>>,
    parent_capabilities: Arc<StdMutex<Option<ParentCapabilities>>>,
    activity_hub: Arc<StdMutex<Option<Arc<SubagentActivityHub>>>>,
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
    task_id: SubagentTaskId,
    task: SubagentTaskSpec,
    task_anchor: TaskAnchor,
    cancellation_token: CancellationToken,
    plan_link: Option<PlanLinkSnapshot>,
}

#[derive(Clone)]
struct ChildScheduler {
    factory: Arc<dyn ChildRuntimeFactory>,
    state: Arc<Mutex<SubagentManagerState>>,
    notify: Arc<Notify>,
    enabled: Arc<AtomicBool>,
    max_threads: Arc<AtomicUsize>,
    depth: u8,
    plan_link_runtime: Arc<StdMutex<Option<Arc<dyn PlanLinkRuntime>>>>,
    activity_hub: Arc<StdMutex<Option<Arc<SubagentActivityHub>>>>,
}

impl ChildScheduler {
    fn effective_max_threads(&self) -> usize {
        if self.enabled.load(Ordering::Acquire) {
            self.max_threads.load(Ordering::Acquire)
        } else {
            0
        }
    }

    fn attached_activity_hub(&self) -> Option<Arc<SubagentActivityHub>> {
        self.activity_hub
            .lock()
            .expect("subagent activity hub mutex is not poisoned")
            .clone()
    }
}

struct ChildLoopLaunch {
    agent_id: SubagentId,
    task_id: SubagentTaskId,
    task: SubagentTaskSpec,
    token: CancellationToken,
    runtime: Runtime,
    generation_config: GenerationConfig,
    activity_hub: Option<Arc<SubagentActivityHub>>,
}

struct ChildErrorTransition {
    claimed: bool,
    to_start: Vec<ReservedChildStart>,
}

enum PlanScopeLookupError {
    Cancelled,
    Lookup(String),
}

async fn lookup_plan_subagent_scope(
    plan_link: Option<&PlanLinkSnapshot>,
    plan_link_runtime: Option<&Arc<dyn PlanLinkRuntime>>,
    token: &CancellationToken,
) -> Result<Option<PlanSubagentScope>, PlanScopeLookupError> {
    if token.is_cancelled() {
        return Err(PlanScopeLookupError::Cancelled);
    }
    let Some((link, runtime)) = plan_link.zip(plan_link_runtime) else {
        return Ok(None);
    };
    let lookup = runtime.scope_for_link(link);
    let result = tokio::select! {
        _ = token.cancelled() => return Err(PlanScopeLookupError::Cancelled),
        result = lookup => result,
    };
    if token.is_cancelled() {
        return Err(PlanScopeLookupError::Cancelled);
    }
    result.map_err(PlanScopeLookupError::Lookup)
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
    plan_link: Option<PlanLinkSnapshot>,
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
            has_agents: Arc::new(AtomicBool::new(false)),
            factory,
            state: Arc::new(Mutex::new(SubagentManagerState::default())),
            notify: Arc::new(Notify::new()),
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

    fn attached_plan_link_runtime(&self) -> Option<Arc<dyn PlanLinkRuntime>> {
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

    fn attached_activity_hub(&self) -> Option<Arc<SubagentActivityHub>> {
        self.activity_hub
            .lock()
            .expect("subagent activity hub mutex is not poisoned")
            .clone()
    }

    fn apply_parent_capabilities(
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

    pub(crate) fn is_tool_visible(&self, tool_name: &ToolName) -> bool {
        match tool_name.as_str() {
            SPAWN_SUBAGENTS_TOOL_NAME => self.enabled.load(Ordering::Acquire),
            WAIT_SUBAGENTS_TOOL_NAME | CANCEL_SUBAGENTS_TOOL_NAME => {
                self.enabled.load(Ordering::Acquire) || self.has_agents.load(Ordering::Acquire)
            }
            _ => true,
        }
    }

    pub(crate) async fn update_policy(
        &self,
        enabled: bool,
        config: SubagentConfig,
    ) -> Result<(), RuntimeError> {
        self.max_threads
            .store(config.max_threads(), Ordering::Release);
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
            let plan_link = match (task.plan_task(), self.attached_plan_link_runtime()) {
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
                return Ok(WaitSubagentsOutput::with_wait_state(agents, true, false));
            }

            match deadline {
                Some(deadline) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Ok(WaitSubagentsOutput::with_wait_state(agents, false, true));
                    }
                    if tokio::time::timeout_at(deadline, notified.as_mut())
                        .await
                        .is_err()
                    {
                        let agents = self.status_for(agent_ids).await;
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

    async fn start_child(
        &self,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        task: SubagentTaskSpec,
        task_anchor: TaskAnchor,
        token: CancellationToken,
        plan_link: Option<PlanLinkSnapshot>,
    ) {
        let child_session_id = child_session_id();
        let generation_config = generation_config_for_child_task(&task);
        let plan_link_runtime = self.attached_plan_link_runtime();
        let activity_hub = self.attached_activity_hub();
        let plan_subagent_scope = match lookup_plan_subagent_scope(
            plan_link.as_ref(),
            plan_link_runtime.as_ref(),
            &token,
        )
        .await
        {
            Ok(scope) => scope,
            Err(PlanScopeLookupError::Cancelled) => {
                self.mark_cancelled_and_schedule(&agent_id).await;
                return;
            }
            Err(PlanScopeLookupError::Lookup(error)) => {
                self.mark_failed_and_schedule(
                    &agent_id,
                    "child Plan scope lookup failed",
                    error_info("subagent_plan_scope_error", error),
                )
                .await;
                return;
            }
        };
        if token.is_cancelled() {
            self.mark_cancelled_and_schedule(&agent_id).await;
            return;
        };
        let runtime = match self.factory.build_child(ChildRuntimeInput {
            session_id: child_session_id,
            task_anchor,
            task: task.clone(),
            allowed_tools: task.allowed_tools().to_vec(),
            workspace_scope: ChildWorkspaceScope::from_task(&task),
            depth: self.depth.saturating_add(1),
            generation_config: generation_config.clone(),
            plan_subagent_control: None,
            plan_subagent_scope,
            plan_link: plan_link.clone(),
            plan_link_runtime,
            activity_hub: activity_hub.clone(),
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
        if token.is_cancelled() {
            self.mark_cancelled_and_schedule(&agent_id).await;
            return;
        }

        spawn_child_loop(
            self.child_scheduler(),
            ChildLoopLaunch {
                agent_id,
                task_id,
                task,
                token,
                runtime,
                generation_config,
                activity_hub,
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
        let mut link = None;
        let mut terminal_activity = None;
        if let Some(agent) = state.agents.get_mut(agent_id) {
            if agent.status.is_terminal() {
                return;
            }
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = summary.to_owned();
            agent.diagnostics = Some(diagnostics);
            link = agent.plan_link.clone();
            terminal_activity = terminal_activity_for_agent(agent);
        }
        let to_start = self.reserve_queued_starts_locked(&mut state);
        drop(state);
        self.update_plan_links(link.into_iter().collect(), PlanLinkStatus::Failed)
            .await;
        if let Some((task_id, phase, summary)) = terminal_activity {
            publish_terminal_activity_snapshot(
                self.attached_activity_hub(),
                agent_id.clone(),
                task_id,
                phase,
                &summary,
            );
        }
        self.notify.notify_waiters();
        let _ = self.start_reserved_children(to_start).await;
    }

    async fn mark_cancelled_and_schedule(&self, agent_id: &SubagentId) {
        let mut state = self.state.lock().await;
        let mut link = None;
        let mut terminal_activity = None;
        if let Some(agent) = state.agents.get_mut(agent_id)
            && !agent.status.is_terminal()
        {
            agent.status = SubagentStatusLabel::Cancelled;
            agent.summary = "child cancelled before runtime start".to_owned();
            agent.diagnostics = Some(error_info(
                "subagent_cancelled",
                "child cancellation requested before runtime start",
            ));
            link = agent.plan_link.clone();
            terminal_activity = terminal_activity_for_agent(agent);
        }
        let to_start = self.reserve_queued_starts_locked(&mut state);
        drop(state);
        self.update_plan_links(link.into_iter().collect(), PlanLinkStatus::Cancelled)
            .await;
        if let Some((task_id, phase, summary)) = terminal_activity {
            publish_terminal_activity_snapshot(
                self.attached_activity_hub(),
                agent_id.clone(),
                task_id,
                phase,
                &summary,
            );
        }
        self.notify.notify_waiters();
        let _ = self.start_reserved_children(to_start).await;
    }

    fn reserve_queued_starts_locked(
        &self,
        state: &mut SubagentManagerState,
    ) -> Vec<ReservedChildStart> {
        reserve_queued_starts_locked(state, self.effective_max_threads())
    }

    fn child_scheduler(&self) -> ChildScheduler {
        ChildScheduler {
            factory: Arc::clone(&self.factory),
            state: Arc::clone(&self.state),
            notify: Arc::clone(&self.notify),
            enabled: Arc::clone(&self.enabled),
            max_threads: Arc::clone(&self.max_threads),
            depth: self.depth,
            plan_link_runtime: Arc::clone(&self.plan_link_runtime),
            activity_hub: Arc::clone(&self.activity_hub),
        }
    }

    async fn start_reserved_children(
        &self,
        starts: Vec<ReservedChildStart>,
    ) -> Result<(), RuntimeError> {
        start_reserved_children_iteratively(self.child_scheduler(), starts).await;
        Ok(())
    }

    async fn status_for(&self, agent_ids: &[SubagentId]) -> Vec<SubagentStatusView> {
        let state = self.state.lock().await;
        selected_statuses(&state, agent_ids)
    }

    async fn update_plan_links(&self, links: Vec<PlanLinkSnapshot>, status: PlanLinkStatus) {
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

fn wait_mode_satisfied(mode: WaitMode, agents: &[SubagentStatusView]) -> bool {
    match mode {
        WaitMode::Any => agents.iter().any(SubagentStatusView::is_terminal),
        WaitMode::All => agents.iter().all(SubagentStatusView::is_terminal),
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

enum ReservedChildStartOutcome {
    Started,
    Cancelled,
}

enum ReservedChildStartError {
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
    if let Some(agent) = state_guard.agents.get_mut(&start.agent_id)
        && !agent.status.is_terminal()
    {
        agent.status = status;
        agent.summary = summary.to_owned();
        agent.diagnostics = Some(diagnostics);
        link = agent.plan_link.clone();
        terminal_activity = terminal_activity_for_agent(agent);
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
    scheduler.notify.notify_waiters();
}

async fn spawn_reserved_child(
    scheduler: ChildScheduler,
    start: &ReservedChildStart,
) -> Result<ReservedChildStartOutcome, ReservedChildStartError> {
    let plan_link_runtime = scheduler
        .plan_link_runtime
        .lock()
        .expect("subagent plan link runtime mutex is not poisoned")
        .clone();
    let plan_subagent_scope = match lookup_plan_subagent_scope(
        start.plan_link.as_ref(),
        plan_link_runtime.as_ref(),
        &start.cancellation_token,
    )
    .await
    {
        Ok(scope) => scope,
        Err(PlanScopeLookupError::Cancelled) => {
            return Ok(ReservedChildStartOutcome::Cancelled);
        }
        Err(PlanScopeLookupError::Lookup(error)) => {
            return Err(ReservedChildStartError::PlanScope(error));
        }
    };
    if start.cancellation_token.is_cancelled() {
        return Ok(ReservedChildStartOutcome::Cancelled);
    }
    let child_session_id = child_session_id();
    let generation_config = generation_config_for_child_task(&start.task);
    let runtime = scheduler
        .factory
        .build_child(ChildRuntimeInput {
            session_id: child_session_id,
            task_anchor: start.task_anchor.clone(),
            task: start.task.clone(),
            allowed_tools: start.task.allowed_tools().to_vec(),
            workspace_scope: ChildWorkspaceScope::from_task(&start.task),
            depth: scheduler.depth.saturating_add(1),
            generation_config: generation_config.clone(),
            plan_subagent_control: None,
            plan_subagent_scope,
            plan_link: start.plan_link.clone(),
            plan_link_runtime,
            activity_hub: scheduler.attached_activity_hub(),
        })
        .map_err(ReservedChildStartError::Factory)?;
    if start.cancellation_token.is_cancelled() {
        return Ok(ReservedChildStartOutcome::Cancelled);
    }

    let activity_hub = scheduler.attached_activity_hub();
    spawn_child_loop(
        scheduler,
        ChildLoopLaunch {
            agent_id: start.agent_id.clone(),
            task_id: start.task_id.clone(),
            task: start.task.clone(),
            token: start.cancellation_token.clone(),
            runtime,
            generation_config,
            activity_hub,
        },
    );

    Ok(ReservedChildStartOutcome::Started)
}

fn spawn_child_loop(scheduler: ChildScheduler, launch: ChildLoopLaunch) {
    tokio::spawn(async move {
        let mut activity_reducer =
            SubagentActivityReducer::new(launch.agent_id.clone(), launch.task_id.clone());
        if let Some(hub) = launch.activity_hub.as_deref() {
            hub.publish(activity_reducer.starting(crate::plan::unix_time_ms()));
        }

        let input = match StepInput::user_text(launch.task.task()) {
            Ok(input) => input,
            Err(error) => {
                let cancelled = launch.token.is_cancelled();
                finish_child_with_status(
                    scheduler,
                    &launch,
                    &mut activity_reducer,
                    if cancelled {
                        SubagentStatusLabel::Cancelled
                    } else {
                        SubagentStatusLabel::Failed
                    },
                    if cancelled {
                        "child cancelled before runtime stream"
                    } else {
                        "child task input was rejected"
                    },
                    if cancelled {
                        error_info(
                            "subagent_cancelled",
                            "child cancellation requested before runtime stream",
                        )
                    } else {
                        error_info("subagent_input_error", error.to_string())
                    },
                )
                .await;
                return;
            }
        };
        let config = match AgentLoopConfig::new(launch.task.max_model_turns() as usize) {
            Ok(config) => config,
            Err(error) => {
                let cancelled = launch.token.is_cancelled();
                finish_child_with_status(
                    scheduler,
                    &launch,
                    &mut activity_reducer,
                    if cancelled {
                        SubagentStatusLabel::Cancelled
                    } else {
                        SubagentStatusLabel::Failed
                    },
                    if cancelled {
                        "child cancelled before runtime stream"
                    } else {
                        "child loop configuration was rejected"
                    },
                    if cancelled {
                        error_info(
                            "subagent_cancelled",
                            "child cancellation requested before runtime stream",
                        )
                    } else {
                        error_info("subagent_config_error", error.to_string())
                    },
                )
                .await;
                return;
            }
        };

        let loop_token = launch.token.clone();
        let mut stream = match launch.runtime.run_agent_loop_stream(
            input,
            StepContext::new(loop_token.clone())
                .with_generation_config(launch.generation_config.clone()),
            config,
        ) {
            Ok(stream) => stream,
            Err(error) => {
                let cancelled = loop_token.is_cancelled();
                finish_child_with_status(
                    scheduler,
                    &launch,
                    &mut activity_reducer,
                    if cancelled {
                        SubagentStatusLabel::Cancelled
                    } else {
                        SubagentStatusLabel::Failed
                    },
                    if cancelled {
                        "child cancelled before runtime stream"
                    } else {
                        "child runtime stream failed to start"
                    },
                    if cancelled {
                        error_info(
                            "subagent_cancelled",
                            "child cancellation requested before runtime stream",
                        )
                    } else {
                        error_info("subagent_stream_start_error", error.to_string())
                    },
                )
                .await;
                return;
            }
        };

        let mut bridge_request = None;
        while let Some(message) = stream.next_driver_message().await {
            match message {
                AgentLoopStreamMessage::Event(event) => {
                    if let Some(hub) = launch.activity_hub.as_deref()
                        && let Some(snapshot) =
                            activity_reducer.reduce(&event, crate::plan::unix_time_ms())
                    {
                        hub.publish(snapshot);
                    }
                }
                AgentLoopStreamMessage::BridgeToolRequest { call } => {
                    bridge_request = Some(call);
                    break;
                }
            }
        }
        let loop_result = if let Some(call) = bridge_request.as_ref() {
            tracing::warn!(
                subagent_id = %launch.agent_id,
                task_id = %launch.task_id,
                tool_name = %call.name(),
                "child runtime has no bridge host for requested tool"
            );
            drop(stream);
            None
        } else {
            stream.result().await
        };
        let child_projection = match loop_result.as_ref() {
            Some(result) => ChildLoopProjection::from_result(&launch.runtime, result).await,
            None => ChildLoopProjection::default(),
        };

        let mut state_guard = scheduler.state.lock().await;
        if state_guard
            .agents
            .get(&launch.agent_id)
            .is_some_and(|agent| agent.status == SubagentStatusLabel::Cancelled)
        {
            let to_start =
                reserve_queued_starts_locked(&mut state_guard, scheduler.effective_max_threads());
            drop(state_guard);
            scheduler.notify.notify_waiters();
            start_reserved_children_iteratively(scheduler, to_start).await;
            return;
        }
        if let Some(agent) = state_guard.agents.get_mut(&launch.agent_id) {
            match loop_result {
                Some(result) => apply_loop_result(agent, &result, child_projection),
                None if loop_token.is_cancelled() => {
                    agent.status = SubagentStatusLabel::Cancelled;
                    agent.summary = "child cancelled before stream result".to_owned();
                    agent.result = None;
                    agent.diagnostics = Some(error_info(
                        "subagent_cancelled",
                        "child stream ended after cancellation without a result",
                    ));
                }
                None if bridge_request.is_some() => {
                    let call = bridge_request
                        .as_ref()
                        .expect("bridge request guard ensures a call is present");
                    agent.status = SubagentStatusLabel::Failed;
                    agent.summary =
                        format!("child bridge tool {} has no host", call.name().as_str());
                    agent.result = None;
                    agent.diagnostics = Some(error_info(
                        "subagent_bridge_unavailable",
                        format!(
                            "child bridge tool {} requested without a bridge host",
                            call.name().as_str()
                        ),
                    ));
                }
                None => {
                    agent.status = SubagentStatusLabel::Failed;
                    agent.summary = "child runtime stream ended without result".to_owned();
                    agent.result = None;
                    agent.diagnostics = Some(error_info(
                        "subagent_stream_result_missing",
                        "child runtime stream ended without a durable result",
                    ));
                }
            }
        }
        let (terminal_link, terminal_status, terminal_activity) = state_guard
            .agents
            .get(&launch.agent_id)
            .map(|agent| {
                (
                    agent.plan_link.clone(),
                    plan_link_status_for_agent(agent),
                    terminal_activity_for_agent(agent),
                )
            })
            .unwrap_or((None, PlanLinkStatus::Failed, None));
        let to_start =
            reserve_queued_starts_locked(&mut state_guard, scheduler.effective_max_threads());
        drop(state_guard);
        update_plan_link_with_scheduler(&scheduler, terminal_link, terminal_status).await;
        if let Some((_task_id, phase, summary)) = terminal_activity {
            publish_terminal_activity(
                launch.activity_hub.as_deref(),
                &mut activity_reducer,
                phase,
                &summary,
            );
        }
        scheduler.notify.notify_waiters();
        start_reserved_children_iteratively(scheduler, to_start).await;
    });
}

async fn finish_child_with_status(
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
    let (link, link_status, terminal_activity) = scheduler
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
            )
        })
        .unwrap_or((None, PlanLinkStatus::Failed, None));
    update_plan_link_with_scheduler(&scheduler, link, link_status).await;
    if let Some((_task_id, phase, summary)) = terminal_activity {
        publish_terminal_activity(activity_hub.as_deref(), reducer, phase, &summary);
    }
    scheduler.notify.notify_waiters();
    start_reserved_children_iteratively(scheduler, to_start).await;
}

fn publish_terminal_activity(
    hub: Option<&SubagentActivityHub>,
    reducer: &mut SubagentActivityReducer,
    phase: SubagentActivityPhase,
    summary: &str,
) {
    if let Some(hub) = hub {
        hub.publish(reducer.terminal(phase, summary, crate::plan::unix_time_ms()));
    }
}

fn publish_terminal_activity_snapshot(
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

fn plan_link_status_for_agent(agent: &ManagedSubagent) -> PlanLinkStatus {
    match agent.status {
        SubagentStatusLabel::Completed => PlanLinkStatus::Completed,
        SubagentStatusLabel::Cancelled => PlanLinkStatus::Cancelled,
        SubagentStatusLabel::Failed => PlanLinkStatus::Failed,
        SubagentStatusLabel::Queued | SubagentStatusLabel::Running => PlanLinkStatus::Failed,
    }
}

fn terminal_activity_for_agent(
    agent: &ManagedSubagent,
) -> Option<(SubagentTaskId, SubagentActivityPhase, String)> {
    let phase = match agent.status {
        SubagentStatusLabel::Completed => SubagentActivityPhase::Completed,
        SubagentStatusLabel::Failed => SubagentActivityPhase::Failed,
        SubagentStatusLabel::Cancelled => SubagentActivityPhase::Cancelled,
        SubagentStatusLabel::Queued | SubagentStatusLabel::Running => return None,
    };
    Some((agent.task_id.clone(), phase, agent.summary.clone()))
}

async fn update_plan_link_with_scheduler(
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

fn generation_config_for_child_task(task: &SubagentTaskSpec) -> GenerationConfig {
    GenerationConfig::default().with_reasoning_effort(task.reasoning_effort().cloned())
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

fn child_session_id() -> merry_core::SessionId {
    merry_core::SessionId::random()
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
    events: &[RuntimeJournalEvent],
) -> Vec<String> {
    let mut pending_tool_names = BTreeMap::new();
    let mut paths = BTreeSet::new();

    for event in events {
        match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => {
                pending_tool_names.insert(call.id().clone(), call.name().clone());
            }
            RuntimeJournalPayload::ToolCallResolved { result }
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
        SubagentTaskSpec::new("Read the whole workspace.", 4)
            .expect("valid task")
            .with_read_scope(["."])
            .expect("workspace root is a valid scope");
        for invalid in [
            "",
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
        assert!(
            specs[0]
                .description()
                .contains("exact registered Merry tool names")
        );
        assert!(specs[0].description().contains("functions.run_process"));

        for spec in &specs {
            let value = serde_json::to_value(spec.input_schema()).expect("schema serializes");
            assert!(matches!(value, Value::Object(_)));
        }

        let spawn_schema =
            serde_json::to_string(specs[0].input_schema()).expect("spawn schema should serialize");
        assert!(spawn_schema.contains("Exact registered Merry tool names"));
        assert!(spawn_schema.contains("functions.run_process"));
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
                }],
                "timed_out": false,
                "terminal": true,
                "pending_agent_ids": []
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

    #[test]
    fn task_capability_fields_record_whether_the_parent_authored_them() {
        let omitted = SubagentTaskSpec::new("Inspect the runtime.", 2).expect("valid task");
        assert!(!omitted.allowed_tools_are_explicit());
        assert!(!omitted.read_scope_is_explicit());
        assert!(!omitted.write_scope_is_explicit());
        assert!(!omitted.forbidden_paths_are_explicit());

        let explicit = omitted
            .with_allowed_tools([ToolName::new("workspace_read_file").expect("valid tool")])
            .with_read_scope([PathBuf::from("crates")])
            .expect("valid read scope")
            .with_write_scope([PathBuf::from("tmp")])
            .expect("valid write scope")
            .with_forbidden_paths([PathBuf::from(".git")])
            .expect("valid forbidden scope");
        assert!(explicit.allowed_tools_are_explicit());
        assert!(explicit.read_scope_is_explicit());
        assert!(explicit.write_scope_is_explicit());
        assert!(explicit.forbidden_paths_are_explicit());
    }

    #[test]
    fn omitted_child_capabilities_inherit_and_explicit_values_cannot_expand() {
        struct CapabilityFactory;
        impl ChildRuntimeFactory for CapabilityFactory {
            fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
                Runtime::builder(input.session_id).build()
            }
        }

        let manager = SubagentManager::runtime_controlled(
            merry_core::SessionId::new("capability-inheritance").expect("valid session"),
            SubagentConfig::default(),
            Arc::new(CapabilityFactory),
            true,
        );
        manager.attach_parent_capabilities(
            vec![ToolName::new("workspace_read_file").expect("valid tool")],
            ChildWorkspaceScope {
                read_scope: vec![PathBuf::from("crates")],
                write_scope: vec![PathBuf::from("tmp")],
                forbidden_paths: vec![PathBuf::from(".git")],
            },
        );
        let omitted = SubagentTaskSpec::new("Inspect inherited scope.", 2).expect("valid task");
        let inherited = manager
            .apply_parent_capabilities(omitted)
            .expect("omitted capabilities should inherit");
        assert_eq!(inherited.allowed_tools().len(), 1);
        assert_eq!(inherited.read_scope(), &[PathBuf::from("crates")]);
        assert_eq!(inherited.write_scope(), &[PathBuf::from("tmp")]);
        assert_eq!(inherited.forbidden_paths(), &[PathBuf::from(".git")]);

        let expanded = SubagentTaskSpec::new("Expand scope.", 2)
            .expect("valid task")
            .with_read_scope([PathBuf::from(".")])
            .expect("valid scope");
        assert!(matches!(
            manager.apply_parent_capabilities(expanded),
            Err(SubagentError::CapabilityExpansion {
                field: "read_scope",
                ..
            })
        ));
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
        ModelStreamContext, ModelToolCall, ModelToolCallId, ReasoningEffort, ToolArguments,
        testing::FakeModelProvider,
    };
    use schemars::Schema;
    use serde_json::{Map, json};
    use std::{
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct FakeChildFactory {
        started: Arc<AtomicUsize>,
    }

    #[tokio::test]
    async fn runtime_controlled_manager_changes_spawn_tool_visibility() {
        let manager = SubagentManager::runtime_controlled(
            merry_core::SessionId::new("dynamic-subagents").unwrap(),
            SubagentConfig::default(),
            Arc::new(FakeChildFactory::new()),
            false,
        );
        let spawn = ToolName::new(SPAWN_SUBAGENTS_TOOL_NAME).unwrap();
        let workspace_read = ToolName::new("workspace_read_file").unwrap();

        assert!(!manager.is_tool_visible(&spawn));
        assert!(manager.is_tool_visible(&workspace_read));

        manager
            .update_policy(true, SubagentConfig::default())
            .await
            .expect("policy update should apply");

        assert!(manager.is_tool_visible(&spawn));
    }

    #[tokio::test]
    async fn runtime_controlled_manager_rejects_spawn_after_it_is_disabled() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::runtime_controlled(
            SessionId::new("dynamic-subagents-disabled").expect("valid session id"),
            SubagentConfig::default(),
            factory.clone(),
            true,
        );
        manager
            .update_policy(false, SubagentConfig::default())
            .await
            .expect("policy update should apply");

        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Must not start.", 1).expect("valid task")],
                None,
                CancellationToken::new(),
            )
            .await
            .expect("disabled spawn returns a structured rejection");

        assert!(output.spawned.is_empty());
        assert_eq!(output.rejected.len(), 1);
        assert_eq!(output.rejected[0].reason, "subagent spawning is disabled");
        assert_eq!(factory.started.load(Ordering::SeqCst), 0);
        assert!(manager.snapshot().await.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_controlled_manager_promotes_queued_children_after_thread_limit_increase() {
        let factory = Arc::new(AlwaysPendingChildFactory::new());
        let manager = SubagentManager::runtime_controlled(
            SessionId::new("dynamic-subagents-threads").expect("valid session id"),
            SubagentConfig::new(1, 1).expect("valid initial config"),
            factory.clone(),
            true,
        );
        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("First pending task.", 1).expect("valid task"),
                    SubagentTaskSpec::new("Second queued task.", 1).expect("valid task"),
                ],
                None,
                CancellationToken::new(),
            )
            .await
            .expect("spawn should succeed");

        assert_eq!(
            output.spawned[0].status,
            SpawnedSubagentStatusLabel::Running
        );
        assert_eq!(output.spawned[1].status, SpawnedSubagentStatusLabel::Queued);
        assert_eq!(factory.started.load(Ordering::SeqCst), 1);

        manager
            .update_policy(
                true,
                SubagentConfig::new(2, 1).expect("valid updated config"),
            )
            .await
            .expect("thread limit update should apply");

        assert_eq!(factory.started.load(Ordering::SeqCst), 2);
        let statuses = manager.snapshot().await;
        assert_eq!(
            statuses
                .iter()
                .filter(|agent| agent.status == SubagentStatusLabel::Running)
                .count(),
            2
        );
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

    #[derive(Clone)]
    struct AlwaysPendingChildFactory {
        started: Arc<AtomicUsize>,
    }

    impl AlwaysPendingChildFactory {
        fn new() -> Self {
            Self {
                started: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl ChildRuntimeFactory for AlwaysPendingChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .model_provider(
                    Arc::new(PendingModelProvider::new()),
                    ModelName::new("fake/pending").expect("valid model name"),
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
                    true, true, false, false, None, None,
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

    #[derive(Clone)]
    struct RecordingModelChildFactory {
        provider: FakeModelProvider,
    }

    impl RecordingModelChildFactory {
        fn new() -> Self {
            Self {
                provider: FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
                    response: ModelResponse::new(
                        vec![ModelOutput::text("child done")],
                        FinishReason::Stop,
                        None,
                    ),
                })]),
            }
        }

        fn recorded_requests(&self) -> Vec<ModelRequest> {
            self.provider.recorded_requests()
        }
    }

    struct GatedRecordingModelChildFactory {
        release: CancellationToken,
    }

    impl ChildRuntimeFactory for GatedRecordingModelChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            let provider = ScriptedStepProvider::with_release(
                vec![vec![Ok(ModelEvent::Completed {
                    response: ModelResponse::new(
                        vec![ModelOutput::text("gated child done")],
                        FinishReason::Stop,
                        None,
                    ),
                })]],
                self.release.clone(),
            );
            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .model_provider(
                    Arc::new(provider),
                    ModelName::new("fake/gated-recording-child").expect("valid model name"),
                )
                .build()
        }
    }

    impl ChildRuntimeFactory for RecordingModelChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .model_provider(
                    Arc::new(self.provider.clone()),
                    ModelName::new("fake/recording-child").expect("valid model name"),
                )
                .build()
        }
    }

    struct BridgeRequestChildFactory;

    impl ChildRuntimeFactory for BridgeRequestChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
            let provider = ScriptedStepProvider::new(vec![vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(ModelToolCall::new(
                        ModelToolCallId::new("call-child-bridge").expect("valid call id"),
                        ToolName::new("child_bridge").expect("valid tool name"),
                        ToolArguments::new(Map::new()),
                    ))],
                    FinishReason::ToolCalls,
                    None,
                ),
            })]]);

            Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .model_provider(
                    Arc::new(provider),
                    ModelName::new("fake/bridge-child").expect("valid model name"),
                )
                .registered_tool_allowlist(input.allowed_tools)
                .allow_bridge_tools()
                .register_tool(RegisteredTool::bridge(bridge_tool_spec()))
                .build()
        }
    }

    struct DefaultScopePlanLinkRuntime;

    impl PlanLinkRuntime for DefaultScopePlanLinkRuntime {
        fn bind_subagent<'a>(
            &'a self,
            _client_key: String,
            _agent_id: SubagentId,
            _task_id: SubagentTaskId,
            _now_ms: u64,
        ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
            Box::pin(async { Err("unused test binding".to_owned()) })
        }

        fn update_subagent_link<'a>(
            &'a self,
            _binding_id: PlanBindingId,
            _status: PlanLinkStatus,
            _now_ms: u64,
        ) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_link_runtime_default_scope_lookup_is_unbound() {
        let link = PlanLinkSnapshot {
            plan_id: merry_core::PlanId::new("default-plan").expect("valid plan id"),
            node_id: merry_core::PlanNodeId::new("default-node").expect("valid node id"),
            binding_id: PlanBindingId::new("default-binding").expect("valid binding id"),
            subagent_id: SubagentId::new("default-agent").expect("valid agent id"),
            task_id: SubagentTaskId::new("default-task").expect("valid task id"),
            status: PlanLinkStatus::Active,
            linked_at_ms: 1,
            terminal_at_ms: None,
            superseded_by: None,
        };
        assert!(
            DefaultScopePlanLinkRuntime
                .scope_for_link(&link)
                .await
                .expect("default scope lookup succeeds")
                .is_none()
        );
    }

    fn synthetic_plan_link(
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        binding_id: PlanBindingId,
    ) -> PlanLinkSnapshot {
        PlanLinkSnapshot {
            plan_id: merry_core::PlanId::new("synthetic-plan").expect("valid plan id"),
            node_id: merry_core::PlanNodeId::new("synthetic-node").expect("valid node id"),
            binding_id,
            subagent_id: agent_id,
            task_id,
            status: PlanLinkStatus::Active,
            linked_at_ms: 1,
            terminal_at_ms: None,
            superseded_by: None,
        }
    }

    struct FailingScopePlanLinkRuntime {
        updates: Arc<StdMutex<Vec<PlanLinkStatus>>>,
    }

    impl PlanLinkRuntime for FailingScopePlanLinkRuntime {
        fn bind_subagent<'a>(
            &'a self,
            _client_key: String,
            agent_id: SubagentId,
            task_id: SubagentTaskId,
            _now_ms: u64,
        ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
            Box::pin(async move {
                Ok(synthetic_plan_link(
                    agent_id,
                    task_id,
                    PlanBindingId::new("synthetic-binding").expect("valid binding id"),
                ))
            })
        }

        fn update_subagent_link<'a>(
            &'a self,
            _binding_id: PlanBindingId,
            status: PlanLinkStatus,
            _now_ms: u64,
        ) -> BoxFuture<'a, Result<(), String>> {
            let updates = Arc::clone(&self.updates);
            Box::pin(async move {
                updates
                    .lock()
                    .expect("link updates mutex is not poisoned")
                    .push(status);
                Ok(())
            })
        }

        fn scope_for_link<'a>(
            &'a self,
            _link: &'a PlanLinkSnapshot,
        ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
            Box::pin(async { Err("synthetic scope lookup failed".to_owned()) })
        }
    }

    #[derive(Clone)]
    struct OrderingPlanLinkRuntime {
        hub: Arc<SubagentActivityHub>,
        terminal_seen_during_update: Arc<StdMutex<Vec<bool>>>,
        phases_during_update: Arc<StdMutex<Vec<Option<merry_core::SubagentActivityPhase>>>>,
        update_started: Arc<Notify>,
        release: CancellationToken,
        update_completed: Arc<AtomicBool>,
        update_completed_notify: Arc<Notify>,
    }

    impl PlanLinkRuntime for OrderingPlanLinkRuntime {
        fn bind_subagent<'a>(
            &'a self,
            _client_key: String,
            agent_id: SubagentId,
            task_id: SubagentTaskId,
            _now_ms: u64,
        ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
            Box::pin(async move {
                let binding_id = PlanBindingId::new("ordering-binding").expect("valid binding id");
                Ok(synthetic_plan_link(agent_id, task_id, binding_id))
            })
        }

        fn update_subagent_link<'a>(
            &'a self,
            _binding_id: PlanBindingId,
            _status: PlanLinkStatus,
            _now_ms: u64,
        ) -> BoxFuture<'a, Result<(), String>> {
            let hub = Arc::clone(&self.hub);
            let observations = Arc::clone(&self.terminal_seen_during_update);
            let phases = Arc::clone(&self.phases_during_update);
            let update_started = Arc::clone(&self.update_started);
            let release = self.release.clone();
            let update_completed = Arc::clone(&self.update_completed);
            let update_completed_notify = Arc::clone(&self.update_completed_notify);
            Box::pin(async move {
                let activity = hub.current();
                let terminal_seen = activity.iter().any(|snapshot| {
                    matches!(
                        snapshot.phase,
                        merry_core::SubagentActivityPhase::Completed
                            | merry_core::SubagentActivityPhase::Failed
                            | merry_core::SubagentActivityPhase::Cancelled
                    )
                });
                phases
                    .lock()
                    .expect("ordering phases mutex is not poisoned")
                    .push(activity.first().map(|snapshot| snapshot.phase));
                observations
                    .lock()
                    .expect("ordering observations mutex is not poisoned")
                    .push(terminal_seen);
                update_started.notify_one();
                release.cancelled().await;
                update_completed.store(true, Ordering::SeqCst);
                update_completed_notify.notify_one();
                Ok(())
            })
        }

        fn scope_for_link<'a>(
            &'a self,
            _link: &'a PlanLinkSnapshot,
        ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[derive(Clone)]
    struct BlockingScopePlanLinkRuntime {
        scope_calls: Arc<AtomicUsize>,
        lookup_started: Arc<Notify>,
        lookup_dropped: Arc<Notify>,
        release: CancellationToken,
        updates: Arc<StdMutex<Vec<PlanLinkStatus>>>,
    }

    struct LookupDropGuard(Arc<Notify>);

    impl Drop for LookupDropGuard {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    impl PlanLinkRuntime for BlockingScopePlanLinkRuntime {
        fn bind_subagent<'a>(
            &'a self,
            _client_key: String,
            agent_id: SubagentId,
            task_id: SubagentTaskId,
            _now_ms: u64,
        ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
            Box::pin(async move {
                let binding_id =
                    PlanBindingId::new(&format!("synthetic-binding-{}", task_id.as_str()))
                        .expect("valid binding id");
                Ok(synthetic_plan_link(agent_id, task_id, binding_id))
            })
        }

        fn update_subagent_link<'a>(
            &'a self,
            _binding_id: PlanBindingId,
            status: PlanLinkStatus,
            _now_ms: u64,
        ) -> BoxFuture<'a, Result<(), String>> {
            let updates = Arc::clone(&self.updates);
            Box::pin(async move {
                updates
                    .lock()
                    .expect("link updates mutex is not poisoned")
                    .push(status);
                Ok(())
            })
        }

        fn scope_for_link<'a>(
            &'a self,
            _link: &'a PlanLinkSnapshot,
        ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
            let call = self.scope_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return Box::pin(async { Ok(None) });
            }
            let lookup_started = Arc::clone(&self.lookup_started);
            let lookup_dropped = Arc::clone(&self.lookup_dropped);
            let release = self.release.clone();
            Box::pin(async move {
                let _guard = LookupDropGuard(lookup_dropped);
                lookup_started.notify_one();
                release.cancelled().await;
                Ok(None)
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_activity_terminal_publishes_after_plan_link_update() {
        let hub = Arc::new(SubagentActivityHub::new());
        let model_release = CancellationToken::new();
        let terminal_seen_during_update = Arc::new(StdMutex::new(Vec::new()));
        let phases_during_update = Arc::new(StdMutex::new(Vec::new()));
        let update_started = Arc::new(Notify::new());
        let release = CancellationToken::new();
        let update_completed = Arc::new(AtomicBool::new(false));
        let update_completed_notify = Arc::new(Notify::new());
        let manager = SubagentManager::new(
            SessionId::new("subagent-activity-ordering").expect("valid session id"),
            SubagentConfig::new(1, 1).expect("valid subagent config"),
            Arc::new(GatedRecordingModelChildFactory {
                release: model_release.clone(),
            }),
        );
        manager.attach_activity_hub(Arc::clone(&hub));
        manager.attach_plan_link_runtime(Arc::new(OrderingPlanLinkRuntime {
            hub: Arc::clone(&hub),
            terminal_seen_during_update: Arc::clone(&terminal_seen_during_update),
            phases_during_update: Arc::clone(&phases_during_update),
            update_started: Arc::clone(&update_started),
            release: release.clone(),
            update_completed: Arc::clone(&update_completed),
            update_completed_notify: Arc::clone(&update_completed_notify),
        }));
        let mut activity_receiver = hub.subscribe();

        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Complete the ordered child task.", 1)
                        .expect("valid task")
                        .with_plan_task(Some("synthetic".to_owned())),
                ],
                None,
                CancellationToken::new(),
            )
            .await
            .expect("linked spawn succeeds");
        let agent_id = output.spawned[0].agent_id.clone();

        model_release.cancel();
        update_started.notified().await;
        let during_link_update = hub
            .current()
            .into_iter()
            .find(|snapshot| snapshot.subagent_id == agent_id)
            .expect("structured activity should precede link update");
        assert_eq!(
            during_link_update.phase,
            merry_core::SubagentActivityPhase::Running
        );
        assert!(!update_completed.load(Ordering::SeqCst));
        assert_eq!(
            phases_during_update
                .lock()
                .expect("ordering phases mutex is not poisoned")
                .as_slice(),
            &[Some(merry_core::SubagentActivityPhase::Running)]
        );

        release.cancel();
        update_completed_notify.notified().await;

        let wait = manager
            .wait(
                std::slice::from_ref(&agent_id),
                WaitMode::All,
                Some(Duration::from_secs(2)),
            )
            .await
            .expect("child wait succeeds");
        assert!(wait.terminal);
        assert_eq!(wait.agents[0].status, SubagentStatusLabel::Completed);
        assert_eq!(wait.agents[0].summary, "child completed");

        assert_eq!(
            terminal_seen_during_update
                .lock()
                .expect("ordering observations mutex is not poisoned")
                .as_slice(),
            &[false]
        );
        assert!(update_completed.load(Ordering::SeqCst));
        let activity = hub
            .current()
            .into_iter()
            .find(|snapshot| snapshot.subagent_id == agent_id)
            .expect("completed child activity is published");
        assert_eq!(activity.phase, merry_core::SubagentActivityPhase::Completed);
        assert_eq!(activity.task_id, output.spawned[0].task_id);
        assert_eq!(
            hub.published_phases(),
            vec![
                merry_core::SubagentActivityPhase::Starting,
                merry_core::SubagentActivityPhase::Running,
                merry_core::SubagentActivityPhase::Completed,
            ]
        );

        loop {
            activity_receiver
                .changed()
                .await
                .expect("terminal activity should be published");
            let snapshot = activity_receiver.borrow_and_update()[0].clone();
            if matches!(
                snapshot.phase,
                merry_core::SubagentActivityPhase::Completed
                    | merry_core::SubagentActivityPhase::Failed
                    | merry_core::SubagentActivityPhase::Cancelled
            ) {
                assert_eq!(snapshot.phase, merry_core::SubagentActivityPhase::Completed);
                break;
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_bridge_request_fails_without_claiming_completed() {
        let hub = Arc::new(SubagentActivityHub::new());
        let manager = SubagentManager::new(
            SessionId::new("subagent-bridge-driver").expect("valid session id"),
            SubagentConfig::new(1, 1).expect("valid subagent config"),
            Arc::new(BridgeRequestChildFactory),
        );
        manager.attach_activity_hub(Arc::clone(&hub));

        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Use the child bridge.", 2)
                        .expect("valid task")
                        .with_allowed_tools([
                            ToolName::new("child_bridge").expect("valid bridge tool name")
                        ]),
                ],
                None,
                CancellationToken::new(),
            )
            .await
            .expect("bridge child spawn succeeds");
        let agent_id = output.spawned[0].agent_id.clone();

        let wait = tokio::time::timeout(
            Duration::from_secs(1),
            manager.wait(std::slice::from_ref(&agent_id), WaitMode::All, None),
        )
        .await
        .expect("bridge request must not strand the child")
        .expect("bridge child wait succeeds");

        assert!(wait.terminal);
        assert_eq!(wait.agents[0].status, SubagentStatusLabel::Failed);
        assert!(wait.agents[0].result.is_none());
        assert!(
            wait.agents[0].summary.contains("bridge"),
            "unexpected bridge child summary: {}",
            wait.agents[0].summary
        );
        assert_eq!(
            hub.current()
                .into_iter()
                .find(|snapshot| snapshot.subagent_id == agent_id)
                .expect("bridge failure activity exists")
                .phase,
            merry_core::SubagentActivityPhase::Failed
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unbound_child_activity_reaches_completed_without_a_plan_link() {
        let hub = Arc::new(SubagentActivityHub::new());
        let manager = SubagentManager::new(
            SessionId::new("subagent-activity-unbound").expect("valid session id"),
            SubagentConfig::new(1, 1).expect("valid subagent config"),
            Arc::new(RecordingModelChildFactory::new()),
        );
        manager.attach_activity_hub(Arc::clone(&hub));

        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Complete an unbound child.", 1).expect("valid task")],
                None,
                CancellationToken::new(),
            )
            .await
            .expect("unbound spawn succeeds");
        let agent_id = output.spawned[0].agent_id.clone();
        let wait = manager
            .wait(std::slice::from_ref(&agent_id), WaitMode::All, None)
            .await
            .expect("child wait succeeds");
        assert!(wait.terminal);
        assert_eq!(wait.agents[0].status, SubagentStatusLabel::Completed);
        assert_eq!(
            hub.current()
                .into_iter()
                .find(|snapshot| snapshot.subagent_id == agent_id)
                .expect("unbound child activity exists")
                .phase,
            merry_core::SubagentActivityPhase::Completed
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_stream_result_follows_cancellation_without_claiming_completed() {
        let hub = Arc::new(SubagentActivityHub::new());
        let manager = SubagentManager::new(
            SessionId::new("subagent-activity-missing-result").expect("valid session id"),
            SubagentConfig::new(1, 1).expect("valid subagent config"),
            Arc::new(AlwaysPendingChildFactory::new()),
        );
        manager.attach_activity_hub(Arc::clone(&hub));
        let parent_token = CancellationToken::new();
        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Cancel before a stream result.", 1).expect("valid task"),
                ],
                None,
                parent_token.clone(),
            )
            .await
            .expect("pending child spawn succeeds");
        parent_token.cancel();

        let wait = manager
            .wait(
                std::slice::from_ref(&output.spawned[0].agent_id),
                WaitMode::All,
                None,
            )
            .await
            .expect("cancelled child wait succeeds");
        assert!(wait.terminal);
        assert_eq!(wait.agents[0].status, SubagentStatusLabel::Cancelled);
        assert_ne!(
            hub.current()
                .into_iter()
                .find(|snapshot| snapshot.subagent_id == output.spawned[0].agent_id)
                .expect("cancelled activity exists")
                .phase,
            merry_core::SubagentActivityPhase::Completed
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_scope_lookup_error_fails_child_and_plan_link() {
        let factory = Arc::new(FakeChildFactory::new());
        let activity_hub = Arc::new(SubagentActivityHub::new());
        let updates = Arc::new(StdMutex::new(Vec::new()));
        let manager = SubagentManager::new(
            SessionId::new("scope-lookup-error").expect("valid session id"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory.clone(),
        );
        manager.attach_activity_hub(Arc::clone(&activity_hub));
        manager.attach_plan_link_runtime(Arc::new(FailingScopePlanLinkRuntime {
            updates: Arc::clone(&updates),
        }));

        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Fail during scope lookup.", 1)
                        .expect("valid task")
                        .with_plan_task(Some("synthetic".to_owned())),
                ],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn returns structured output");
        let agent_id = output.spawned[0].agent_id.clone();
        let snapshot = manager.snapshot().await;
        let agent = snapshot
            .iter()
            .find(|agent| agent.agent_id == agent_id)
            .expect("failed child remains tracked");
        assert_eq!(agent.status, SubagentStatusLabel::Failed);
        assert_eq!(
            activity_hub
                .current()
                .into_iter()
                .find(|snapshot| snapshot.subagent_id == agent_id)
                .expect("failed child activity exists")
                .phase,
            merry_core::SubagentActivityPhase::Failed
        );
        assert_eq!(factory.started.load(Ordering::SeqCst), 0);
        assert!(
            updates
                .lock()
                .expect("link updates mutex is not poisoned")
                .contains(&PlanLinkStatus::Failed)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_child_cancellation_during_scope_lookup_skips_factory() {
        let factory = Arc::new(FailsFirstChildFactory::new());
        let runtime = Arc::new(BlockingScopePlanLinkRuntime {
            scope_calls: Arc::new(AtomicUsize::new(0)),
            lookup_started: Arc::new(Notify::new()),
            lookup_dropped: Arc::new(Notify::new()),
            release: CancellationToken::new(),
            updates: Arc::new(StdMutex::new(Vec::new())),
        });
        let manager = SubagentManager::new(
            SessionId::new("queued-scope-cancel").expect("valid session id"),
            SubagentConfig::new(1, 1).expect("valid config"),
            factory.clone(),
        );
        manager.attach_plan_link_runtime(runtime.clone());

        let spawn_manager = manager.clone();
        let spawn_handle = tokio::spawn(async move {
            spawn_manager
                .spawn(
                    vec![
                        SubagentTaskSpec::new("Fail first start.", 1)
                            .expect("valid task")
                            .with_plan_task(Some("synthetic".to_owned())),
                        SubagentTaskSpec::new("Cancel during lookup.", 1)
                            .expect("valid task")
                            .with_plan_task(Some("synthetic".to_owned())),
                    ],
                    Some(1),
                    CancellationToken::new(),
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(1), runtime.lookup_started.notified())
            .await
            .expect("queued child should enter scope lookup");
        let snapshot = manager.snapshot().await;
        let queued_agent = snapshot
            .iter()
            .find(|agent| agent.status == SubagentStatusLabel::Running)
            .expect("queued child remains reserved");
        let queued_id = queued_agent.agent_id.clone();

        manager
            .cancel(std::slice::from_ref(&queued_id))
            .await
            .expect("cancel should succeed");
        tokio::time::timeout(Duration::from_secs(1), runtime.lookup_dropped.notified())
            .await
            .expect("cancelled lookup should be dropped");
        let _output = spawn_handle
            .await
            .expect("spawn task should join")
            .expect("spawn succeeds");

        let snapshot = manager.snapshot().await;
        let cancelled = snapshot
            .iter()
            .find(|agent| agent.agent_id == queued_id)
            .expect("cancelled child remains tracked");
        assert_eq!(cancelled.status, SubagentStatusLabel::Cancelled);
        assert_eq!(factory.calls(), 1);
        assert!(
            runtime
                .updates
                .lock()
                .expect("link updates mutex is not poisoned")
                .contains(&PlanLinkStatus::Cancelled)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plan_link_scope_requires_current_exact_active_binding() {
        let session_id = SessionId::new("scope-exact-binding").expect("valid session id");
        let session = Arc::new(tokio::sync::Mutex::new(crate::session::SessionState::new(
            session_id,
        )));
        let (controller, _events) = PlanController::start(
            Arc::clone(&session),
            None,
            std::num::NonZeroUsize::new(16).expect("non-zero event buffer"),
        );
        controller
            .begin(crate::plan::BeginPlanInput {
                reason: "validate exact linked scope".to_owned(),
                governing_skill_id: None,
            })
            .await
            .expect("plan activation succeeds");
        controller
            .update(crate::plan::UpdatePlanInput {
                reason: "define linked scope task".to_owned(),
                execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: crate::plan::PlanChangeInput::DefinePlan {
                    expected_plan_revision: 0,
                    root: crate::plan::PlanNodeInput {
                        id: None,
                        client_key: Some("owned".to_owned()),
                        objective: "Complete the owned task".to_owned(),
                        acceptance: vec!["owned task completes".to_owned()],
                        status: None,
                        executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                        harness: merry_core::PlanHarnessSnapshot::default(),
                        recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                        depends_on: Vec::new(),
                        children: Vec::new(),
                    },
                },
            })
            .await
            .expect("plan definition succeeds");
        let link = controller
            .bind_subagent(
                "owned".to_owned(),
                merry_core::SubagentId::new("scope-agent").expect("valid agent id"),
                merry_core::SubagentTaskId::new("scope-task").expect("valid task id"),
                1,
            )
            .await
            .expect("link binds");
        let runtime = plan_link_runtime_for_controller(controller.clone());

        assert!(
            runtime
                .scope_for_link(&link)
                .await
                .expect("matching scope lookup succeeds")
                .is_some()
        );

        for status in [
            merry_core::PlanLinkStatus::Completed,
            merry_core::PlanLinkStatus::Failed,
            merry_core::PlanLinkStatus::Cancelled,
            merry_core::PlanLinkStatus::Superseded,
        ] {
            let mut terminal = link.clone();
            terminal.status = status;
            assert!(
                runtime
                    .scope_for_link(&terminal)
                    .await
                    .expect("terminal scope lookup succeeds")
                    .is_none(),
                "terminal input status {status:?} must not create a scope"
            );
        }

        let mut foreign_plan = link.clone();
        foreign_plan.plan_id = merry_core::PlanId::new("foreign-plan").expect("valid plan id");
        assert!(
            runtime
                .scope_for_link(&foreign_plan)
                .await
                .expect("foreign plan scope lookup succeeds")
                .is_none()
        );

        let mut foreign_node = link.clone();
        foreign_node.node_id = merry_core::PlanNodeId::new("foreign-node").expect("valid node id");
        assert!(
            runtime
                .scope_for_link(&foreign_node)
                .await
                .expect("foreign node scope lookup succeeds")
                .is_none()
        );

        let mut foreign_binding = link.clone();
        foreign_binding.binding_id =
            merry_core::PlanBindingId::new("foreign-binding").expect("valid binding id");
        assert!(
            runtime
                .scope_for_link(&foreign_binding)
                .await
                .expect("foreign binding scope lookup succeeds")
                .is_none()
        );

        let mut foreign_subagent = link.clone();
        foreign_subagent.subagent_id =
            merry_core::SubagentId::new("foreign-agent").expect("valid agent id");
        assert!(
            runtime
                .scope_for_link(&foreign_subagent)
                .await
                .expect("foreign subagent scope lookup succeeds")
                .is_none()
        );

        let mut foreign_task = link.clone();
        foreign_task.task_id =
            merry_core::SubagentTaskId::new("foreign-task").expect("valid task id");
        assert!(
            runtime
                .scope_for_link(&foreign_task)
                .await
                .expect("foreign task scope lookup succeeds")
                .is_none()
        );

        for (now_ms, status) in [
            (2, merry_core::PlanLinkStatus::Completed),
            (3, merry_core::PlanLinkStatus::Failed),
            (4, merry_core::PlanLinkStatus::Cancelled),
            (5, merry_core::PlanLinkStatus::Superseded),
        ] {
            controller
                .update_subagent_link(link.binding_id.clone(), status, now_ms)
                .await
                .expect("link status transition commits");
            assert!(
                runtime
                    .scope_for_link(&link)
                    .await
                    .expect("terminal controller lookup succeeds")
                    .is_none(),
                "a stale active link must not retain scope after {status:?} controller transition"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn linked_spawn_updates_plan_from_active_to_completed() {
        let session_id = SessionId::new("subagent-plan-link").expect("valid session id");
        let session = Arc::new(tokio::sync::Mutex::new(crate::session::SessionState::new(
            session_id.clone(),
        )));
        let (controller, _events) = PlanController::start(
            Arc::clone(&session),
            None,
            std::num::NonZeroUsize::new(16).expect("non-zero event buffer"),
        );
        controller
            .begin(crate::plan::BeginPlanInput {
                reason: "link child lifecycle".to_owned(),
                governing_skill_id: None,
            })
            .await
            .expect("plan activation succeeds");
        controller
            .update(crate::plan::UpdatePlanInput {
                reason: "define linked task".to_owned(),
                execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: crate::plan::PlanChangeInput::DefinePlan {
                    expected_plan_revision: 0,
                    root: crate::plan::PlanNodeInput {
                        id: None,
                        client_key: Some("root".to_owned()),
                        objective: "Complete the linked task".to_owned(),
                        acceptance: vec!["child completes".to_owned()],
                        status: None,
                        executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                        harness: merry_core::PlanHarnessSnapshot::default(),
                        recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                        depends_on: Vec::new(),
                        children: Vec::new(),
                    },
                },
            })
            .await
            .expect("plan definition succeeds");

        let manager = SubagentManager::new(
            session_id,
            SubagentConfig::new(1, 1).expect("valid subagent config"),
            Arc::new(RecordingModelChildFactory::new()),
        );
        manager.attach_plan_link_runtime(plan_link_runtime_for_controller(controller.clone()));
        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Complete the linked task.", 2)
                        .expect("valid task")
                        .with_plan_task(Some("root".to_owned())),
                ],
                None,
                CancellationToken::new(),
            )
            .await
            .expect("linked spawn succeeds");
        assert_eq!(output.spawned.len(), 1);

        manager
            .wait(
                &[output.spawned[0].agent_id.clone()],
                WaitMode::All,
                Some(Duration::from_secs(2)),
            )
            .await
            .expect("child wait succeeds");
        let snapshot = controller
            .snapshot()
            .await
            .expect("plan snapshot reads")
            .expect("active plan exists");
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.client_key.as_deref() == Some("root"))
            .expect("linked node exists");
        assert_eq!(node.execution_summary.active, 0);
        assert_eq!(node.execution_summary.completed, 1);
        assert_eq!(node.links.len(), 1);
        assert_eq!(node.links[0].status, merry_core::PlanLinkStatus::Completed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn linked_children_complete_each_plan_node_before_follow_up_update() {
        let session_id = SessionId::new("subagent-plan-links").expect("valid session id");
        let session = Arc::new(tokio::sync::Mutex::new(crate::session::SessionState::new(
            session_id.clone(),
        )));
        let (controller, _events) = PlanController::start(
            Arc::clone(&session),
            None,
            std::num::NonZeroUsize::new(16).expect("non-zero event buffer"),
        );
        controller
            .begin(crate::plan::BeginPlanInput {
                reason: "link parallel child lifecycles".to_owned(),
                governing_skill_id: None,
            })
            .await
            .expect("plan activation succeeds");
        controller
            .update(crate::plan::UpdatePlanInput {
                reason: "define parallel linked tasks".to_owned(),
                execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: Some(2),
                change: crate::plan::PlanChangeInput::DefinePlan {
                    expected_plan_revision: 0,
                    root: crate::plan::PlanNodeInput {
                        id: None,
                        client_key: Some("root".to_owned()),
                        objective: "Complete both linked tasks".to_owned(),
                        acceptance: vec!["both children complete".to_owned()],
                        status: None,
                        executor_policy: merry_core::PlanExecutorPolicy::Local,
                        harness: merry_core::PlanHarnessSnapshot::default(),
                        recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                        depends_on: Vec::new(),
                        children: vec![
                            crate::plan::PlanNodeInput {
                                id: None,
                                client_key: Some("left".to_owned()),
                                objective: "Complete the left task".to_owned(),
                                acceptance: vec!["left child completes".to_owned()],
                                status: None,
                                executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                                harness: merry_core::PlanHarnessSnapshot::default(),
                                recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                                depends_on: Vec::new(),
                                children: Vec::new(),
                            },
                            crate::plan::PlanNodeInput {
                                id: None,
                                client_key: Some("right".to_owned()),
                                objective: "Complete the right task".to_owned(),
                                acceptance: vec!["right child completes".to_owned()],
                                status: None,
                                executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                                harness: merry_core::PlanHarnessSnapshot::default(),
                                recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                                depends_on: Vec::new(),
                                children: Vec::new(),
                            },
                        ],
                    },
                },
            })
            .await
            .expect("plan definition succeeds");

        let manager = SubagentManager::new(
            session_id,
            SubagentConfig::new(2, 1).expect("valid subagent config"),
            Arc::new(RecordingModelChildFactory::new()),
        );
        manager.attach_plan_link_runtime(plan_link_runtime_for_controller(controller.clone()));
        let output = manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Complete the left task.", 2)
                        .expect("valid left task")
                        .with_plan_task(Some("left".to_owned())),
                    SubagentTaskSpec::new("Complete the right task.", 2)
                        .expect("valid right task")
                        .with_plan_task(Some("right".to_owned())),
                ],
                Some(2),
                CancellationToken::new(),
            )
            .await
            .expect("linked spawn succeeds");
        let agent_ids = output
            .spawned
            .iter()
            .map(|agent| agent.agent_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(agent_ids.len(), 2);

        manager
            .wait(&agent_ids, WaitMode::All, Some(Duration::from_secs(2)))
            .await
            .expect("child wait succeeds");

        let snapshot = controller
            .snapshot()
            .await
            .expect("plan snapshot reads")
            .expect("active plan exists");
        let linked_nodes = snapshot
            .nodes
            .iter()
            .filter(|node| matches!(node.client_key.as_deref(), Some("left" | "right")))
            .collect::<Vec<_>>();
        assert_eq!(linked_nodes.len(), 2);
        assert!(linked_nodes.iter().all(|node| {
            node.execution_summary.active == 0
                && node.execution_summary.completed == 1
                && node.links.len() == 1
                && node.links[0].status == merry_core::PlanLinkStatus::Completed
        }));
        assert_ne!(
            linked_nodes[0].links[0].binding_id,
            linked_nodes[1].links[0].binding_id
        );

        let revision = snapshot.revision;
        controller
            .update(crate::plan::UpdatePlanInput {
                reason: "continue after parallel linked children completed".to_owned(),
                execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: crate::plan::PlanChangeInput::UseCurrentPlan {
                    expected_plan_revision: revision,
                },
            })
            .await
            .expect("follow-up plan update succeeds after both children complete");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nested_subagent_keeps_parent_plan_link_runtime() {
        let session_id = SessionId::new("nested-subagent-plan-link").expect("valid session id");
        let session = Arc::new(tokio::sync::Mutex::new(crate::session::SessionState::new(
            session_id.clone(),
        )));
        let (controller, _events) = PlanController::start(
            Arc::clone(&session),
            None,
            std::num::NonZeroUsize::new(16).expect("non-zero event buffer"),
        );
        controller
            .begin(crate::plan::BeginPlanInput {
                reason: "link nested children".to_owned(),
                governing_skill_id: None,
            })
            .await
            .expect("plan activation succeeds");
        controller
            .update(crate::plan::UpdatePlanInput {
                reason: "define nested linked task".to_owned(),
                execution_intent: crate::plan::PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: crate::plan::PlanChangeInput::DefinePlan {
                    expected_plan_revision: 0,
                    root: crate::plan::PlanNodeInput {
                        id: None,
                        client_key: Some("root".to_owned()),
                        objective: "Complete nested linked work".to_owned(),
                        acceptance: vec!["nested child completes".to_owned()],
                        status: None,
                        executor_policy: merry_core::PlanExecutorPolicy::Delegate,
                        harness: merry_core::PlanHarnessSnapshot::default(),
                        recovery_policy: merry_core::PlanRecoveryPolicySnapshot::default(),
                        depends_on: Vec::new(),
                        children: Vec::new(),
                    },
                },
            })
            .await
            .expect("plan definition succeeds");

        #[derive(Clone)]
        struct CapturingFactory {
            input: Arc<StdMutex<Option<ChildRuntimeInput>>>,
        }

        impl ChildRuntimeFactory for CapturingFactory {
            fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
                *self
                    .input
                    .lock()
                    .expect("child input mutex is not poisoned") = Some(input.clone());
                let mut builder = Runtime::builder(input.session_id).task_anchor(input.task_anchor);
                if let Some(hub) = input.activity_hub {
                    builder = builder.subagent_activity_hub(hub);
                }
                builder.build()
            }
        }

        let captured = Arc::new(StdMutex::new(None));
        let activity_hub = Arc::new(SubagentActivityHub::new());
        let root_manager = SubagentManager::runtime_controlled(
            session_id.clone(),
            SubagentConfig::new(1, 2).expect("valid root config"),
            Arc::new(CapturingFactory {
                input: Arc::clone(&captured),
            }),
            true,
        );
        root_manager.attach_activity_hub(Arc::clone(&activity_hub));
        root_manager.attach_plan_link_runtime(plan_link_runtime_for_controller(controller.clone()));
        let root = root_manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Complete root-linked work.", 1)
                        .expect("valid root task")
                        .with_plan_task(Some("root".to_owned())),
                ],
                None,
                CancellationToken::new(),
            )
            .await
            .expect("root spawn succeeds");
        assert_eq!(root.spawned.len(), 1);

        let nested_link_runtime = captured
            .lock()
            .expect("child input mutex is not poisoned")
            .as_ref()
            .and_then(|input| input.plan_link_runtime.clone())
            .expect("child receives the parent Plan link runtime");
        let forwarded_activity_hub = captured
            .lock()
            .expect("child input mutex is not poisoned")
            .as_ref()
            .and_then(|input| input.activity_hub.clone())
            .expect("child receives the parent activity hub");
        assert!(Arc::ptr_eq(&forwarded_activity_hub, &activity_hub));
        assert!(
            captured
                .lock()
                .expect("child input mutex is not poisoned")
                .as_ref()
                .and_then(|input| input.plan_subagent_scope.as_ref())
                .is_some(),
            "linked child receives the opaque Plan subtree scope"
        );
        let nested_captured = Arc::new(StdMutex::new(None));
        let nested_manager = SubagentManager::runtime_controlled_at_depth(
            SessionId::new("nested-parent").expect("valid nested session id"),
            SubagentConfig::new(1, 2).expect("valid nested config"),
            Arc::new(CapturingFactory {
                input: Arc::clone(&nested_captured),
            }),
            true,
            1,
        );
        nested_manager.attach_activity_hub(Arc::clone(&forwarded_activity_hub));
        nested_manager.attach_plan_link_runtime(nested_link_runtime);
        let nested = nested_manager
            .spawn(
                vec![
                    SubagentTaskSpec::new("Complete nested linked work.", 1)
                        .expect("valid nested task")
                        .with_plan_task(Some("root".to_owned())),
                ],
                None,
                CancellationToken::new(),
            )
            .await
            .expect("nested spawn succeeds");
        assert_eq!(nested.spawned.len(), 1);
        let nested_activity_hub = nested_captured
            .lock()
            .expect("nested child input mutex is not poisoned")
            .as_ref()
            .and_then(|input| input.activity_hub.clone())
            .expect("nested child receives the shared activity hub");
        assert!(Arc::ptr_eq(&nested_activity_hub, &activity_hub));

        let snapshot = controller
            .snapshot()
            .await
            .expect("plan snapshot reads")
            .expect("active plan exists");
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.client_key.as_deref() == Some("root"))
            .expect("linked node exists");
        assert_eq!(node.links.len(), 2);
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
        release: Option<CancellationToken>,
    }

    impl ScriptedStepProvider {
        fn new(responses: ScriptedStepResponses) -> Self {
            Self {
                name: merry_core::ProviderName::new("scripted-step-provider")
                    .expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities"),
                responses: Arc::new(StdMutex::new(responses.into_iter().rev().collect())),
                release: None,
            }
        }

        fn with_release(responses: ScriptedStepResponses, release: CancellationToken) -> Self {
            Self {
                release: Some(release),
                ..Self::new(responses)
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
            let release = self.release.clone();
            Box::pin(async move {
                if let Some(release) = release {
                    release.cancelled().await;
                }
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

    fn bridge_tool_spec() -> ToolSpec {
        let schema = Schema::try_from(json!({ "type": "object" }))
            .expect("test schema should be a JSON schema");
        ToolSpec::new(
            ToolName::new("child_bridge").expect("valid tool name"),
            "Request a host bridge operation.",
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
    async fn max_length_parent_session_still_starts_random_child_session() {
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

        assert_eq!(output.spawned.len(), 2);
        assert_eq!(
            output.spawned[0].status,
            SpawnedSubagentStatusLabel::Running
        );
        assert_eq!(output.spawned[1].status, SpawnedSubagentStatusLabel::Queued);
        assert_eq!(factory.started.load(Ordering::SeqCst), 1);
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
    async fn child_model_request_uses_task_reasoning_effort() {
        let factory = Arc::new(RecordingModelChildFactory::new());
        let activity_hub = Arc::new(SubagentActivityHub::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            factory.clone(),
        );
        manager.attach_activity_hub(activity_hub);
        let task = SubagentTaskSpec::new("Run a cheap child task.", 1)
            .expect("valid task")
            .with_reasoning_effort(Some(
                ReasoningEffort::new("low").expect("valid reasoning effort"),
            ));
        let output = manager
            .spawn(vec![task], Some(1), CancellationToken::new())
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
            .expect("child should complete");
        let requests = factory.recorded_requests();

        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .generation()
                .reasoning_effort()
                .map(|effort| effort.as_str()),
            Some("low")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_model_request_omits_reasoning_effort_when_task_does_not_set_it() {
        let factory = Arc::new(RecordingModelChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            factory.clone(),
        );
        let output = manager
            .spawn(
                vec![SubagentTaskSpec::new("Run a default child task.", 1).expect("valid task")],
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
            .expect("child should complete");
        let requests = factory.recorded_requests();

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].generation().reasoning_effort(), None);
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
    async fn startup_error_after_terminal_claim_does_not_republish_terminal_activity() {
        let hub = Arc::new(SubagentActivityHub::new());
        let manager = SubagentManager::new(
            SessionId::new("startup-error-terminal-race").expect("valid session id"),
            SubagentConfig::default(),
            Arc::new(FakeChildFactory::new()),
        );
        manager.attach_activity_hub(Arc::clone(&hub));

        let task = SubagentTaskSpec::new("Fail after parent cancellation.", 1).expect("valid task");
        let task_anchor = TaskAnchor::new(task.task()).expect("valid task anchor");
        let agent_id = SubagentId::new("agent-terminal-race").expect("valid agent id");
        let task_id = SubagentTaskId::new("task-terminal-race").expect("valid task id");
        let cancellation_token = CancellationToken::new();
        {
            let mut state = manager.state.lock().await;
            state
                .batches
                .insert(1, SubagentBatch { max_concurrency: 1 });
            state.agents.insert(
                agent_id.clone(),
                ManagedSubagent {
                    batch_id: 1,
                    agent_id: agent_id.clone(),
                    task_id: task_id.clone(),
                    task: task.clone(),
                    task_anchor: task_anchor.clone(),
                    status: SubagentStatusLabel::Running,
                    summary: "child running".to_owned(),
                    result: None,
                    output_paths: Vec::new(),
                    changed_paths: Vec::new(),
                    diagnostics: None,
                    cancellation_token: cancellation_token.clone(),
                    plan_link: None,
                },
            );
        }

        manager
            .cancel(std::slice::from_ref(&agent_id))
            .await
            .expect("parent cancellation should succeed");
        assert_eq!(
            hub.published_phases(),
            vec![merry_core::SubagentActivityPhase::Cancelled]
        );

        let runtime = Runtime::builder(
            SessionId::new("startup-error-terminal-child").expect("valid child session id"),
        )
        .task_anchor(task_anchor.clone())
        .build()
        .expect("child runtime should build");
        let generation_config = generation_config_for_child_task(&task);
        let launch = ChildLoopLaunch {
            agent_id: agent_id.clone(),
            task_id: task_id.clone(),
            task,
            token: cancellation_token,
            runtime,
            generation_config,
            activity_hub: Some(Arc::clone(&hub)),
        };
        let mut reducer = SubagentActivityReducer::new(agent_id.clone(), task_id);

        finish_child_with_status(
            manager.child_scheduler(),
            &launch,
            &mut reducer,
            SubagentStatusLabel::Failed,
            "child startup failed",
            error_info("subagent_start_error", "synthetic startup failure"),
        )
        .await;

        assert_eq!(
            hub.published_phases(),
            vec![merry_core::SubagentActivityPhase::Cancelled]
        );
        assert_eq!(
            manager
                .snapshot()
                .await
                .into_iter()
                .find(|agent| agent.agent_id == agent_id)
                .expect("terminal child remains tracked")
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
