# Subagent Scope Enforcement And Module Split

Date: 2026-06-07

## Purpose

The original parallel subagent spec already says child agents must not write
outside assigned write scopes. The current implementation only partially
realizes that contract:

- `allowed_tools` is applied by the CLI child runtime factory.
- `write_scope` is used to reject overlapping parallel writes before spawn.
- `read_scope`, `write_scope`, and `forbidden_paths` are stored and reported,
  but are not yet materialized into child workspace tool/profile boundaries.

This spec defines the next implementation slice:

```text
Subagent task scopes become real child runtime boundaries for writes and
explicit forbids, while read_scope remains advisory in the first version.
```

This work should also split `merry-runtime/src/subagent.rs` into focused
modules so enforcement does not land in another large all-in-one file.

## Current Evidence

`SubagentTaskSpec` contains:

```text
task
display_name?
max_model_turns
allowed_tools[]
read_scope[]
write_scope[]
forbidden_paths[]
expected_output?
```

Current use:

- `allowed_tools` is copied into `ChildRuntimeInput`.
- `write_scope` participates in `validate_no_write_scope_conflicts`.
- `read_scope` and `write_scope` are returned in `SpawnedSubagentView`.
- `forbidden_paths` is validated and preserved.

Current gap:

- `ChildRuntimeInput` exposes `task` and `allowed_tools`, but the CLI
  `CodingLoopChildRuntimeFactory` only reads `allowed_tools`,
  `session_id`, and `task_anchor`.
- The child workspace profile is built from parent workspace roots and skill
  roots, not from the task's `write_scope` or `forbidden_paths`.
- A child may therefore be scheduled as if it owns only `crates/foo`, while
  its workspace tools still have the parent workspace capability surface.

## Design

### Tool Subset

`allowed_tools` remains a subagent task field.

Rules:

- Empty `allowed_tools` means use the default child tool set selected by the
  parent runtime/factory.
- Non-empty `allowed_tools` can only reduce the child tool surface.
- A child must not receive a tool that the parent runtime/profile would not
  otherwise make available.

First implementation:

- Keep the existing CLI behavior that maps `allowed_tools` to patch/process
  tool enablement.
- Add focused tests that prove a child cannot enable patch/process tools by
  naming tools outside the parent factory's supported set.

### Read Scope

`read_scope` remains advisory in the first enforcement slice.

Reason:

- Read restrictions are easy to make too narrow for code tasks.
- Child agents often need surrounding definitions, callers, tests, and config.
- The main safety risk in parallel delegation is write collision and unwanted
  mutation, not read access inside the already configured workspace roots.

First implementation:

- Keep validating and reporting `read_scope`.
- Render it as task guidance / status metadata if needed.
- Do not reject read tool calls outside `read_scope` yet.

Later option:

```text
strict_read_scope: true
```

or a high-security profile can turn `read_scope` into a hard read boundary.

### Write Scope

`write_scope` becomes a hard boundary for child write-capable workspace tools.

Rules:

- A child with no `write_scope` is read-only for workspace mutation unless the
  parent factory explicitly assigns a default scratch/output write root.
- A child with `write_scope` may only write paths under those relative
  workspace paths.
- Existing overlap checks continue to reject conflicting sibling write scopes
  before any child starts.

First implementation:

- Convert `SubagentTaskSpec::write_scope()` into child workspace path rules.
- Apply those rules before registering `workspace_patch` for the child.
- Add deterministic tests proving a child patch outside `write_scope` is
  rejected before mutation.

### Forbidden Paths

`forbidden_paths` becomes a hard deny boundary.

Rules:

- A forbidden path denies both reads and writes for child workspace tools when
  the workspace tool layer supports it.
- For the first narrow slice, at minimum it must deny writes.
- `forbidden_paths` must win over `write_scope` if the two overlap.

First implementation:

- Apply forbidden path checks to child workspace mutation.
- Keep hidden path rules separate; existing hidden path behavior still applies.
- Add deterministic tests proving forbidden paths are rejected even when they
  are also under an allowed write scope.

## Implementation Plan

### Step 1: Split Runtime Subagent Modules

Target structure:

```text
crates/merry-runtime/src/subagent/
  mod.rs
  spec.rs
  tools.rs
  manager.rs
  child_loop.rs
```

Responsibilities:

- `spec.rs`: `SubagentConfig`, `SubagentTaskSpec`, scope validation, overlap
  detection.
- `tools.rs`: provider-visible spawn/wait/cancel schemas and tool executors.
- `manager.rs`: `SubagentManager`, registry, queueing, cancellation, status
  snapshots.
- `child_loop.rs`: `ChildRuntimeInput`, `ChildRuntimeFactory`, child session id,
  loop launch, result projection.
- `mod.rs`: re-exports only.

Scope guard:

- Do not change behavior during this split.
- Keep tests passing after each split.

### Step 2: Make Child Scope Explicit

Introduce an internal child scope shape so factory implementors cannot miss the
contract by only looking at `allowed_tools`.

Candidate shape:

```rust
pub struct ChildWorkspaceScope {
    pub read_scope: Vec<PathBuf>,
    pub write_scope: Vec<PathBuf>,
    pub forbidden_paths: Vec<PathBuf>,
}
```

Then:

```rust
pub struct ChildRuntimeInput {
    pub session_id: SessionId,
    pub task_anchor: TaskAnchor,
    pub task: SubagentTaskSpec,
    pub allowed_tools: Vec<ToolName>,
    pub workspace_scope: ChildWorkspaceScope,
    pub depth: u8,
}
```

First version semantics:

- `workspace_scope.read_scope` is advisory.
- `workspace_scope.write_scope` and `workspace_scope.forbidden_paths` must be
  enforced by child runtime construction when write-capable workspace tools are
  enabled.

### Step 3: Connect CLI Child Runtime Factory

Update `CodingLoopChildRuntimeFactory` so child workspace profile construction
uses the child task scope.

Expected behavior:

- Child profile still starts from parent workspace root and skill roots.
- Read/list/search tools can keep parent-readable workspace roots.
- Patch/write capability is constrained to child `write_scope`.
- Forbidden paths deny mutation even when nested under allowed write scope.
- Process tooling must not bypass child write scope for workspace mutation. If
  process cannot be scoped precisely yet, child process write/effect capability
  must remain conservative.

Open implementation question:

- Whether the enforcement belongs in `merry-tool-workspace` path validation,
  `WorkspaceToolsConfig`, or runtime profile path rules.

Preferred first slice:

- Add workspace-tool path rules to `WorkspaceToolsConfig` because
  `workspace_patch` already owns workspace-relative mutation validation.

### Step 4: Tests

Required deterministic tests:

```text
subagent_child_input_carries_workspace_scope
subagent_write_scope_rejects_overlapping_siblings_before_spawn
child_workspace_patch_rejects_path_outside_write_scope
child_workspace_patch_rejects_forbidden_path_inside_write_scope
child_without_write_scope_does_not_receive_workspace_patch_by_default
read_scope_is_advisory_and_does_not_block_read_tools
allowed_tools_cannot_expand_child_tool_surface
```

Test placement:

- Runtime spec/manager tests live near `subagent/spec.rs` and
  `subagent/manager.rs`.
- CLI child factory scope tests live in `merry-cli` because that is where the
  coding-agent child runtime is materialized.
- Workspace path-rule tests live in `merry-tool-workspace` if enforcement is
  implemented there.

## Implementation Status

Completed in the first enforcement slice:

- `merry-runtime` now carries an explicit `ChildWorkspaceScope` in
  `ChildRuntimeInput`.
- Spawned child factory inputs are populated from `SubagentTaskSpec` read,
  write, and forbidden path scopes.
- `merry-tool-workspace` supports optional `workspace_patch` write scope and
  forbidden path boundaries.
- `workspace_patch` rejects writes outside configured scope before mutation.
- Forbidden patch paths override allowed write scopes.
- `merry-cli` child runtimes pass child scopes into workspace tool config.
- `merry-cli` child runtimes with explicit workspace boundaries do not receive
  the permissioned process lane, so process execution cannot bypass the first
  write-scope boundary.

Still pending:

- Strict read-scope enforcement remains intentionally out of scope.
- Process write/effect scoping can be revisited when process permission grants
  can be constrained to child workspace rules precisely.
- The broad module split stopped at `spec.rs`, `protocol.rs`, and `tools.rs`;
  `manager.rs` and `child_loop.rs` remain future cleanup.
- A dedicated CLI child factory unit test can be added later if the factory
  gains injectable inspection points; current verification compiles the CLI
  factory and tests the actual workspace patch boundary in
  `merry-tool-workspace`.

## Non-Goals

- Do not implement strict read-scope enforcement in the first slice.
- Do not add grandchildren support.
- Do not add interactive parent-child messaging.
- Do not introduce a second permission/profile system for subagents.
- Do not let a subagent task request elevate beyond the parent runtime profile.
- Do not solve merge/conflict resolution beyond conservative write-scope
  overlap rejection.

## Acceptance

The slice is complete when:

- `subagent.rs` is split into focused modules with no file over the repository
  size guideline unless tests remain temporarily concentrated.
- Child runtime construction receives an explicit workspace scope.
- `write_scope` and `forbidden_paths` affect real child write capability.
- `read_scope` is documented and tested as advisory.
- Existing subagent spawn/wait/cancel behavior remains unchanged except for
  newly enforced invalid writes.
- The following pass:

```bash
cargo fmt --all --check
cargo test -p merry-runtime
cargo test -p merry-cli
cargo test -p merry-tool-workspace
cargo clippy -p merry-runtime --all-targets --all-features -- -D warnings
cargo clippy -p merry-cli --all-targets --all-features -- -D warnings
cargo clippy -p merry-tool-workspace --all-targets --all-features -- -D warnings
```
