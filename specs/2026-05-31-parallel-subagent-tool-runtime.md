# Parallel Subagent Tool Runtime

## Purpose

Merry should support subagents as runtime-owned tools, not as a separate chat
participant system. The first useful version must support parallel delegated
work, because a serial "delegate" call is just an expensive wrapper around the
existing agent loop.

The MVP goal is:

```text
The main agent can spawn multiple bounded child agents, track their status, and
read their compact results through normal tool-result flow while all large
cross-agent data moves through a shared filesystem workspace.
```

## External Reference

Codex's current subagent documentation describes an experimental
`spawn_agents_on_csv` workflow where the runtime reads a CSV, spawns one worker
per row, waits for the batch, and exports combined results. The useful design
signals for Merry are:

- subagents are exposed through tools
- fan-out is bounded by `max_concurrency`
- runtime has global guards such as `agents.max_threads` and `agents.max_depth`
- each worker must report a structured result once
- exported result metadata includes item identity, status, error, and result

Merry should not copy the CSV-specific interface. Merry's main agent should
spawn explicit task specs directly.

Source: <https://developers.openai.com/codex/subagents#process-csv-batches-with-subagents-experimental>

## Non-Goals

- Do not build a separate long-lived chat surface for child agents.
- Do not implement grandchildren in v1. Maximum depth is 1.
- Do not implement artifact pass-through between parent and child sessions.
- Do not implement intelligent merge.
- Do not let child agents write outside their assigned write scopes.
- Do not require a TUI to manage child agents.
- Do not require CSV as the task input format.

## Core Model

Subagent support enters the main agent as tools:

```text
spawn_subagents(tasks[], max_concurrency?)
wait_subagents(agent_ids[], mode?)
cancel_subagents(agent_ids[])
```

`send_subagent_input(agent_id, input)` is reserved for later interactive
workflows. The v1 shape should not depend on it.

Each spawned child is a bounded runtime-owned worker:

```text
SubagentTask:
  task_id
  display_name?
  task
  max_steps
  allowed_tools[]
  read_scope[]
  write_scope[]
  forbidden_paths[]
  expected_output
```

The child runtime receives:

- its own `SessionId`
- a `TaskAnchor` equal to the assigned task
- selected provider/model config
- a restricted tool registry
- shared workspace/scratch roots matching the task scopes
- no subagent tools in v1

## Shared Filesystem Data Plane

Cross-agent large data should use a shared filesystem workspace, not runtime
artifact handoff.

Example layout:

```text
.merry/local/sessions/<parent_session>/shared/
  subagents/
    <agent_id>/
      result.md
      findings.json
      scratch/
```

The parent and all children can receive the same shared directory as a readable
workspace root. Child write access is scoped to its own subdirectory plus any
explicit write scopes granted by the main agent.

`wait_subagents` returns only compact metadata:

```text
SubagentStatus:
  agent_id
  task_id
  status
  summary
  output_paths[]
  changed_paths[]
  diagnostics?
```

If the main agent needs detail, it reads `output_paths` through normal
workspace tools. This keeps large child outputs out of parent prompt text and
avoids defining cross-session artifact references before there is a real need.

Runtime artifacts remain per-session evidence/result storage. They are not the
inter-agent IPC layer in this milestone.

## Parallelism And Guards

The implementation must support parallel children in the first version.

Required controls:

- `max_concurrency` per `spawn_subagents` call
- runtime/global `max_threads`
- runtime/global `max_depth`
- per-child `max_steps`
- per-child timeout or cancellation token

Depth rule:

```text
parent depth 0 may spawn child depth 1
child depth 1 cannot receive subagent tools in v1
```

Thread rule:

```text
open child runtimes count against max_threads until completed, failed,
cancelled, timed out, or explicitly closed.
```

## Scope Ownership

The main agent owns child interaction policy. It decides what each child may
read, write, and report.

For coding-agent workflows, the main agent should allocate non-overlapping
write scopes before spawning parallel children. Runtime should reject a
`spawn_subagents` request when two children declare overlapping write scopes,
unless both are read-only.

Write-scope checks are intentionally path-level and conservative. If the main
agent cannot provide clear write ownership, it should use read-only children or
spawn fewer workers.

## Tool Semantics

### `spawn_subagents`

Creates child runtimes and starts each child loop up to `max_concurrency`.

Inputs:

```text
tasks[]:
  task
  display_name?
  max_steps?
  allowed_tools?
  read_scope?
  write_scope?
  forbidden_paths?
  expected_output?
max_concurrency?
```

Output:

```text
spawned:
  agent_id
  task_id
  display_name?
  status: queued | running
  task_anchor
  read_scope
  write_scope
rejected:
  task_index
  reason
```

The tool result should be small and provider-visible. It should not include
child transcripts, raw artifacts, or large output files.

### `wait_subagents`

Collects child status and final compact results.

Inputs:

```text
agent_ids[]
mode: any | all
timeout_ms?
```

Output:

```text
agents[]:
  agent_id
  task_id
  status
  summary
  output_paths[]
  changed_paths[]
  diagnostics?
```

`wait_subagents` is an information-management tool for the main agent. It is
not just a blocking sleep. The main agent should be able to poll, inspect
finished workers, and decide whether to cancel or continue.

### `cancel_subagents`

Requests cooperative cancellation for selected children and returns their
latest known statuses.

## Runtime Events

Subagent events should be provider-neutral runtime events so CLI, TUI, and
Python bindings can observe them without parsing logs:

```text
SubagentSpawned
SubagentStarted
SubagentStatusChanged
SubagentCompleted
SubagentFailed
SubagentCancelled
```

These events describe lifecycle and metadata only. They should not embed child
transcripts or large child outputs.

## Result Contract

Each child must produce exactly one compact result record before normal
completion.

Recommended result fields:

```text
summary
output_paths[]
changed_paths[]
open_questions[]
verification[]
```

For v1, the child may produce this result by final answer convention and runtime
post-processing. A later version may expose a dedicated `report_subagent_result`
tool to make the contract stricter.

## First Acceptance Slice

The first implementation slice should be deterministic and offline:

1. Fake provider asks the parent runtime to call `spawn_subagents` with two
   read-only tasks.
2. Runtime creates two child runtimes with separate `TaskAnchor` values.
3. The child runtimes run concurrently under `max_concurrency = 2`.
4. The children do not receive subagent tools because depth is already 1.
5. `wait_subagents(mode=all)` returns both compact results and output paths.
6. Parent receives the wait output as a normal tool result and can continue.
7. A conflicting write-scope spawn request is rejected before any child starts.
8. Runtime events expose the subagent lifecycle without large payloads.

Suggested deterministic tests:

```text
spawn_subagents_starts_parallel_child_runtimes
child_runtime_receives_task_anchor_and_restricted_tools
wait_subagents_returns_compact_status_and_paths
conflicting_child_write_scopes_are_rejected_before_spawn
child_depth_one_cannot_spawn_grandchildren
subagent_events_do_not_embed_child_transcripts
```

## Open Questions

- Whether v1 should require a dedicated `report_subagent_result` tool or accept
  final answer post-processing first.
- Whether child provider/model config should default to the parent primary
  model or allow per-task role selection in the first implementation.
- Whether completed child runtimes are retained for inspection or closed after
  result collection.
- Whether shared scratch should live under `.merry/local` only, or allow an
  injected tempfs/scratch root from SDK callers.
