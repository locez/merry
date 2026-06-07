# Workspace Tool Crate Split

Date: 2026-06-07

## Purpose

`merry-tool-workspace` already owns the right product boundary: workspace file
tools and coding-loop profile composition live outside `merry-runtime`. The
problem is internal structure. `src/lib.rs` has grown into a 6000-line
implementation file containing public config, runtime profile wiring, path
validation, read/list/search tools, patch parsing/planning/execution, tracing,
guidance envelopes, and unit tests.

This cleanup makes the crate publishable for 0.1.0 testing without changing
behavior.

## Non-Goals

- Do not change public tool names.
- Do not change tool schemas.
- Do not change output envelope shapes.
- Do not change path-safety behavior or the current residual TOCTOU tradeoff.
- Do not add new runtime, CLI, SDK, or TUI features in this cleanup.

## Target Structure

```text
crates/merry-tool-workspace/src/
  lib.rs
  config.rs
  errors.rs
  profile.rs
  state.rs
  trace.rs
  schema.rs
  path.rs
  read.rs
  list.rs
  search.rs
  patch/
    mod.rs
    apply.rs
    parse.rs
    plan.rs
    types.rs
  tests/
    mod.rs
    config.rs
    path.rs
    read_list_search.rs
    patch.rs
    trace.rs
```

Module ownership:

- `lib.rs` is the public crate entrance: module declarations, public
  re-exports, and stable tool-name constants.
- `config.rs` owns `WorkspaceToolLimits`, `WorkspaceToolsConfig`, and config
  validation errors.
- `profile.rs` owns `WorkspaceCodingLoopProfile` and runtime-builder
  integration.
- `state.rs` owns canonical workspace roots, allowed scope checks, hidden path
  policy, and workspace summary helpers.
- `schema.rs` owns tool argument structs, tool specs, and argument parsing.
- `path.rs` owns relative path validation, symlink-safe open helpers, and
  path-oriented diagnostics.
- `errors.rs` owns shared diagnostic codes, guidance text, and tool-result
  failure envelopes.
- `trace.rs` owns workspace trace payloads, bounded-text helpers, and test trace
  hooks.
- `read.rs`, `list.rs`, and `search.rs` own their corresponding executors and
  blocking implementations.
- `patch/` owns patch parsing, planning, and application as a local subsystem.
- `tests/` keeps large unit tests out of production modules while preserving
  the current behavior coverage.

## Plan

1. Add this spec as the acceptance contract for the refactor.
2. Extract public config/profile/state/error/path/schema/trace modules while
   preserving current public re-exports from `lib.rs`.
3. Extract read/list/search executors without changing tool specs or result
   formatting.
4. Extract patch parser/planner/apply code into a `patch` subsystem without
   changing patch behavior.
5. Move unit tests out of `lib.rs` and split them by behavior area.
6. Run formatting and targeted verification.

## Acceptance Checks

- Existing downstream imports continue to work for:
  - `WorkspaceToolLimits`
  - `WorkspaceToolsConfig`
  - `WorkspaceToolConfigError`
  - `WorkspaceCodingLoopProfile`
  - `WorkspaceCodingLoopProfileError`
  - `WorkspaceRuntimeProfileBuilderExt`
  - `ReadOnlyWorkspaceTools`
  - workspace tool-name constants
- `crates/merry-tool-workspace/src/lib.rs` only contains crate entry wiring and
  public re-exports.
- Production implementation files stay below the repository's soft 1000-line
  limit where practical.
- Unit tests are not kept in the production `lib.rs`.
- `cargo fmt --all --check`
- `cargo test -p merry-tool-workspace`
- If time allows: `cargo clippy -p merry-tool-workspace --all-targets --all-features -- -D warnings`
