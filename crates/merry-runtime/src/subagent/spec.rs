use merry_core::ToolName;
use merry_llm::ReasoningEffort;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Maximum UTF-8 task text size accepted for one child task.
pub(crate) const MAX_TASK_BYTES: usize = 16 * 1024;
/// Default bounded child loop model-turn count.
///
/// A small single-digit budget is too restrictive for a child that needs to
/// inspect files, use tools, verify the result, and report a conclusion. The
/// limit remains bounded, but is intentionally generous and configurable at
/// the runtime level.
pub const DEFAULT_MAX_MODEL_TURNS: u32 = 1024;
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
    /// The delegated task has no allowed model turns.
    #[error("max_model_turns must be greater than zero")]
    ZeroMaxModelTurns,
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
    /// A child task requested a capability outside the parent's effective envelope.
    #[error("child task capability expansion is not allowed for {field}: {value}")]
    CapabilityExpansion {
        /// Capability category that attempted to expand.
        field: &'static str,
        /// Requested capability value.
        value: String,
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
    max_model_turns: u32,
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
            max_model_turns: DEFAULT_MAX_MODEL_TURNS,
        })
    }

    /// Replaces the default model-turn budget inherited by child tasks that
    /// omit an explicit `max_model_turns` value.
    pub fn with_max_model_turns(mut self, max_model_turns: u32) -> Result<Self, SubagentError> {
        if max_model_turns == 0 {
            return Err(SubagentError::ZeroMaxModelTurns);
        }
        self.max_model_turns = max_model_turns;
        Ok(self)
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

    /// Returns the default maximum number of model turns for one child task.
    #[must_use]
    pub fn max_model_turns(self) -> u32 {
        self.max_model_turns
    }
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            max_threads: DEFAULT_MAX_THREADS,
            max_depth: DEFAULT_MAX_DEPTH,
            max_model_turns: DEFAULT_MAX_MODEL_TURNS,
        }
    }
}

/// Parent-authored specification for a bounded child agent task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentTaskSpec {
    display_name: Option<String>,
    task: String,
    max_model_turns: u32,
    allowed_tools: Vec<ToolName>,
    allowed_tools_explicit: bool,
    read_scope: Vec<PathBuf>,
    read_scope_explicit: bool,
    write_scope: Vec<PathBuf>,
    write_scope_explicit: bool,
    forbidden_paths: Vec<PathBuf>,
    forbidden_paths_explicit: bool,
    expected_output: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    plan_client_key: Option<String>,
}

impl SubagentTaskSpec {
    /// Creates a child task specification from validated task text and model-turn bound.
    pub fn new(task: impl Into<String>, max_model_turns: u32) -> Result<Self, SubagentError> {
        let task = task.into();
        validate_task_text(&task)?;
        if max_model_turns == 0 {
            return Err(SubagentError::ZeroMaxModelTurns);
        }
        Ok(Self {
            display_name: None,
            task,
            max_model_turns,
            allowed_tools: Vec::new(),
            allowed_tools_explicit: false,
            read_scope: Vec::new(),
            read_scope_explicit: false,
            write_scope: Vec::new(),
            write_scope_explicit: false,
            forbidden_paths: Vec::new(),
            forbidden_paths_explicit: false,
            expected_output: None,
            reasoning_effort: None,
            plan_client_key: None,
        })
    }

    /// Returns the child task prompt.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    /// Returns the maximum number of bounded child model turns.
    #[must_use]
    pub fn max_model_turns(&self) -> u32 {
        self.max_model_turns
    }

    /// Returns the provider/tool names the child may use.
    #[must_use]
    pub fn allowed_tools(&self) -> &[ToolName] {
        &self.allowed_tools
    }

    pub(crate) fn allowed_tools_are_explicit(&self) -> bool {
        self.allowed_tools_explicit
    }

    pub(crate) fn read_scope_is_explicit(&self) -> bool {
        self.read_scope_explicit
    }

    /// Returns whether the parent explicitly supplied the write scope,
    /// including an explicit empty scope for a read-only child.
    #[must_use]
    pub fn write_scope_is_explicit(&self) -> bool {
        self.write_scope_explicit
    }

    pub(crate) fn forbidden_paths_are_explicit(&self) -> bool {
        self.forbidden_paths_explicit
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

    /// Returns the optional reasoning-effort override for this child task.
    #[must_use]
    pub fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }

    /// Returns the optional authored Plan node client key.
    #[must_use]
    pub fn plan_client_key(&self) -> Option<&str> {
        self.plan_client_key.as_deref()
    }

    /// Replaces the child's allowed tool list.
    #[must_use]
    pub fn with_allowed_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = ToolName>,
    {
        self.allowed_tools = tools.into_iter().collect();
        self.allowed_tools_explicit = true;
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
        self.read_scope_explicit = true;
        Ok(self)
    }

    /// Replaces the write scope after validating each path.
    pub fn with_write_scope<I, P>(mut self, paths: I) -> Result<Self, SubagentError>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.write_scope = validate_scope_paths(paths)?;
        self.write_scope_explicit = true;
        Ok(self)
    }

    /// Replaces the forbidden path scope after validating each path.
    pub fn with_forbidden_paths<I, P>(mut self, paths: I) -> Result<Self, SubagentError>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.forbidden_paths = validate_scope_paths(paths)?;
        self.forbidden_paths_explicit = true;
        Ok(self)
    }

    /// Replaces the optional expected-output instruction, treating blank text as absent.
    #[must_use]
    pub fn with_expected_output(mut self, expected_output: Option<String>) -> Self {
        self.expected_output = expected_output.filter(|value| !value.trim().is_empty());
        self
    }

    /// Replaces the optional reasoning-effort override for this child task.
    #[must_use]
    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    /// Associates the child with an authored Plan node client key.
    #[must_use]
    pub fn with_plan_client_key(mut self, plan_client_key: Option<String>) -> Self {
        self.plan_client_key = plan_client_key.filter(|value| !value.trim().is_empty());
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

pub(super) fn validate_scope_path(path: PathBuf) -> Result<PathBuf, SubagentError> {
    let path_label = path.display().to_string();
    if !crate::workspace_scope::is_valid_workspace_scope(&path) {
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
    crate::workspace_scope::workspace_scopes_overlap(first, second)
}
