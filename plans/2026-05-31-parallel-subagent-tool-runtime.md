# Parallel Subagent Tool Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deterministic first slice of parallel subagent delegation where the parent agent uses runtime-owned tools to spawn bounded child runtimes, wait for compact status/results, and continue from normal tool results.

**Architecture:** Subagents are implemented as runtime-owned tool executors backed by a shared `SubagentManager`. The manager owns child task specs, lifecycle status, concurrency/depth guards, child runtime construction, and compact result state; large inter-agent data is represented as shared workspace paths, not artifact pass-through. The first slice is runtime-first and offline-testable with fake providers.

**Tech Stack:** Rust 2024, Tokio tasks, `serde`/`schemars` for provider-visible tool schemas, `merry-core` event/id protocol types, `merry-runtime` `Runtime::run_agent_loop`, deterministic fake `ModelProvider` and test executors.

---

## Design Inputs

- Spec: `specs/2026-05-31-parallel-subagent-tool-runtime.md`.
- Current runtime loop: `crates/merry-runtime/src/agent_loop.rs`.
- Current tool boundary: `crates/merry-runtime/src/tool.rs`.
- Current runtime builder/model config: `crates/merry-runtime/src/runtime.rs` and `crates/merry-runtime/src/model_config.rs`.
- Current event protocol: `crates/merry-core/src/event.rs`.
- User correction: subagents are tools and v1 must support parallel execution.
- User correction: do not implement cross-agent artifact handoff in v1; use a shared filesystem/scratch workspace as the data plane.
- User correction: parent/main agent owns child task boundaries such as allowed tools and write scopes; child workers are disposable.

## Scope

This plan implements the first deterministic runtime-visible subagent slice:

- provider-visible `spawn_subagents`, `wait_subagents`, and `cancel_subagents` tool specs;
- a runtime-owned `SubagentManager` that starts child runtimes concurrently;
- child runtimes with task anchors, restricted tools, and depth=1 no-grandchild behavior;
- compact status/result management for `wait_subagents`;
- conflict rejection for overlapping child write scopes before spawn;
- provider-neutral lifecycle events for child spawn/start/completion/failure/cancellation;
- deterministic parent-loop test that spawns two child workers in parallel and continues from wait output.

This plan does not implement:

- artifact pass-through between sessions;
- TUI child panels;
- persistent SQLite job storage;
- interactive `send_subagent_input`;
- grandchildren;
- intelligent merge;
- live provider smoke;
- CLI command UX beyond optional config/example plumbing after runtime behavior is proven.

## File Structure

- Modify `crates/merry-core/src/id.rs`: add `SubagentId` and `SubagentTaskId` newtypes.
- Modify `crates/merry-core/src/lib.rs`: export the new ids.
- Modify `crates/merry-core/src/event.rs`: add provider-neutral subagent lifecycle event variants.
- Modify `crates/merry-core/tests/protocol.rs`: add JSON shape/round-trip tests for the new ids and events.
- Create `crates/merry-runtime/src/subagent.rs`: own subagent task/status types, validation, manager, tool specs, tool executors, child runtime factory trait, and wait/cancel behavior.
- Modify `crates/merry-runtime/src/lib.rs`: export public subagent config/types and register the module.
- Modify `crates/merry-runtime/src/runtime.rs`: add `RuntimeBuilder::subagent_manager(...)`, store an optional manager, expose `Runtime::subagent_snapshot`, and drain subagent lifecycle events at the normal tool-resolution boundary.
- Modify `crates/merry-runtime/src/ledger.rs`: add lifecycle fact variants for subagent events.
- Modify `crates/merry-runtime/src/session.rs`: add event-recording helpers for subagent lifecycle events.
- Modify `crates/merry-runtime/tests/agent_loop.rs`: add deterministic parent/child fake-provider tests for parallel spawn/wait and depth restrictions.
- Modify `crates/merry-runtime/tests/provider_boundary.rs` only if event-kind helpers need the new events.
- Modify `examples/config.toml` and `crates/merry-cli/src/config.rs` only in the final task if the runtime slice needs visible config defaults.

## Acceptance Commands

Focused checks after each task:

```bash
cargo test -p merry-core subagent
cargo test -p merry-runtime subagent
cargo test -p merry-runtime spawn_subagents_starts_parallel_child_runtimes_and_parent_continues
cargo test -p merry-runtime child_runtime_receives_task_anchor_and_restricted_tools
cargo test -p merry-runtime wait_subagents_returns_compact_status_and_paths
cargo test -p merry-runtime conflicting_child_write_scopes_are_rejected_before_spawn
cargo test -p merry-runtime child_depth_one_cannot_spawn_grandchildren
```

Broader checks before reporting implementation complete:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

No live provider test is required for this plan.

## Task 1: Core IDs And Runtime Events

**Files:**
- Modify: `crates/merry-core/src/id.rs`
- Modify: `crates/merry-core/src/lib.rs`
- Modify: `crates/merry-core/src/event.rs`
- Modify: `crates/merry-core/tests/protocol.rs`

- [ ] **Step 1: Write failing ID and event protocol tests**

Append these tests to `crates/merry-core/tests/protocol.rs`:

```rust
#[test]
fn subagent_ids_validate_and_round_trip_as_json_strings() {
    let agent_id = SubagentId::new("agent-1").expect("valid subagent id");
    let task_id = SubagentTaskId::new("task-1").expect("valid subagent task id");

    assert_eq!(
        serde_json::to_value(&agent_id).expect("id serializes"),
        json!("agent-1")
    );
    assert_eq!(
        serde_json::from_value::<SubagentId>(json!("agent-1")).expect("id deserializes"),
        agent_id
    );
    assert_eq!(
        serde_json::to_value(&task_id).expect("task id serializes"),
        json!("task-1")
    );
    assert_eq!(
        serde_json::from_value::<SubagentTaskId>(json!("task-1")).expect("task id deserializes"),
        task_id
    );
}

#[test]
fn subagent_spawned_event_uses_snake_case_and_round_trips() {
    let event = RuntimeEvent::new(
        SessionId::new("parent").expect("valid session id"),
        12,
        RuntimeEventKind::SubagentSpawned {
            agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
            task_id: SubagentTaskId::new("task-1").expect("valid task id"),
            task_anchor: "Review src/lib.rs for risks.".to_owned(),
        },
    );

    assert_eq!(
        serde_json::to_value(&event).expect("event serializes"),
        json!({
            "session_id": "parent",
            "sequence": 12,
            "kind": {
                "type": "subagent_spawned",
                "agent_id": "agent-1",
                "task_id": "task-1",
                "task_anchor": "Review src/lib.rs for risks."
            }
        })
    );
    assert_json_round_trip(&event);
}

#[test]
fn subagent_terminal_events_do_not_embed_large_payloads() {
    let completed = RuntimeEvent::new(
        SessionId::new("parent").expect("valid session id"),
        13,
        RuntimeEventKind::SubagentCompleted {
            agent_id: SubagentId::new("agent-1").expect("valid subagent id"),
            task_id: SubagentTaskId::new("task-1").expect("valid task id"),
            summary: "Found one risk; details are in shared/subagents/agent-1/result.md."
                .to_owned(),
            output_paths: vec!["shared/subagents/agent-1/result.md".to_owned()],
            changed_paths: Vec::new(),
        },
    );
    assert_eq!(
        serde_json::to_value(&completed).expect("event serializes"),
        json!({
            "session_id": "parent",
            "sequence": 13,
            "kind": {
                "type": "subagent_completed",
                "agent_id": "agent-1",
                "task_id": "task-1",
                "summary": "Found one risk; details are in shared/subagents/agent-1/result.md.",
                "output_paths": ["shared/subagents/agent-1/result.md"],
                "changed_paths": []
            }
        })
    );
    assert_json_round_trip(&completed);
}
```

Also update the top import in the same file to include:

```rust
SubagentId, SubagentTaskId,
```

- [ ] **Step 2: Run the tests and confirm failure**

Run:

```bash
cargo test -p merry-core subagent
```

Expected: FAIL because `SubagentId`, `SubagentTaskId`, and event variants do not exist.

- [ ] **Step 3: Add core ids**

In `crates/merry-core/src/id.rs`, add these next to the existing `define_id!` calls:

```rust
define_id!(SubagentId, "SubagentId");
define_id!(SubagentTaskId, "SubagentTaskId");
```

In `crates/merry-core/src/lib.rs`, change the id export to:

```rust
pub use id::{
    ArtifactId, ProviderName, SessionId, SkillId, SubagentId, SubagentTaskId, ToolCallId,
    ToolName,
};
```

- [ ] **Step 4: Add runtime event variants**

In `crates/merry-core/src/event.rs`, add `SubagentId` and `SubagentTaskId` to the `use crate::{...}` list.

Add these variants to `RuntimeEventKind` after `SkillUsed`:

```rust
/// A child agent task was accepted by runtime and assigned an id.
SubagentSpawned {
    /// Runtime-owned child agent id.
    agent_id: SubagentId,
    /// Parent-assigned or runtime-assigned task id.
    task_id: SubagentTaskId,
    /// Compact task anchor assigned to the child runtime.
    task_anchor: String,
},
/// A child agent began running its bounded loop.
SubagentStarted {
    /// Runtime-owned child agent id.
    agent_id: SubagentId,
    /// Task id associated with the child.
    task_id: SubagentTaskId,
},
/// A child agent status changed without producing a terminal result.
SubagentStatusChanged {
    /// Runtime-owned child agent id.
    agent_id: SubagentId,
    /// Task id associated with the child.
    task_id: SubagentTaskId,
    /// Compact status label such as queued or running.
    status: String,
},
/// A child agent completed and published a compact result.
SubagentCompleted {
    /// Runtime-owned child agent id.
    agent_id: SubagentId,
    /// Task id associated with the child.
    task_id: SubagentTaskId,
    /// Compact provider-visible result summary.
    summary: String,
    /// Shared-workspace output paths for exact follow-up reads.
    output_paths: Vec<String>,
    /// Shared-workspace paths changed by the child.
    changed_paths: Vec<String>,
},
/// A child agent failed.
SubagentFailed {
    /// Runtime-owned child agent id.
    agent_id: SubagentId,
    /// Task id associated with the child.
    task_id: SubagentTaskId,
    /// Stable diagnostic.
    diagnostic: ErrorInfo,
},
/// A child agent was cancelled.
SubagentCancelled {
    /// Runtime-owned child agent id.
    agent_id: SubagentId,
    /// Task id associated with the child.
    task_id: SubagentTaskId,
    /// Stable diagnostic.
    diagnostic: ErrorInfo,
},
```

- [ ] **Step 5: Run core protocol tests**

Run:

```bash
cargo test -p merry-core subagent
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-core/src/id.rs crates/merry-core/src/lib.rs crates/merry-core/src/event.rs crates/merry-core/tests/protocol.rs
git commit -m "feat(core): add subagent lifecycle protocol"
```

## Task 2: Subagent Data Types, Validation, And Tool Schemas

**Files:**
- Create: `crates/merry-runtime/src/subagent.rs`
- Modify: `crates/merry-runtime/src/lib.rs`

- [ ] **Step 1: Write failing runtime subagent validation/schema tests**

Create `crates/merry-runtime/src/subagent.rs` with these tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::ToolName;
    use serde_json::{Value, json};

    #[test]
    fn subagent_task_rejects_blank_task_and_zero_steps() {
        let blank = SubagentTaskSpec::new(" ", 4).expect_err("blank task should fail");
        assert!(blank.to_string().contains("task must not be blank"));

        let zero = SubagentTaskSpec::new("Review src/lib.rs.", 0)
            .expect_err("zero max steps should fail");
        assert!(zero.to_string().contains("max_steps must be greater than zero"));
    }

    #[test]
    fn conflicting_child_write_scopes_are_rejected_before_spawn() {
        let first = SubagentTaskSpec::new("Edit runtime module.", 4)
            .expect("valid task")
            .with_write_scope(["src/runtime.rs"])
            .expect("valid scope");
        let second = SubagentTaskSpec::new("Edit nested function.", 4)
            .expect("valid task")
            .with_write_scope(["src/runtime.rs"])
            .expect("valid scope");

        let error = validate_no_write_scope_conflicts(&[first, second])
            .expect_err("same write scope should conflict");
        assert!(error.to_string().contains("overlapping write scope"));
    }

    #[test]
    fn read_only_tasks_do_not_conflict() {
        let first = SubagentTaskSpec::new("Read runtime module.", 4)
            .expect("valid task")
            .with_read_scope(["src/runtime.rs"])
            .expect("valid scope");
        let second = SubagentTaskSpec::new("Read runtime tests.", 4)
            .expect("valid task")
            .with_read_scope(["src/runtime.rs"])
            .expect("valid scope");

        validate_no_write_scope_conflicts(&[first, second]).expect("read-only tasks do not conflict");
    }

    #[test]
    fn spawn_wait_and_cancel_tool_specs_have_stable_names() {
        let specs = subagent_tool_specs().expect("tool specs should build");
        let names = specs
            .iter()
            .map(|spec| spec.name().as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["spawn_subagents", "wait_subagents", "cancel_subagents"]);

        for spec in specs {
            let value = serde_json::to_value(spec.input_schema()).expect("schema serializes");
            assert!(matches!(value, Value::Object(_)));
        }
    }

    #[test]
    fn wait_output_serializes_compact_status_without_transcripts() {
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
                    "diagnostic": null
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
```

- [ ] **Step 2: Run tests and confirm failure**

Run:

```bash
cargo test -p merry-runtime subagent
```

Expected: FAIL because the module is not wired and the types/functions are missing.

- [ ] **Step 3: Wire the module and public exports**

In `crates/merry-runtime/src/lib.rs`, add:

```rust
mod subagent;
```

Add exports:

```rust
pub use subagent::{
    CancelSubagentsInput, ChildRuntimeFactory, ChildRuntimeInput, SpawnSubagentsInput,
    SpawnSubagentsOutput, SubagentConfig, SubagentError, SubagentManager, SubagentStatusLabel,
    SubagentStatusView, SubagentTaskSpec, WaitMode, WaitSubagentsInput, WaitSubagentsOutput,
    subagent_registered_tools, subagent_tool_specs,
};
```

- [ ] **Step 4: Implement subagent types and validation**

In `crates/merry-runtime/src/subagent.rs`, implement these public shapes:

```rust
//! Runtime-owned parallel subagent tools and manager.

use merry_core::{
    ErrorInfo, SubagentId, SubagentTaskId, ToolInputSchema, ToolName, ToolSpec,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::{Component, Path, PathBuf}};
use thiserror::Error;

const SPAWN_SUBAGENTS_TOOL_NAME: &str = "spawn_subagents";
const WAIT_SUBAGENTS_TOOL_NAME: &str = "wait_subagents";
const CANCEL_SUBAGENTS_TOOL_NAME: &str = "cancel_subagents";
const MAX_TASK_BYTES: usize = 16 * 1024;
const DEFAULT_MAX_STEPS: u32 = 8;
const DEFAULT_MAX_THREADS: usize = 6;
const DEFAULT_MAX_DEPTH: u8 = 1;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SubagentError {
    #[error("task must not be blank")]
    BlankTask,
    #[error("task is longer than the allowed maximum")]
    TaskTooLong,
    #[error("max_steps must be greater than zero")]
    ZeroMaxSteps,
    #[error("scope path must be relative and normalized: {path}")]
    InvalidScopePath { path: String },
    #[error("overlapping write scope between task {first_index} and task {second_index}: {path}")]
    OverlappingWriteScope {
        first_index: usize,
        second_index: usize,
        path: String,
    },
    #[error("subagent max_threads must be greater than zero")]
    ZeroMaxThreads,
    #[error("subagent max_depth must be greater than zero")]
    ZeroMaxDepth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentConfig {
    max_threads: usize,
    max_depth: u8,
}

impl SubagentConfig {
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

    #[must_use]
    pub fn max_threads(self) -> usize {
        self.max_threads
    }

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
```

Add `SubagentTaskSpec`:

```rust
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

    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    #[must_use]
    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }

    #[must_use]
    pub fn allowed_tools(&self) -> &[ToolName] {
        &self.allowed_tools
    }

    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    #[must_use]
    pub fn write_scope(&self) -> &[PathBuf] {
        &self.write_scope
    }

    pub fn with_allowed_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = ToolName>,
    {
        self.allowed_tools = tools.into_iter().collect();
        self
    }

    pub fn with_display_name(mut self, display_name: Option<String>) -> Self {
        self.display_name = display_name.filter(|value| !value.trim().is_empty());
        self
    }

    pub fn with_read_scope<I, P>(mut self, paths: I) -> Result<Self, SubagentError>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.read_scope = validate_scope_paths(paths)?;
        Ok(self)
    }

    pub fn with_write_scope<I, P>(mut self, paths: I) -> Result<Self, SubagentError>
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.write_scope = validate_scope_paths(paths)?;
        Ok(self)
    }

}
```

Add validation helpers:

```rust
fn validate_task_text(task: &str) -> Result<(), SubagentError> {
    if task.trim().is_empty() {
        return Err(SubagentError::BlankTask);
    }
    if task.as_bytes().len() > MAX_TASK_BYTES {
        return Err(SubagentError::TaskTooLong);
    }
    Ok(())
}

fn validate_scope_paths<I, P>(paths: I) -> Result<Vec<PathBuf>, SubagentError>
where
    I: IntoIterator<Item = P>,
    P: Into<PathBuf>,
{
    paths
        .into_iter()
        .map(|path| validate_scope_path(path.into()))
        .collect()
}

fn validate_scope_path(path: PathBuf) -> Result<PathBuf, SubagentError> {
    let invalid = path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if invalid || path.as_os_str().is_empty() {
        return Err(SubagentError::InvalidScopePath {
            path: path.display().to_string(),
        });
    }
    Ok(path)
}

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

fn paths_overlap(first: &Path, second: &Path) -> bool {
    first == second || first.starts_with(second) || second.starts_with(first)
}
```

- [ ] **Step 5: Implement provider-visible input/output types and schemas**

Add these serializable types in the same file:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentsInput {
    pub tasks: Vec<SpawnSubagentTaskInput>,
    pub max_concurrency: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentTaskInput {
    pub task: String,
    pub display_name: Option<String>,
    pub max_steps: Option<u32>,
    pub allowed_tools: Option<Vec<ToolName>>,
    pub read_scope: Option<Vec<String>>,
    pub write_scope: Option<Vec<String>>,
    pub forbidden_paths: Option<Vec<String>>,
    pub expected_output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnSubagentsOutput {
    pub spawned: Vec<SpawnedSubagentView>,
    pub rejected: Vec<RejectedSubagentView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnedSubagentView {
    pub agent_id: SubagentId,
    pub task_id: SubagentTaskId,
    pub display_name: Option<String>,
    pub status: String,
    pub task_anchor: String,
    pub read_scope: Vec<String>,
    pub write_scope: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RejectedSubagentView {
    pub task_index: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitMode {
    Any,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitSubagentsInput {
    pub agent_ids: Vec<SubagentId>,
    pub mode: Option<WaitMode>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelSubagentsInput {
    pub agent_ids: Vec<SubagentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitSubagentsOutput {
    pub agents: Vec<SubagentStatusView>,
}

impl WaitSubagentsOutput {
    #[must_use]
    pub fn new(agents: Vec<SubagentStatusView>) -> Self {
        Self { agents }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatusLabel {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl SubagentStatusLabel {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubagentStatusView {
    pub agent_id: SubagentId,
    pub task_id: SubagentTaskId,
    pub status: SubagentStatusLabel,
    pub summary: String,
    pub output_paths: Vec<String>,
    pub changed_paths: Vec<String>,
    pub diagnostic: Option<ErrorInfo>,
}

impl SubagentStatusView {
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
            diagnostic: None,
        }
    }
}
```

Add `subagent_tool_specs()`:

```rust
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
    let schema = schemars::schema_for!(T);
    ToolSpec::new(
        ToolName::new(name)?,
        description,
        ToolInputSchema::new(schema)?,
    )
}
```

- [ ] **Step 6: Run runtime subagent tests**

Run:

```bash
cargo test -p merry-runtime subagent
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/merry-runtime/src/subagent.rs crates/merry-runtime/src/lib.rs
git commit -m "feat(runtime): add subagent task protocol"
```

## Task 3: Subagent Manager And Child Runtime Factory

**Files:**
- Modify: `crates/merry-runtime/src/subagent.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/src/lib.rs`

- [ ] **Step 1: Add failing manager tests**

Append these tests to `crates/merry-runtime/src/subagent.rs`:

```rust
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

    #[tokio::test(flavor = "current_thread")]
    async fn manager_rejects_overlapping_write_scopes_before_spawn() {
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::default(),
            Arc::new(FakeChildFactory::new()),
        );
        let first = SubagentTaskSpec::new("Edit one.", 2)
            .expect("valid")
            .with_write_scope(["src/lib.rs"])
            .expect("valid scope");
        let second = SubagentTaskSpec::new("Edit two.", 2)
            .expect("valid")
            .with_write_scope(["src/lib.rs"])
            .expect("valid scope");

        let output = manager
            .spawn(vec![first, second], Some(2), CancellationToken::new())
            .await
            .expect("spawn tool should return a structured result");

        assert!(output.spawned.is_empty());
        assert_eq!(output.rejected.len(), 2);
        assert_eq!(manager.snapshot().await.len(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_starts_children_under_max_concurrency() {
        let factory = Arc::new(FakeChildFactory::new());
        let manager = SubagentManager::new(
            SessionId::new("parent").expect("valid id"),
            SubagentConfig::new(4, 1).expect("valid config"),
            factory.clone(),
        );
        let first = SubagentTaskSpec::new("First task.", 2).expect("valid");
        let second = SubagentTaskSpec::new("Second task.", 2).expect("valid");

        let output = manager
            .spawn(vec![first, second], Some(2), CancellationToken::new())
            .await
            .expect("spawn should succeed");

        assert_eq!(output.spawned.len(), 2);
        assert_eq!(factory.started.load(Ordering::SeqCst), 2);
        assert_eq!(manager.snapshot().await.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_returns_child_statuses() {
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
    }
}
```

- [ ] **Step 2: Run tests and confirm failure**

Run:

```bash
cargo test -p merry-runtime manager_
```

Expected: FAIL because `SubagentManager`, `ChildRuntimeFactory`, and related methods do not exist.

- [ ] **Step 3: Add child factory and manager skeleton**

In `crates/merry-runtime/src/subagent.rs`, add:

```rust
use crate::{AgentLoopConfig, AgentLoopStatus, Runtime, RuntimeError, StepContext, StepInput, TaskAnchor};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ChildRuntimeInput {
    pub session_id: SessionId,
    pub task_anchor: TaskAnchor,
    pub task: SubagentTaskSpec,
    pub allowed_tools: Vec<ToolName>,
    pub depth: u8,
}

pub trait ChildRuntimeFactory: Send + Sync {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError>;
}

#[derive(Clone)]
pub struct SubagentManager {
    parent_session_id: SessionId,
    config: SubagentConfig,
    factory: Arc<dyn ChildRuntimeFactory>,
    state: Arc<Mutex<SubagentManagerState>>,
    notify: Arc<Notify>,
    next_id: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct SubagentManagerState {
    agents: BTreeMap<SubagentId, ManagedSubagent>,
}

#[derive(Debug, Clone)]
struct ManagedSubagent {
    agent_id: SubagentId,
    task_id: SubagentTaskId,
    task: SubagentTaskSpec,
    status: SubagentStatusLabel,
    summary: String,
    output_paths: Vec<String>,
    changed_paths: Vec<String>,
    diagnostic: Option<ErrorInfo>,
    cancellation_token: CancellationToken,
}
```

Implement `SubagentManager::new`, `snapshot`, `spawn`, `wait`, and `cancel`:

```rust
impl SubagentManager {
    pub fn new(
        parent_session_id: SessionId,
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
        }
    }

    pub async fn snapshot(&self) -> Vec<SubagentStatusView> {
        let state = self.state.lock().await;
        state.agents.values().map(ManagedSubagent::status_view).collect()
    }

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

        let max_concurrency = max_concurrency
            .unwrap_or(tasks.len())
            .min(self.config.max_threads())
            .max(1);
        let mut to_start = Vec::new();
        let mut spawned = Vec::new();
        let rejected = Vec::new();

        for (index, task) in tasks.into_iter().enumerate() {
            let number = self.next_id.fetch_add(1, Ordering::SeqCst);
            let agent_id = SubagentId::new(&format!("{}-child-{number}", self.parent_session_id))?;
            let task_id = SubagentTaskId::new(&format!("task-{number}"))?;
            let task_anchor = TaskAnchor::new(task.task()).map_err(RuntimeError::from)?;
            let child_token = parent_token.child_token();
            let status = SubagentStatusLabel::Queued;
            let managed = ManagedSubagent {
                agent_id: agent_id.clone(),
                task_id: task_id.clone(),
                task: task.clone(),
                status: status.clone(),
                summary: String::new(),
                output_paths: Vec::new(),
                changed_paths: Vec::new(),
                diagnostic: None,
                cancellation_token: child_token.clone(),
            };
            {
                let mut state = self.state.lock().await;
                state.agents.insert(agent_id.clone(), managed);
            }
            spawned.push(SpawnedSubagentView {
                agent_id: agent_id.clone(),
                task_id: task_id.clone(),
                display_name: task.display_name().map(str::to_owned),
                status: status.as_str().to_owned(),
                task_anchor: task.task().to_owned(),
                read_scope: task.read_scope.iter().map(|path| path.display().to_string()).collect(),
                write_scope: task.write_scope.iter().map(|path| path.display().to_string()).collect(),
            });
            if index < max_concurrency {
                to_start.push((agent_id, task_id, task, task_anchor, child_token));
            }
        }

        for (agent_id, task_id, task, task_anchor, child_token) in to_start {
            self.start_child(agent_id, task_id, task, task_anchor, child_token)?;
        }

        Ok(SpawnSubagentsOutput { spawned, rejected })
    }

    pub async fn wait(
        &self,
        agent_ids: &[SubagentId],
        mode: WaitMode,
        timeout: Option<Duration>,
    ) -> Result<WaitSubagentsOutput, RuntimeError> {
        let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);
        loop {
            let output = self.status_for(agent_ids).await;
            let ready = match mode {
                WaitMode::Any => output.agents.iter().any(SubagentStatusView::is_terminal),
                WaitMode::All => output.agents.iter().all(SubagentStatusView::is_terminal),
            };
            if ready {
                return Ok(output);
            }
            if let Some(deadline) = deadline {
                if tokio::time::Instant::now() >= deadline {
                    return Ok(output);
                }
            }
            self.notify.notified().await;
        }
    }

    pub async fn cancel(&self, agent_ids: &[SubagentId]) -> Result<WaitSubagentsOutput, RuntimeError> {
        let mut state = self.state.lock().await;
        for agent_id in agent_ids {
            if let Some(agent) = state.agents.get_mut(agent_id) {
                agent.cancellation_token.cancel();
                agent.status = SubagentStatusLabel::Cancelled;
                agent.summary = "cancelled by parent".to_owned();
            }
        }
        self.notify.notify_waiters();
        Ok(WaitSubagentsOutput {
            agents: state
                .agents
                .values()
                .filter(|agent| agent_ids.contains(&agent.agent_id))
                .map(ManagedSubagent::status_view)
                .collect(),
        })
    }
}
```

Add helper methods:

```rust
impl SubagentManager {
    fn start_child(
        &self,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        task: SubagentTaskSpec,
        task_anchor: TaskAnchor,
        token: CancellationToken,
    ) -> Result<(), RuntimeError> {
        let runtime = self.factory.build_child(ChildRuntimeInput {
            session_id: SessionId::new(agent_id.as_str())?,
            task_anchor,
            task: task.clone(),
            allowed_tools: task.allowed_tools().to_vec(),
            depth: 1,
        })?;
        let state = Arc::clone(&self.state);
        let notify = Arc::clone(&self.notify);
        tokio::spawn(async move {
            {
                let mut state = state.lock().await;
                if let Some(agent) = state.agents.get_mut(&agent_id) {
                    agent.status = SubagentStatusLabel::Running;
                }
            }
            notify.notify_waiters();

            let result = runtime
                .run_agent_loop(
                    StepInput::user_text(task.task()).expect("validated task remains valid input"),
                    StepContext::new(token.clone()),
                    AgentLoopConfig::new(task.max_steps() as usize)
                        .expect("validated max_steps is non-zero"),
                )
                .await;

            let mut state = state.lock().await;
            if let Some(agent) = state.agents.get_mut(&agent_id) {
                match result {
                    Ok(loop_result) => {
                        agent.status = match loop_result.status() {
                            AgentLoopStatus::Completed => SubagentStatusLabel::Completed,
                            AgentLoopStatus::Cancelled { .. } => SubagentStatusLabel::Cancelled,
                            AgentLoopStatus::Failed { .. } | AgentLoopStatus::Blocked { .. } => {
                                SubagentStatusLabel::Failed
                            }
                        };
                        agent.summary = subagent_summary_from_status(loop_result.status());
                        agent.output_paths = vec![format!(
                            "shared/subagents/{}/result.md",
                            agent.agent_id.as_str()
                        )];
                    }
                    Err(error) => {
                        agent.status = SubagentStatusLabel::Failed;
                        agent.summary = "child runtime error".to_owned();
                        agent.diagnostic = Some(
                            ErrorInfo::new("subagent.runtime_error", &error.to_string())
                                .expect("static code and runtime message are valid diagnostic"),
                        );
                    }
                }
            }
            notify.notify_waiters();
        });
        Ok(())
    }

    async fn status_for(&self, agent_ids: &[SubagentId]) -> WaitSubagentsOutput {
        let state = self.state.lock().await;
        WaitSubagentsOutput {
            agents: state
                .agents
                .values()
                .filter(|agent| agent_ids.contains(&agent.agent_id))
                .map(ManagedSubagent::status_view)
                .collect(),
        }
    }
}

impl ManagedSubagent {
    fn status_view(&self) -> SubagentStatusView {
        SubagentStatusView {
            agent_id: self.agent_id.clone(),
            task_id: self.task_id.clone(),
            status: self.status.clone(),
            summary: self.summary.clone(),
            output_paths: self.output_paths.clone(),
            changed_paths: self.changed_paths.clone(),
            diagnostic: self.diagnostic.clone(),
        }
    }
}

impl SubagentStatusView {
    fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            SubagentStatusLabel::Completed
                | SubagentStatusLabel::Failed
                | SubagentStatusLabel::Cancelled
        )
    }
}

fn subagent_summary_from_status(status: &AgentLoopStatus) -> String {
    match status {
        AgentLoopStatus::Completed => "child completed".to_owned(),
        AgentLoopStatus::Failed { diagnostic } => {
            format!("child failed: {}", diagnostic.message())
        }
        AgentLoopStatus::Cancelled { diagnostic } => {
            format!("child cancelled: {}", diagnostic.message())
        }
        AgentLoopStatus::Blocked { reason } => format!("child blocked: {reason:?}"),
    }
}
```

- [ ] **Step 4: Add runtime builder storage for the manager**

In `crates/merry-runtime/src/runtime.rs`, add `SubagentManager` imports and an optional manager field:

```rust
SubagentConfig, SubagentManager,
```

Add to `RuntimeBuilder`:

```rust
subagent_manager: Option<SubagentManager>,
```

Initialize it to `None`.

Add builder method:

```rust
#[must_use]
pub fn subagent_manager(mut self, manager: SubagentManager) -> Self {
    self.subagent_manager = Some(manager);
    self
}
```

Add to `RuntimeInner`:

```rust
subagent_manager: Option<SubagentManager>,
```

Pass the builder value in `build`.

Add runtime method:

```rust
pub async fn subagent_snapshot(&self) -> Option<Vec<crate::SubagentStatusView>> {
    match &self.inner.subagent_manager {
        Some(manager) => Some(manager.snapshot().await),
        None => None,
    }
}
```

- [ ] **Step 5: Run manager tests**

Run:

```bash
cargo test -p merry-runtime manager_
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-runtime/src/subagent.rs crates/merry-runtime/src/runtime.rs crates/merry-runtime/src/lib.rs
git commit -m "feat(runtime): add subagent manager"
```

## Task 4: Subagent Tool Executors And Parent Tool Registration

**Files:**
- Modify: `crates/merry-runtime/src/subagent.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/src/lib.rs`
- Test: `crates/merry-runtime/src/subagent.rs`

- [ ] **Step 1: Add failing tool executor tests**

Append to `crates/merry-runtime/src/subagent.rs` tests:

```rust
#[cfg(test)]
mod tool_tests {
    use super::*;
    use crate::{ToolExecutionContext, ToolExecutor};
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use serde_json::{Value, json};
    use std::sync::Arc;

    fn pending_call(name: &str, args: Value) -> PendingToolCall {
        let object = args.as_object().expect("object args").clone();
        PendingToolCall::new(
            ToolCallId::new("call-subagent").expect("valid call id"),
            ToolName::new(name).expect("valid tool name"),
            ToolCallArguments::new(object),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_tool_returns_structured_spawn_output() {
        let manager = test_manager();
        let executor = SpawnSubagentsExecutor::new(manager);
        let call = pending_call(
            SPAWN_SUBAGENTS_TOOL_NAME,
            json!({
                "tasks": [
                    { "task": "Read src/lib.rs.", "max_steps": 1 }
                ],
                "max_concurrency": 1
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("tool should execute");

        assert_eq!(outcome.status(), merry_core::ToolCallResultStatus::Succeeded);
        let text = outcome.content().as_text().expect("json content is text");
        assert!(text.contains("\"spawned\""));
        assert!(text.contains("\"agent_id\""));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_subagents_returns_compact_status_and_paths() {
        let manager = test_manager();
        let spawn = manager
            .spawn(
                vec![SubagentTaskSpec::new("Read src/lib.rs.", 1).expect("valid")],
                Some(1),
                CancellationToken::new(),
            )
            .await
            .expect("spawn succeeds");
        let agent_id = spawn.spawned[0].agent_id.clone();
        let executor = WaitSubagentsExecutor::new(manager);
        let call = pending_call(
            WAIT_SUBAGENTS_TOOL_NAME,
            json!({
                "agent_ids": [agent_id.as_str()],
                "mode": "all",
                "timeout_ms": 10
            }),
        );

        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("tool should execute");

        assert_eq!(outcome.status(), merry_core::ToolCallResultStatus::Succeeded);
        let text = outcome.content().as_text().expect("json content is text");
        assert!(text.contains("\"agents\""));
        assert!(text.contains(agent_id.as_str()));
    }
}
```

Add a `test_manager()` helper in the same test module:

```rust
fn test_manager() -> SubagentManager {
    SubagentManager::new(
        SessionId::new("parent").expect("valid id"),
        SubagentConfig::default(),
        Arc::new(manager_tests::FakeChildFactory::new()),
    )
}
```

If `FakeChildFactory` is private to `manager_tests`, move it to the outer test module so both modules can reuse it.

- [ ] **Step 2: Run tests and confirm failure**

Run:

```bash
cargo test -p merry-runtime subagent::tool_tests
```

Expected: FAIL because the executors are not implemented.

- [ ] **Step 3: Implement tool executors**

In `crates/merry-runtime/src/subagent.rs`, add:

```rust
use crate::{ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture};

#[derive(Clone)]
pub struct SpawnSubagentsExecutor {
    manager: SubagentManager,
}

impl SpawnSubagentsExecutor {
    #[must_use]
    pub fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for SpawnSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: merry_core::PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input: SpawnSubagentsInput =
                serde_json::from_value(serde_json::Value::Object(call.arguments().as_object().clone()))
                    .map_err(|error| crate::ToolExecutionError::Infrastructure {
                        message: error.to_string(),
                    })?;
            let tasks = spawn_input_to_specs(input.tasks)
                .map_err(|error| crate::ToolExecutionError::Infrastructure {
                    message: error.to_string(),
                })?;
            let output = self
                .manager
                .spawn(tasks, input.max_concurrency, context.cancellation_token().clone())
                .await
                .map_err(|error| crate::ToolExecutionError::Infrastructure {
                    message: error.to_string(),
                })?;
            let json = serde_json::to_string(&output).expect("spawn output serializes");
            Ok(ToolExecutionOutcome::succeeded_json(json))
        })
    }
}

#[derive(Clone)]
pub struct WaitSubagentsExecutor {
    manager: SubagentManager,
}

impl WaitSubagentsExecutor {
    #[must_use]
    pub fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for WaitSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input: WaitSubagentsInput =
                serde_json::from_value(serde_json::Value::Object(call.arguments().as_object().clone()))
                    .map_err(|error| crate::ToolExecutionError::Infrastructure {
                        message: error.to_string(),
                    })?;
            let timeout = input.timeout_ms.map(Duration::from_millis);
            let output = self
                .manager
                .wait(
                    &input.agent_ids,
                    input.mode.unwrap_or(WaitMode::All),
                    timeout,
                )
                .await
                .map_err(|error| crate::ToolExecutionError::Infrastructure {
                    message: error.to_string(),
                })?;
            let json = serde_json::to_string(&output).expect("wait output serializes");
            Ok(ToolExecutionOutcome::succeeded_json(json))
        })
    }
}

#[derive(Clone)]
pub struct CancelSubagentsExecutor {
    manager: SubagentManager,
}

impl CancelSubagentsExecutor {
    #[must_use]
    pub fn new(manager: SubagentManager) -> Self {
        Self { manager }
    }
}

impl ToolExecutor for CancelSubagentsExecutor {
    fn execute<'a>(
        &'a self,
        call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let input: CancelSubagentsInput =
                serde_json::from_value(serde_json::Value::Object(call.arguments().as_object().clone()))
                    .map_err(|error| crate::ToolExecutionError::Infrastructure {
                        message: error.to_string(),
                    })?;
            let output = self
                .manager
                .cancel(&input.agent_ids)
                .await
                .map_err(|error| crate::ToolExecutionError::Infrastructure {
                    message: error.to_string(),
                })?;
            let json = serde_json::to_string(&output).expect("cancel output serializes");
            Ok(ToolExecutionOutcome::succeeded_json(json))
        })
    }
}
```

Add input conversion helper:

```rust
fn spawn_input_to_specs(
    tasks: Vec<SpawnSubagentTaskInput>,
) -> Result<Vec<SubagentTaskSpec>, SubagentError> {
    tasks
        .into_iter()
        .map(|task| {
            let mut spec = SubagentTaskSpec::new(
                task.task,
                task.max_steps.unwrap_or(DEFAULT_MAX_STEPS),
            )?
            .with_display_name(task.display_name);
            if let Some(tools) = task.allowed_tools {
                spec = spec.with_allowed_tools(tools);
            }
            if let Some(paths) = task.read_scope {
                spec = spec.with_read_scope(paths)?;
            }
            if let Some(paths) = task.write_scope {
                spec = spec.with_write_scope(paths)?;
            }
            Ok(spec)
        })
        .collect()
}
```

- [ ] **Step 4: Add helper to build registered tools**

Add function:

```rust
pub fn subagent_registered_tools(
    manager: SubagentManager,
) -> Result<[crate::RegisteredTool; 3], merry_core::CoreError> {
    let [spawn, wait, cancel] = subagent_tool_specs()?;
    Ok([
        crate::RegisteredTool::read_only(spawn, Arc::new(SpawnSubagentsExecutor::new(manager.clone()))),
        crate::RegisteredTool::read_only(wait, Arc::new(WaitSubagentsExecutor::new(manager.clone()))),
        crate::RegisteredTool::read_only(cancel, Arc::new(CancelSubagentsExecutor::new(manager))),
    ])
}
```

Export it from `crates/merry-runtime/src/lib.rs`.

- [ ] **Step 5: Run tool executor tests**

Run:

```bash
cargo test -p merry-runtime subagent::tool_tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-runtime/src/subagent.rs crates/merry-runtime/src/lib.rs
git commit -m "feat(runtime): add subagent tools"
```

## Task 5: Parent Runtime Integration And Deterministic Parallel Loop Test

**Files:**
- Modify: `crates/merry-runtime/src/subagent.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`
- Modify: `crates/merry-runtime/tests/provider_boundary.rs` if event helper updates are required.

- [ ] **Step 1: Add failing parent-loop integration test**

Append to `crates/merry-runtime/tests/agent_loop.rs`:

```rust
#[derive(Clone)]
struct TestSubagentRuntimeFactory {
    children: Arc<Mutex<Vec<(Arc<dyn ModelProvider>, ModelName)>>>,
}

impl TestSubagentRuntimeFactory {
    fn new(children: Vec<(Arc<dyn ModelProvider>, ModelName)>) -> Self {
        Self {
            children: Arc::new(Mutex::new(children.into_iter().rev().collect())),
        }
    }
}

impl merry_runtime::ChildRuntimeFactory for TestSubagentRuntimeFactory {
    fn build_child(
        &self,
        input: merry_runtime::ChildRuntimeInput,
    ) -> Result<Runtime, RuntimeError> {
        let (provider, model) = self
            .children
            .lock()
            .expect("children mutex should not be poisoned")
            .pop()
            .expect("test child provider should exist");
        let mut builder = Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(provider, model);
        for tool in input.allowed_tools {
            if tool.as_str() == "workspace_read_file" {
                builder = builder.register_tool(workspace_read_file_tool());
            }
        }
        builder.build()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_subagents_starts_parallel_child_runtimes_and_parent_continues() {
    let spawn_call = model_tool_call_with_arguments(
        "call-spawn-subagents",
        "spawn_subagents",
        json!({
            "tasks": [
                {
                    "task": "Read src/lib.rs and summarize the runtime exports.",
                    "max_steps": 2,
                    "allowed_tools": ["workspace_read_file"],
                    "read_scope": ["src/lib.rs"]
                },
                {
                    "task": "Read src/runtime.rs and summarize runtime builder behavior.",
                    "max_steps": 2,
                    "allowed_tools": ["workspace_read_file"],
                    "read_scope": ["src/runtime.rs"]
                }
            ],
            "max_concurrency": 2
        }),
    );
    let wait_call = model_tool_call_with_arguments(
        "call-wait-subagents",
        "wait_subagents",
        json!({
            "agent_ids": ["parent-subagent-child-1", "parent-subagent-child-2"],
            "mode": "all",
            "timeout_ms": 1000
        }),
    );
    let parent_provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(spawn_call))],
        vec![Ok(completed_tool_call_event(wait_call))],
        vec![Ok(completed_text_event("parent used child results"))],
    ]);

    let child_one_provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        "child one summary",
    ))]]);
    let child_two_provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        "child two summary",
    ))]]);

    let factory = TestSubagentRuntimeFactory::new(vec![
        (Arc::new(child_one_provider.clone()), model_name()),
        (Arc::new(child_two_provider.clone()), model_name()),
    ]);
    let manager = merry_runtime::SubagentManager::new(
        session_id("parent-subagent"),
        merry_runtime::SubagentConfig::new(4, 1).expect("valid config"),
        Arc::new(factory),
    );
    let [spawn_tool, wait_tool, cancel_tool] =
        merry_runtime::subagent_registered_tools(manager.clone()).expect("tool specs");
    let runtime = Runtime::builder(session_id("parent-subagent"))
        .model_provider(Arc::new(parent_provider.clone()), model_name())
        .subagent_manager(manager.clone())
        .register_tool(spawn_tool)
        .register_tool(wait_tool)
        .register_tool(cancel_tool)
        .build()
        .expect("runtime should build");

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Delegate investigation.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(8).expect("valid loop config"),
        )
        .await
        .expect("loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );

    let snapshot = manager.snapshot().await;
    assert_eq!(snapshot.len(), 2);
    assert!(
        snapshot
            .iter()
            .all(|agent| matches!(agent.status, merry_runtime::SubagentStatusLabel::Completed))
    );
    let child_one_requests = child_one_provider.recorded_requests();
    let child_two_requests = child_two_provider.recorded_requests();
    assert_eq!(child_one_requests.len(), 1);
    assert_eq!(child_two_requests.len(), 1);
    let child_one_text = child_one_requests[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(child_one_text.contains("Read src/lib.rs and summarize the runtime exports."));
}
```

Also add this local helper near the existing test tool helpers:

```rust
fn workspace_read_file_tool() -> merry_runtime::RegisteredTool {
    merry_runtime::RegisteredTool::read_only(
        tool_spec("workspace_read_file"),
        Arc::new(ScriptedToolExecutor::succeeding_text("file contents")),
    )
}
```

- [ ] **Step 2: Add failing child task-anchor/tool-scope test**

Append to `crates/merry-runtime/tests/agent_loop.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn child_runtime_receives_task_anchor_and_restricted_tools() {
    let spawn_call = model_tool_call_with_arguments(
        "call-spawn-subagents",
        "spawn_subagents",
        json!({
            "tasks": [
                {
                    "task": "Inspect only the runtime library exports.",
                    "max_steps": 2,
                    "allowed_tools": ["workspace_read_file"],
                    "read_scope": ["src/lib.rs"]
                }
            ],
            "max_concurrency": 1
        }),
    );
    let parent_provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(spawn_call))],
        vec![Ok(completed_text_event("parent done"))],
    ]);
    let child_provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        "child summary",
    ))]]);
    let factory = TestSubagentRuntimeFactory::new(vec![(Arc::new(child_provider.clone()), model_name())]);
    let manager = merry_runtime::SubagentManager::new(
        session_id("parent-child-scope"),
        merry_runtime::SubagentConfig::new(2, 1).expect("valid config"),
        Arc::new(factory),
    );
    let [spawn_tool, wait_tool, cancel_tool] =
        merry_runtime::subagent_registered_tools(manager.clone()).expect("tool specs");
    let runtime = Runtime::builder(session_id("parent-child-scope"))
        .model_provider(Arc::new(parent_provider), model_name())
        .subagent_manager(manager)
        .register_tool(spawn_tool)
        .register_tool(wait_tool)
        .register_tool(cancel_tool)
        .build()
        .expect("runtime should build");

    runtime
        .run_agent_loop(
            StepInput::user_text("Delegate scoped child.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .await
        .expect("loop should run");

    let child_requests = child_provider.recorded_requests();
    assert_eq!(child_requests.len(), 1);
    let child_text = child_requests[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(child_text.contains("task-anchor:"));
    assert!(child_text.contains("Inspect only the runtime library exports."));
    assert_eq!(
        child_requests[0]
            .tools()
            .iter()
            .map(|tool| tool.name().as_str())
            .collect::<Vec<_>>(),
        ["workspace_read_file"]
    );
}
```

- [ ] **Step 3: Run the integration tests and confirm failure**

Run:

```bash
cargo test -p merry-runtime spawn_subagents_starts_parallel_child_runtimes_and_parent_continues
cargo test -p merry-runtime child_runtime_receives_task_anchor_and_restricted_tools
```

Expected: FAIL because `ChildRuntimeFactory`, `SubagentManager`, or subagent tool registration integration is missing.

- [ ] **Step 4: Keep the deterministic child runtime factory test-local**

Do not add a public `ScriptedSubagentRuntimeFactory` to runtime. The `TestSubagentRuntimeFactory` above is enough: it proves the runtime boundary without adding product API surface just for tests.

- [ ] **Step 5: Update event-kind helpers for subagent events**

In any test helper matching `RuntimeEventKind`, add:

```rust
RuntimeEventKind::SubagentSpawned { .. } => "SubagentSpawned",
RuntimeEventKind::SubagentStarted { .. } => "SubagentStarted",
RuntimeEventKind::SubagentStatusChanged { .. } => "SubagentStatusChanged",
RuntimeEventKind::SubagentCompleted { .. } => "SubagentCompleted",
RuntimeEventKind::SubagentFailed { .. } => "SubagentFailed",
RuntimeEventKind::SubagentCancelled { .. } => "SubagentCancelled",
```

- [ ] **Step 6: Run the parent-loop integration tests**

Run:

```bash
cargo test -p merry-runtime spawn_subagents_starts_parallel_child_runtimes_and_parent_continues
cargo test -p merry-runtime child_runtime_receives_task_anchor_and_restricted_tools
```

Expected: PASS.

- [ ] **Step 7: Add depth/no-grandchildren test**

Append to `crates/merry-runtime/tests/agent_loop.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn child_depth_one_cannot_spawn_grandchildren() {
    let spawn_call = model_tool_call_with_arguments(
        "call-spawn",
        "spawn_subagents",
        json!({
            "tasks": [{ "task": "Try to spawn a nested child.", "max_steps": 2 }],
            "max_concurrency": 1
        }),
    );
    let parent_provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        spawn_call,
    ))]]);
    let child_provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        "child has no subagent tools",
    ))]]);
    let factory = TestSubagentRuntimeFactory::new(vec![(Arc::new(child_provider.clone()), model_name())]);
    let manager = merry_runtime::SubagentManager::new(
        session_id("parent-depth"),
        merry_runtime::SubagentConfig::new(2, 1).expect("valid config"),
        Arc::new(factory),
    );
    let [spawn_tool, wait_tool, cancel_tool] =
        merry_runtime::subagent_registered_tools(manager.clone()).expect("tool specs");
    let runtime = Runtime::builder(session_id("parent-depth"))
        .model_provider(Arc::new(parent_provider), model_name())
        .subagent_manager(manager)
        .register_tool(spawn_tool)
        .register_tool(wait_tool)
        .register_tool(cancel_tool)
        .build()
        .expect("runtime should build");

    runtime
        .run_agent_loop(
            StepInput::user_text("Spawn child.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .await
        .expect("loop should run");

    let child_requests = child_provider.recorded_requests();
    assert_eq!(child_requests.len(), 1);
    assert!(
        child_requests[0]
            .tools()
            .iter()
            .all(|tool| tool.name().as_str() != "spawn_subagents")
    );
}
```

- [ ] **Step 8: Run depth test**

Run:

```bash
cargo test -p merry-runtime child_depth_one_cannot_spawn_grandchildren
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/merry-runtime/src/subagent.rs crates/merry-runtime/src/runtime.rs crates/merry-runtime/tests/agent_loop.rs crates/merry-runtime/tests/provider_boundary.rs
git commit -m "feat(runtime): run parallel subagent tools"
```

## Task 6: Runtime Events And Ledger Facts For Subagents

**Files:**
- Modify: `crates/merry-runtime/src/ledger.rs`
- Modify: `crates/merry-runtime/src/session.rs`
- Modify: `crates/merry-runtime/src/subagent.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Add failing lifecycle event test**

Append to `crates/merry-runtime/tests/agent_loop.rs`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn subagent_events_do_not_embed_child_transcripts() {
    let child_summary = "child transcript sentinel should not appear in lifecycle event";
    let events = vec![
        RuntimeEvent::new(
            session_id("parent-events"),
            0,
            RuntimeEventKind::SubagentSpawned {
                agent_id: merry_core::SubagentId::new("agent-1").expect("valid id"),
                task_id: merry_core::SubagentTaskId::new("task-1").expect("valid id"),
                task_anchor: "Review file.".to_owned(),
            },
        ),
        RuntimeEvent::new(
            session_id("parent-events"),
            1,
            RuntimeEventKind::SubagentCompleted {
                agent_id: merry_core::SubagentId::new("agent-1").expect("valid id"),
                task_id: merry_core::SubagentTaskId::new("task-1").expect("valid id"),
                summary: "compact summary".to_owned(),
                output_paths: vec!["shared/subagents/agent-1/result.md".to_owned()],
                changed_paths: Vec::new(),
            },
        ),
    ];
    let text = serde_json::to_string(&events).expect("events serialize");
    assert!(!text.contains(child_summary));
    assert!(text.contains("shared/subagents/agent-1/result.md"));
}
```

This test should already pass at core protocol level, but it establishes the no-transcript event contract in runtime tests before wiring runtime emission.

- [ ] **Step 2: Add ledger fact variants**

In `crates/merry-runtime/src/ledger.rs`, add:

```rust
/// A subagent lifecycle event was recorded.
SubagentLifecycle,
```

- [ ] **Step 3: Add session helpers**

In `crates/merry-runtime/src/session.rs`, add helper methods:

```rust
pub(crate) fn record_subagent_spawned(
    &mut self,
    agent_id: merry_core::SubagentId,
    task_id: merry_core::SubagentTaskId,
    task_anchor: String,
) -> RuntimeEvent {
    self.record_event(
        RuntimeEventKind::SubagentSpawned {
            agent_id,
            task_id,
            task_anchor,
        },
        LedgerFactKind::SubagentLifecycle,
    )
}
```

Add equivalent helpers for started/completed/failed/cancelled:

```rust
pub(crate) fn record_subagent_started(
    &mut self,
    agent_id: merry_core::SubagentId,
    task_id: merry_core::SubagentTaskId,
) -> RuntimeEvent {
    self.record_event(
        RuntimeEventKind::SubagentStarted { agent_id, task_id },
        LedgerFactKind::SubagentLifecycle,
    )
}

pub(crate) fn record_subagent_completed(
    &mut self,
    agent_id: merry_core::SubagentId,
    task_id: merry_core::SubagentTaskId,
    summary: String,
    output_paths: Vec<String>,
    changed_paths: Vec<String>,
) -> RuntimeEvent {
    self.record_event(
        RuntimeEventKind::SubagentCompleted {
            agent_id,
            task_id,
            summary,
            output_paths,
            changed_paths,
        },
        LedgerFactKind::SubagentLifecycle,
    )
}

pub(crate) fn record_subagent_status_changed(
    &mut self,
    agent_id: merry_core::SubagentId,
    task_id: merry_core::SubagentTaskId,
    status: String,
) -> RuntimeEvent {
    self.record_event(
        RuntimeEventKind::SubagentStatusChanged {
            agent_id,
            task_id,
            status,
        },
        LedgerFactKind::SubagentLifecycle,
    )
}

pub(crate) fn record_subagent_failed(
    &mut self,
    agent_id: merry_core::SubagentId,
    task_id: merry_core::SubagentTaskId,
    diagnostic: merry_core::ErrorInfo,
) -> RuntimeEvent {
    self.record_event(
        RuntimeEventKind::SubagentFailed {
            agent_id,
            task_id,
            diagnostic,
        },
        LedgerFactKind::SubagentLifecycle,
    )
}

pub(crate) fn record_subagent_cancelled(
    &mut self,
    agent_id: merry_core::SubagentId,
    task_id: merry_core::SubagentTaskId,
    diagnostic: merry_core::ErrorInfo,
) -> RuntimeEvent {
    self.record_event(
        RuntimeEventKind::SubagentCancelled {
            agent_id,
            task_id,
            diagnostic,
        },
        LedgerFactKind::SubagentLifecycle,
    )
}
```

- [ ] **Step 4: Wire event emission from subagent manager**

Do not let tool executors mutate the parent session directly. Keep all parent event recording in `Runtime`, at the same boundary that already records tool artifacts and tool resolutions.

In `crates/merry-runtime/src/subagent.rs`, add an internal lifecycle queue to the manager:

```rust
#[derive(Debug, Clone)]
pub(crate) enum SubagentLifecycleEvent {
    Spawned {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        task_anchor: String,
    },
    Started {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
    },
    StatusChanged {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        status: String,
    },
    Completed {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        summary: String,
        output_paths: Vec<String>,
        changed_paths: Vec<String>,
    },
    Failed {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        diagnostic: ErrorInfo,
    },
    Cancelled {
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        diagnostic: ErrorInfo,
    },
}

#[derive(Debug, Default)]
struct SubagentManagerState {
    agents: BTreeMap<SubagentId, ManagedSubagent>,
    lifecycle_events: Vec<SubagentLifecycleEvent>,
}

impl SubagentManager {
    pub(crate) async fn drain_lifecycle_events(&self) -> Vec<SubagentLifecycleEvent> {
        let mut state = self.state.lock().await;
        std::mem::take(&mut state.lifecycle_events)
    }
}
```

Push lifecycle events only after the corresponding manager state mutation:

```rust
state.lifecycle_events.push(SubagentLifecycleEvent::Spawned {
            agent_id: agent_id.clone(),
            task_id: task_id.clone(),
            task_anchor: task_anchor.objective().to_owned(),
        });
```

For child terminal events, update `ManagedSubagent` first, then push `Completed`, `Failed`, or `Cancelled` from the compact `SubagentStatusView`. Do not include child transcript text; use the compact summary and shared output paths already visible in `wait_subagents`.

- [ ] **Step 5: Drain lifecycle events in runtime tool execution**

In `crates/merry-runtime/src/runtime.rs`, after a generic tool executor returns and before `submit_tool_execution_outcome(...)`, drain only for subagent tool names:

```rust
let mut subagent_events = Vec::new();
if matches!(
    pending.name().as_str(),
    "spawn_subagents" | "wait_subagents" | "cancel_subagents"
) {
    if let Some(manager) = &self.inner.subagent_manager {
        subagent_events = manager.drain_lifecycle_events().await;
    }
}
let (status, content, diagnostic, execution_evidence) = outcome.into_parts();
let mut session = self.inner.session.lock().await;
let mut events = record_subagent_lifecycle_events(&mut session, subagent_events);
events.extend(session.submit_tool_execution_outcome(
    call_id,
    status,
    content,
    diagnostic,
    execution_evidence,
)?);
Ok(events)
```

Add a small private helper near the runtime execution path:

```rust
fn record_subagent_lifecycle_events(
    session: &mut SessionState,
    lifecycle_events: Vec<crate::SubagentLifecycleEvent>,
) -> Vec<RuntimeEvent> {
    lifecycle_events
        .into_iter()
        .map(|event| match event {
            crate::SubagentLifecycleEvent::Spawned {
                agent_id,
                task_id,
                task_anchor,
            } => session.record_subagent_spawned(agent_id, task_id, task_anchor),
            crate::SubagentLifecycleEvent::Started { agent_id, task_id } => {
                session.record_subagent_started(agent_id, task_id)
            }
            crate::SubagentLifecycleEvent::StatusChanged {
                agent_id,
                task_id,
                status,
            } => session.record_subagent_status_changed(agent_id, task_id, status),
            crate::SubagentLifecycleEvent::Completed {
                agent_id,
                task_id,
                summary,
                output_paths,
                changed_paths,
            } => session.record_subagent_completed(
                agent_id,
                task_id,
                summary,
                output_paths,
                changed_paths,
            ),
            crate::SubagentLifecycleEvent::Failed {
                agent_id,
                task_id,
                diagnostic,
            } => session.record_subagent_failed(agent_id, task_id, diagnostic),
            crate::SubagentLifecycleEvent::Cancelled {
                agent_id,
                task_id,
                diagnostic,
            } => session.record_subagent_cancelled(agent_id, task_id, diagnostic),
        })
        .collect()
}
```

This preserves the runtime rule: manager state changes first, ledger lifecycle facts are recorded before events become observable, and tool result continuation is still recorded through the normal session path.

- [ ] **Step 6: Update the parent-loop expected event sequence**

After lifecycle events are wired, update `spawn_subagents_starts_parallel_child_runtimes_and_parent_continues` to include subagent lifecycle events before the corresponding `ToolCallResolved` entries. The exact order should be deterministic for spawn/start; terminal events may arrive before `wait_subagents` resolves because child tasks finish asynchronously. Assert relative ordering instead of a brittle full-list equality:

```rust
let names = event_kind_names(result.events());
assert!(names.contains(&"SubagentSpawned"));
assert!(names.contains(&"SubagentStarted"));
assert!(names.contains(&"SubagentCompleted"));
let first_tool_resolved = names
    .iter()
    .position(|name| *name == "ToolCallResolved")
    .expect("spawn tool should resolve");
let first_subagent_spawned = names
    .iter()
    .position(|name| *name == "SubagentSpawned")
    .expect("spawn event should exist");
assert!(
    first_subagent_spawned < first_tool_resolved,
    "subagent spawn lifecycle should be visible before spawn tool resolution"
);
```

- [ ] **Step 7: Run lifecycle tests**

Run:

```bash
cargo test -p merry-runtime subagent_events_do_not_embed_child_transcripts
cargo test -p merry-runtime spawn_subagents_starts_parallel_child_runtimes_and_parent_continues
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/merry-runtime/src/ledger.rs crates/merry-runtime/src/session.rs crates/merry-runtime/src/subagent.rs crates/merry-runtime/tests/agent_loop.rs
git commit -m "feat(runtime): record subagent lifecycle events"
```

## Task 7: Final Verification And Spec Sync

**Files:**
- Modify: `specs/2026-05-31-parallel-subagent-tool-runtime.md` only if implementation intentionally diverges.
- Modify: `plans/2026-05-31-parallel-subagent-tool-runtime.md` only to mark completed checkboxes if executing inline.

- [ ] **Step 1: Run focused tests**

Run:

```bash
cargo test -p merry-core subagent
cargo test -p merry-runtime subagent
cargo test -p merry-runtime spawn_subagents_starts_parallel_child_runtimes_and_parent_continues
cargo test -p merry-runtime child_runtime_receives_task_anchor_and_restricted_tools
cargo test -p merry-runtime wait_subagents_returns_compact_status_and_paths
cargo test -p merry-runtime conflicting_child_write_scopes_are_rejected_before_spawn
cargo test -p merry-runtime child_depth_one_cannot_spawn_grandchildren
cargo test -p merry-runtime subagent_events_do_not_embed_child_transcripts
```

Expected: all PASS.

- [ ] **Step 2: Run full verification**

Run:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: all PASS.

- [ ] **Step 3: Review scope against spec**

Confirm these statements are true in the implementation:

```text
Subagents are exposed to the parent model as tools.
The first implementation supports max_concurrency=2 parallel child starts.
Child runtimes receive task anchors.
Child runtimes do not receive subagent tools at depth 1.
wait_subagents returns compact status and shared output paths.
No child transcript or raw artifact payload is embedded in parent-visible lifecycle events.
Overlapping write scopes are rejected before child spawn.
```

- [ ] **Step 4: Commit final sync if needed**

If any spec or plan status update is needed:

```bash
git add specs/2026-05-31-parallel-subagent-tool-runtime.md plans/2026-05-31-parallel-subagent-tool-runtime.md
git commit -m "docs(plan): sync subagent runtime implementation"
```

If no docs changed, do not create an empty commit.
