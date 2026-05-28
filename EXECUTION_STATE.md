# Execution State

Lease status: complete

## Source Of Truth

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `ROADMAP.md`
- `README.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `plans/2026-05-23-config-backed-observability.md`
- `examples/config.toml`
- `docs/design/mvp-design.md` (ignored private local design source)
- `docs/design/global-design.md` (ignored private local design source)
- `docs/product/product-strategy.md` (ignored private local product source)
- `merry-raw-docs/` (ignored original local source material; do not commit)

## Planning Maturity

Level: implementation-in-progress

Current planning artifact:

- `ROADMAP.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `plans/2026-05-23-config-backed-observability.md`
- `examples/config.toml`

## Communication

Language: Chinese

Style notes:

- Keep user-facing updates concise and direct.
- Preserve Rust/API/config names in English.

## Current Work

Current milestone or track:

- M2 Shell-Compatible Runtime Boundary.

Session milestone:

- Fourth implementation slice: make the first real shell runner adapter
  runtime-owned and prove it against the shell input artifact boundary.

Goal:

- Prove that the read-only shell-wrapper profile can execute through a reusable
  runtime-owned Tokio process runner while preserving pre-execution input
  artifacts, result artifact references, and payload-free metadata.

Task queue status:

- Added `merry-runtime::TokioProcessRunner`, a runtime-owned
  `tokio::process::Command` adapter for the existing `ProcessRunner` trait.
- The runner clears inherited environment, closes stdin, captures bounded
  UTF-8 stdout/stderr, maps process status to `ProcessExitStatus`, and handles
  cooperative cancellation by killing the child process.
- Removed the duplicate CLI-private `TokioProcessRunner`; CLI shell/debug
  paths now reuse the runtime-owned adapter.
- Added a runtime provider-boundary test that uses real `bash -lc "echo
  ProcessRunner | wc -l"` through `allow_read_only_shell_process_actions`,
  proves `process-input-*` is recorded first, and proves the result references
  `input_artifact` without duplicating raw script text.
- Updated README, `ROADMAP.md`, and `DECISIONS.md` to record that the reusable
  real runner adapter exists while authorization remains explicit and narrow.

Allowed expansion:

- Focused runtime process/shell artifact, trace, ledger, cancellation, and
  evidence plumbing and tests.
- Public-safe roadmap/decision/continuity updates.

Done condition:

- Runtime exports a reusable Tokio-backed process runner adapter.
- CLI no longer owns a duplicate real process runner implementation.
- A real read-only shell wrapper pipeline executes through runtime policy and
  preserves the `process-input-*` / result `input_artifact` ordering.
- The slice does not broaden shell authorization, add approval/session
  semantics, or introduce a model-facing shell tool.
- Default validation passes and the lease is committed.

Drift boundary:

- Do not add a broad model-facing shell tool in this slice.
- Do not implement a full shell parser or make the classifier the authorization
  model for complex shell syntax.
- Do not make existing `process.read_only.v1` runner injection imply shell
  execution capability.
- Do not add approval/session grants, long-running process sessions, stdin/env
  shell behavior, or broad shell admission in this slice.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs`.
- Do not make live provider behavior part of default tests.

Task type: code/docs

Acceptance criteria:

- `TokioProcessRunner` is runtime-owned and exported.
- CLI shell/debug paths reuse runtime `TokioProcessRunner`.
- Runtime test proves a real shell wrapper pipeline succeeds under explicit
  `process.shell.read_only.v1` opt-in and preserves input/result artifact
  references without raw script duplication.
- `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  and `cargo test --all` pass.

## Scope

Allowed edits:

- `Cargo.toml`
- `README.md`
- `crates/merry-runtime/src/lib.rs`
- `crates/merry-runtime/src/process_runner.rs`
- `crates/merry-cli/src/main.rs`
- `crates/merry-runtime/tests/provider_boundary.rs`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content
- broad model-facing shell command tool
- general shell session/approval implementation beyond the reusable process
  runner adapter
- approval/session implementation
- full-screen TUI, REPL, or multi-turn UI scope

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`

Validation notes:

- All validation commands passed in this lease.
- Focused checks also passed:
  `cargo test -p merry-runtime --test provider_boundary tokio_process_runner_executes_read_only_shell_wrapper_with_input_artifact`
  and `cargo test -p merry-cli shell_`.
- Ignored/live/bwrap smoke tests remain opt-in and were not run by
  `cargo test --all`.

## Research

Research required: no

Research reason:

- This slice follows the already-recorded M2 direction from local Codex source
  inspection; no new research was required.

Research artifact:

- None.

## Next Action

Next exact action:

- Continue M2 by defining the next shell permission/session boundary for
  broader shell syntax and approval semantics. Keep raw shell execution behind
  explicit runtime construction and permission/session policy; do not expand
  the narrow classifier into a general shell authorization model.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Do not make a Merry-owned subset shell parser the authorization model.
- Do not merge `process.shell.read_only.v1` into `process.read_only.v1`.
- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before reusable runtime/process/tool profiles are
  clearer.
- Do not move private Codex/raw-doc findings into tracked source text.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
