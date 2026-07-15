use merry_core::{ErrorInfo, SubagentId, SubagentTaskId, ToolName};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::spec::MAX_TASK_BYTES;

/// Provider-visible input for `spawn_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentsInput {
    #[schemars(
        description = "Child tasks to spawn. Each task is validated against the current child runtime limits."
    )]
    pub tasks: Vec<SpawnSubagentTaskInput>,
    #[schemars(
        description = "Optional concurrency cap for this batch. Omit it to use the runtime scheduler default; zero queues all tasks until capacity is available.",
        range(min = 0)
    )]
    pub max_concurrency: Option<usize>,
}

/// Provider-visible task item for `spawn_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentTaskInput {
    #[schemars(
        description = "Non-blank delegated task prompt. Keep it within the runtime byte limit.",
        length(min = 1, max = MAX_TASK_BYTES)
    )]
    pub task: String,
    #[schemars(description = "Optional short display name for this child task.")]
    pub display_name: Option<String>,
    #[schemars(
        schema_with = "max_model_turns_schema",
        description = "Optional positive maximum number of model turns for this child. Omit it to use the configured runtime default (1024 unless changed in runtime.subagents.max_model_turns)."
    )]
    pub max_model_turns: Option<u32>,
    /// Exact registered Merry tool names the child may use. Names are copied
    /// verbatim from the current tool list without provider namespace prefixes;
    /// use `run_process`, never `functions.run_process`.
    #[schemars(
        description = "Exact registered Merry tool names copied verbatim from the current tool list. Do not add provider namespace prefixes: use `run_process`, never `functions.run_process`."
    )]
    pub allowed_tools: Option<Vec<ToolName>>,
    #[schemars(
        schema_with = "optional_scope_paths_schema",
        description = "Optional normalized workspace-relative paths the child may read. Use `.` for the workspace root; do not use parent traversal, absolute paths, dot segments, empty segments, or backslashes."
    )]
    #[serde(default)]
    pub read_scope: Option<Vec<String>>,
    #[schemars(
        schema_with = "optional_scope_paths_schema",
        description = "Optional normalized workspace-relative paths the child may write. Use `.` for the workspace root; do not use parent traversal, absolute paths, dot segments, empty segments, or backslashes."
    )]
    #[serde(default)]
    pub write_scope: Option<Vec<String>>,
    #[schemars(
        schema_with = "optional_scope_paths_schema",
        description = "Optional normalized workspace-relative paths the child must not access. Use `.` for the workspace root; do not use parent traversal, absolute paths, dot segments, empty segments, or backslashes."
    )]
    #[serde(default)]
    pub forbidden_paths: Option<Vec<String>>,
    #[schemars(description = "Optional instruction describing the expected child result.")]
    pub expected_output: Option<String>,
    #[schemars(
        description = "Optional child model reasoning-effort override supported by the configured provider."
    )]
    pub reasoning_effort: Option<String>,
    #[schemars(
        description = "Optional Plan node client key that binds this child execution to an authored plan node."
    )]
    pub plan_task: Option<String>,
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
    #[schemars(
        description = "Child agent ids to inspect or wait on. Provide at least one id; the result contains only these selected agents.",
        length(min = 1)
    )]
    pub agent_ids: Vec<SubagentId>,
    #[schemars(
        description = "Optional completion mode. Use any for the first terminal child or all for every selected child; omit it for all."
    )]
    pub mode: Option<WaitMode>,
    /// Optional observation deadline in milliseconds. This is not a task
    /// budget and a timeout never means that the child completed.
    #[schemars(
        description = "Observation deadline in milliseconds, not a task budget. Zero returns an immediate status snapshot; omit it to wait until the selected completion condition. A timed-out result is only a status snapshot and must not be reported as completion.",
        range(min = 0)
    )]
    pub timeout_ms: Option<u64>,
}

/// Provider-visible input for `cancel_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelSubagentsInput {
    #[schemars(
        description = "Child agent ids to cancel. Provide at least one id; the result contains only these selected agents.",
        length(min = 1)
    )]
    pub agent_ids: Vec<SubagentId>,
}

fn optional_scope_paths_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::Schema::try_from(json!({
        "description": "Optional normalized workspace-relative scope paths. Use `.` for the workspace root or a concrete relative path such as `crates/merry-runtime`.",
        "anyOf": [
            { "type": "null" },
            {
                "type": "array",
                "items": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Normalized workspace-relative path. Do not use `..`, absolute paths, embedded `.` segments, empty segments, or backslashes.",
                    "examples": [".", "crates/merry-runtime", "tmp/output"],
                    "anyOf": [
                        { "const": "." },
                        { "allOf": [
                            { "not": { "pattern": "(^|/)\\.\\.?(/|$)" } },
                            { "not": { "pattern": "(^/|//|\\\\)" } },
                            { "not": { "pattern": "[\\u0000-\\u001F\\u007F]" } }
                        ] }
                    ]
                }
            }
        ]
    }))
    .expect("static optional subagent scope schema is valid")
}

fn max_model_turns_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::Schema::try_from(json!({
        "type": "integer",
        "minimum": 1,
        "default": super::spec::DEFAULT_MAX_MODEL_TURNS,
        "description": "Positive child model-turn budget. Omit it to use the configured runtime default."
    }))
    .expect("static child model-turn schema is valid")
}

/// Provider-visible compact status output for `wait_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitSubagentsOutput {
    /// Compact status views for selected child agents.
    pub agents: Vec<SubagentStatusView>,
    /// True when the observation deadline elapsed before the requested mode
    /// became terminal.
    pub timed_out: bool,
    /// True when the requested wait mode is satisfied by the returned status
    /// snapshot. Only terminal=true permits a completion claim.
    pub terminal: bool,
    /// Selected child ids that are still non-terminal in this snapshot.
    pub pending_agent_ids: Vec<SubagentId>,
}

impl WaitSubagentsOutput {
    /// Creates wait output from compact child status views.
    #[must_use]
    pub fn new(agents: Vec<SubagentStatusView>) -> Self {
        let terminal = agents.iter().all(SubagentStatusView::is_terminal);
        Self::with_wait_state(agents, terminal, false)
    }

    pub(crate) fn with_wait_state(
        agents: Vec<SubagentStatusView>,
        terminal: bool,
        timed_out: bool,
    ) -> Self {
        let pending_agent_ids = agents
            .iter()
            .filter(|agent| !agent.is_terminal())
            .map(|agent| agent.agent_id.clone())
            .collect();
        Self {
            agents,
            timed_out,
            terminal,
            pending_agent_ids,
        }
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
