use merry_core::{ErrorInfo, SubagentId, SubagentTaskId, ToolName};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    /// Optional maximum child model turns.
    pub max_model_turns: Option<u32>,
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
    /// Optional child model reasoning-effort override.
    pub reasoning_effort: Option<String>,
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

    pub(super) fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
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
    /// Explicit child result reported by the child loop, when it reached one.
    pub result: Option<SubagentResultView>,
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
            result: None,
            output_paths,
            changed_paths,
            diagnostics: None,
        }
    }

    pub(super) fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

/// Explicit result returned by a child agent to its parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubagentResultView {
    /// Child-authored conclusion. This is not truncated by runtime.
    pub conclusion: String,
}

impl SubagentResultView {
    pub(super) fn from_conclusion(conclusion: impl Into<String>) -> Option<Self> {
        let conclusion = conclusion.into();
        if conclusion.trim().is_empty() {
            None
        } else {
            Some(Self { conclusion })
        }
    }
}
