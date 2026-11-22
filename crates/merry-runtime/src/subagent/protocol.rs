mod tolerant;

use merry_core::{ErrorInfo, ModelUsage, SubagentId, SubagentTaskId, ToolName};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use super::spec::{DEFAULT_MIN_MODEL_TURNS, MAX_TASK_BYTES};
use tolerant::{
    deserialize_optional_agent_ids_field, deserialize_optional_enum_tolerant,
    deserialize_optional_paths_tolerant, deserialize_optional_string_tolerant, merge_agent_ids,
};

/// Filesystem isolation mode for a child subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubagentWorkspaceMode {
    /// Run inside the parent's shared workspace directory (default).
    #[default]
    Shared,
    /// Run inside an isolated detached Git worktree outside the main repository.
    Worktree,
}

impl SubagentWorkspaceMode {
    /// Returns the stable provider-visible mode string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Worktree => "worktree",
        }
    }
}

/// Snapshot source used when creating a child Git worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubagentWorktreeBase {
    /// Snapshot the parent's current working tree (tracked diff + small untracked files) on top of `HEAD` (default).
    #[default]
    WorkingTree,
    /// Checkout clean `HEAD` without uncommitted working-tree edits.
    Head,
}

impl SubagentWorktreeBase {
    /// Returns the stable provider-visible worktree base string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkingTree => "working_tree",
            Self::Head => "head",
        }
    }
}

/// Metadata reported for a child subagent executed inside an isolated Git worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubagentWorktreeView {
    /// Absolute path to the isolated Git worktree directory.
    pub path: String,
    /// Baseline commit OID against which net child changes are diffed.
    pub baseline_oid: String,
    /// Compact `git diff --stat <baseline_oid>` summary when net changes exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    /// True when the worktree remains on disk (changes exist or cleanup skipped); false when auto-removed after zero net changes.
    pub retained: bool,
}

/// Optional wait behavior for `spawn_subagents`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpawnWaitMode {
    /// Return immediately after spawning (default asynchronous behavior).
    #[default]
    None,
    /// Wait up to the foreground budget until all spawned children reach terminal state.
    All,
    /// Wait up to the foreground budget until any spawned child reaches terminal state.
    Any,
}

/// Provider-visible input for `spawn_subagents`.
///
/// Accepts either `{ "tasks": [ ... ] }` or a single-task shorthand object
/// `{ "task": "...", ... }` during deserialization while exposing the canonical
/// `tasks` schema to providers.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "spawn_subagents",
    description = "Spawn bounded child agents for parallel delegated tasks."
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
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
    #[schemars(
        description = "Optional wait mode after spawning: `none` (default, return immediately), `all` (wait for every spawned child), or `any` (wait for the first terminal child). If the foreground budget expires or the turn is interrupted, running children stay alive in the background."
    )]
    #[serde(default)]
    pub wait: Option<SpawnWaitMode>,
    #[schemars(
        description = "Optional foreground wait deadline in seconds when `wait` is `all` or `any`. Exceeding it hands running children off to the background without cancelling them.",
        range(min = 1)
    )]
    #[serde(default)]
    pub foreground_budget_secs: Option<u64>,
}

impl<'de> Deserialize<'de> for SpawnSubagentsInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct BatchWire {
            tasks: Vec<SpawnSubagentTaskInput>,
            #[serde(default)]
            max_concurrency: Option<usize>,
            #[serde(default)]
            wait: Option<SpawnWaitMode>,
            #[serde(default)]
            foreground_budget_secs: Option<u64>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SingleWire {
            #[serde(flatten)]
            task: SpawnSubagentTaskInput,
            #[serde(default)]
            max_concurrency: Option<usize>,
            #[serde(default)]
            wait: Option<SpawnWaitMode>,
            #[serde(default)]
            foreground_budget_secs: Option<u64>,
        }

        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("tasks").is_some() {
            let batch: BatchWire =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self {
                tasks: batch.tasks,
                max_concurrency: batch.max_concurrency,
                wait: batch.wait,
                foreground_budget_secs: batch.foreground_budget_secs,
            })
        } else if value.get("task").is_some() {
            let single: SingleWire =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self {
                tasks: vec![single.task],
                max_concurrency: single.max_concurrency,
                wait: single.wait,
                foreground_budget_secs: single.foreground_budget_secs,
            })
        } else {
            let batch: BatchWire =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self {
                tasks: batch.tasks,
                max_concurrency: batch.max_concurrency,
                wait: batch.wait,
                foreground_budget_secs: batch.foreground_budget_secs,
            })
        }
    }
}

/// Provider-visible task item for `spawn_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentTaskInput {
    #[schemars(
        description = "Non-blank delegated task brief. Include the goal, already ruled-out paths, and exact target files/symbols; scope belongs in structured scope fields.",
        length(min = 1, max = MAX_TASK_BYTES)
    )]
    pub task: String,
    #[schemars(description = "Optional short display name for this child task.")]
    #[serde(default, deserialize_with = "deserialize_optional_string_tolerant")]
    pub display_name: Option<String>,
    #[schemars(
        description = "Optional specialist role for this child (for example `explore` for read-only codebase research, `coder` for implementation and local checks, or `review` for independent skeptical verification). Omit to use the default coding role."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_string_tolerant")]
    pub role: Option<String>,
    #[schemars(
        schema_with = "max_model_turns_schema",
        description = "Optional positive child model-turn budget. Omit to use the configured default. If reached, wake the same child with `send_subagent_message` and `extra_model_turns` instead of respawning from scratch."
    )]
    #[serde(default)]
    pub max_model_turns: Option<u32>,
    #[schemars(
        description = "Optional wall-clock timeout in seconds for this child task. Omit to use the runtime default.",
        range(min = 1)
    )]
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Exact registered Merry tool names the child may use. Names are copied
    /// verbatim from the current tool list without provider namespace prefixes;
    /// use `run_process`, never `functions.run_process`.
    #[schemars(
        description = "Optional exact registered Merry tool names copied verbatim from the current tool list. Do not add provider namespace prefixes: use `run_process`, never `functions.run_process`."
    )]
    #[serde(default)]
    pub allowed_tools: Option<Vec<ToolName>>,
    #[schemars(
        schema_with = "optional_scope_paths_schema",
        description = "Optional normalized workspace-relative paths the child may read. Omit to inherit parent scope. Empty array authorizes no workspace reads."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_paths_tolerant")]
    pub read_scope: Option<Vec<String>>,
    #[schemars(
        schema_with = "optional_scope_paths_schema",
        description = "Optional normalized workspace-relative paths the child may write. Omit to inherit parent scope. Empty array makes the child read-only. Parallel writable children must have disjoint write scopes."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_paths_tolerant")]
    pub write_scope: Option<Vec<String>>,
    #[schemars(
        schema_with = "optional_scope_paths_schema",
        description = "Optional normalized workspace-relative paths the child must not access. Further restricts read_scope and write_scope."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_paths_tolerant")]
    pub forbidden_paths: Option<Vec<String>>,
    #[schemars(
        schema_with = "optional_context_files_schema",
        description = "Optional workspace-relative files or line ranges (`path/to/file.rs` or `path/to/file.rs:1-120`) to preload into the child's initial handoff context before turn 1."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_paths_tolerant")]
    pub context_files: Option<Vec<String>>,
    #[schemars(
        description = "Optional format or acceptance checklist for the child's final report (injected into `<merry_subagent_handoff>`)."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_string_tolerant")]
    pub expected_output: Option<String>,
    #[schemars(
        description = "Optional child model reasoning-effort override supported by the configured provider."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_string_tolerant")]
    pub reasoning_effort: Option<String>,
    #[schemars(
        description = "Optional filesystem isolation mode: `shared` (default, shares parent workspace) or `worktree` (isolated detached Git worktree outside the main repository; requires a Git workspace)."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_enum_tolerant")]
    pub workspace: Option<SubagentWorkspaceMode>,
    #[schemars(
        description = "Optional snapshot base when `workspace` is `worktree`: `working_tree` (default, includes parent's uncommitted tracked edits and small untracked files) or `head` (clean `HEAD`)."
    )]
    #[serde(default, deserialize_with = "deserialize_optional_enum_tolerant")]
    pub worktree_base: Option<SubagentWorktreeBase>,
}

/// Notice returned when a foreground wait hands active children off to background execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackgroundHandoffNotice {
    /// Child agent ids still running or queued in the background.
    pub handed_off_agent_ids: Vec<SubagentId>,
    /// Why the foreground wait ended (`foreground_budget_elapsed` or `parent_turn_interrupted`).
    pub reason: String,
    /// Explicit coordinator-facing note preventing duplicate spawns.
    pub message: String,
}

/// Provider-visible output for `spawn_subagents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentsOutput {
    /// Accepted child tasks.
    pub spawned: Vec<SpawnedSubagentView>,
    /// Rejected child tasks with compact reasons.
    pub rejected: Vec<RejectedSubagentView>,
    /// Optional wait result when `wait` was set to `all` or `any`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_result: Option<WaitSubagentsOutput>,
    /// Optional notice when foreground wait ended while children remain active in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_handoff: Option<BackgroundHandoffNotice>,
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
    /// Optional role name requested for this child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
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

/// Delivery policy for `send_subagent_message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubagentMessageDelivery {
    /// Inject at the next safe checkpoint (default; does not break active tool atomicity).
    #[default]
    Steer,
    /// Queue behind any already-buffered follow-up messages.
    Queue,
}

/// Provider-visible input for `send_subagent_message`.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "send_subagent_message",
    description = "Send a follow-up instruction to a running child agent or wake a finished/blocked child with its prior context intact."
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendSubagentMessageInput {
    #[schemars(description = "Target child agent id (for example `agent-1`).")]
    pub agent_id: SubagentId,
    #[schemars(
        description = "Non-blank follow-up instruction, new facts, or correction for the child agent.",
        length(min = 1, max = MAX_TASK_BYTES)
    )]
    pub message: String,
    #[schemars(
        description = "Optional delivery mode when the child is active: `steer` (default, next safe checkpoint) or `queue` (after buffered follow-ups). Ignored when waking a terminal child."
    )]
    #[serde(default)]
    pub delivery: Option<SubagentMessageDelivery>,
    #[schemars(
        description = "Optional positive model-turn budget for the resumed turn when waking a blocked or completed child.",
        range(min = 1)
    )]
    #[serde(default)]
    pub extra_model_turns: Option<u32>,
    #[schemars(
        description = "When true, wait up to `timeout_ms` (or the foreground budget) for the child to settle after delivering or waking."
    )]
    #[serde(default)]
    pub wait: Option<bool>,
    #[schemars(
        description = "Optional wait observation deadline in milliseconds when `wait` is true.",
        range(min = 0)
    )]
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Strongly typed, non-ambiguous delivery outcome for `send_subagent_message`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SendSubagentMessageOutcome {
    /// Message accepted for an active (`running` or `queued`) child.
    Accepted,
    /// Terminal (`completed`, `blocked`, `cancelled`, or `failed`) child woken with same `agent_id`.
    Woken,
    /// Unknown `agent_id` or child runtime cannot be woken.
    NotFoundOrNotOwned,
    /// Child follow-up queue is full.
    Saturated,
    /// Parent session is shutting down and no longer accepts messages.
    SessionClosing,
}

/// Provider-visible output for `send_subagent_message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendSubagentMessageOutput {
    /// Target child agent id.
    pub agent_id: SubagentId,
    /// Non-ambiguous typed delivery outcome.
    pub outcome: SendSubagentMessageOutcome,
    /// Current child status label, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SubagentStatusLabel>,
    /// Optional wait result when `wait` was true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_result: Option<WaitSubagentsOutput>,
    /// Concise human/coordinator-readable explanation.
    pub message: String,
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
///
/// Tolerates both `agent_ids: [...]`, `agent_ids: "agent-1"`, and the singular
/// alias `agent_id: "agent-1"`.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "wait_subagents",
    description = "Inspect or wait for child agent statuses and compact results."
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WaitSubagentsInput {
    #[schemars(
        description = "Child agent ids to inspect or wait on. Provide at least one id; the result contains only these selected agents.",
        length(min = 1)
    )]
    pub agent_ids: Vec<SubagentId>,
    #[schemars(
        description = "Optional completion mode. Use `any` for the first terminal child or `all` for every selected child; omit it for `all`."
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

impl<'de> Deserialize<'de> for WaitSubagentsInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(default, deserialize_with = "deserialize_optional_agent_ids_field")]
            agent_ids: Option<Vec<SubagentId>>,
            #[serde(default)]
            agent_id: Option<SubagentId>,
            #[serde(default)]
            mode: Option<WaitMode>,
            #[serde(default)]
            timeout_ms: Option<u64>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let agent_ids = merge_agent_ids(wire.agent_ids, wire.agent_id)?;
        Ok(Self {
            agent_ids,
            mode: wire.mode,
            timeout_ms: wire.timeout_ms,
        })
    }
}

/// Provider-visible input for `cancel_subagents`.
///
/// Tolerates both `agent_ids: [...]`, `agent_ids: "agent-1"`, and the singular
/// alias `agent_id: "agent-1"`.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "cancel_subagents",
    description = "Cancel selected child agents."
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CancelSubagentsInput {
    #[schemars(
        description = "Child agent ids to cancel. Provide at least one id; the result contains only these selected agents.",
        length(min = 1)
    )]
    pub agent_ids: Vec<SubagentId>,
}

impl<'de> Deserialize<'de> for CancelSubagentsInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            #[serde(default, deserialize_with = "deserialize_optional_agent_ids_field")]
            agent_ids: Option<Vec<SubagentId>>,
            #[serde(default)]
            agent_id: Option<SubagentId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let agent_ids = merge_agent_ids(wire.agent_ids, wire.agent_id)?;
        Ok(Self { agent_ids })
    }
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

fn optional_context_files_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::Schema::try_from(json!({
        "description": "Optional workspace-relative files or line ranges (`path/to/file.rs` or `path/to/file.rs:1-120`) to preload into the child's handoff context.",
        "anyOf": [
            { "type": "null" },
            {
                "type": "array",
                "items": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Normalized workspace-relative file path with optional `:start-end` 1-based line range."
                }
            }
        ]
    }))
    .expect("static optional context_files schema is valid")
}

fn max_model_turns_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::Schema::try_from(json!({
        "type": "integer",
        "minimum": DEFAULT_MIN_MODEL_TURNS,
        "default": super::spec::DEFAULT_MAX_MODEL_TURNS,
        "description": "Optional positive child lifecycle model-turn budget. Omit for the configured default. If reached, wake the same child with `send_subagent_message` and `extra_model_turns`."
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
    /// Explicit note when non-terminal children remain running in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_note: Option<String>,
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
            .collect::<Vec<_>>();
        let handoff_note = (timed_out && !terminal && !pending_agent_ids.is_empty()).then(|| {
            "Pending subagents were NOT cancelled and are still running in the background. Do not spawn them again; wait or rely on automatic completion notification.".to_owned()
        });
        Self {
            agents,
            timed_out,
            terminal,
            pending_agent_ids,
            handoff_note,
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
    /// Child task stopped before completion and can be woken or replaced.
    Blocked,
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
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
        }
    }

    pub(super) fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Blocked | Self::Cancelled
        )
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
    /// Optional short display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Optional specialist role name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
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
    /// Cumulative model turns executed by this child.
    #[serde(default)]
    pub turns_used: u32,
    /// Cumulative tool calls resolved by this child.
    #[serde(default)]
    pub tool_calls: u32,
    /// Wall-clock elapsed execution time in milliseconds.
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Cumulative model token usage reported by this child, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ModelUsage>,
    /// Monotonic count of raw assistant and tool output bytes observed so far
    /// (progress signature for polling loops).
    #[serde(default)]
    pub raw_output_bytes: u64,
    /// Optional isolated Git worktree metadata when the child ran with `workspace: "worktree"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<SubagentWorktreeView>,
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
            display_name: None,
            role: None,
            status: SubagentStatusLabel::Completed,
            summary: summary.into(),
            result: None,
            output_paths,
            changed_paths,
            diagnostics: None,
            turns_used: 0,
            tool_calls: 0,
            elapsed_ms: 0,
            usage: None,
            raw_output_bytes: 0,
            worktree: None,
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
