//! Runtime-owned parallel subagent tool contracts.

use merry_core::{ErrorInfo, SubagentId, SubagentTaskId, ToolInputSchema, ToolName, ToolSpec};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

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
