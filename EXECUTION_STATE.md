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

- Third implementation slice: record exact shell-wrapper input as a
  pre-execution runtime artifact before runner execution.

Goal:

- Prove that an admitted read-only shell-wrapper process records exact
  command/script input before runner execution, keeps result artifacts linked
  to that input artifact, and preserves payload-free traces plus compact ledger
  observations.

Task queue status:

- Added session-owned `process-input-*` runtime artifact IDs and reserved them
  from external `record_artifact` / `submit_tool_result` callers.
- Shell-wrapper execution now records a JSON input artifact before runner
  execution. It contains shell, flag, script text, script byte count, stable
  `fnv1a64` fingerprint, tool call id/name, permission profile, and intent
  summary/cwd.
- Shell-wrapper result artifacts now reference the input artifact via
  `input_artifact` and no longer duplicate exact script text under
  `input_evidence`.
- Shell process compact ledger observations include the result artifact and
  input artifact ids plus payload-free shell/profile/status/output/fingerprint
  metadata, but not raw script text.
- Added deterministic success, runner-cancel, and runner-failure tests proving
  input artifact durability, evidence-ref readability, unresolved pending calls
  on no-output paths, no result artifact before output, and no action audit on
  runner cancel/failure.
- Updated `ROADMAP.md` and `DECISIONS.md` to record that the pre-execution shell
  input artifact boundary is now implemented.

Allowed expansion:

- Focused runtime process/shell artifact, trace, ledger, cancellation, and
  evidence plumbing and tests.
- Public-safe roadmap/decision/continuity updates.

Done condition:

- Admitted shell-wrapper execution records exact script input in a
  pre-execution input artifact before the runner is called.
- Shell-wrapper result artifacts reference the pre-execution input artifact and
  do not duplicate raw script payload.
- Runner cancellation or infrastructure failure after input recording keeps the
  pending call unresolved, records no output/result artifact or action audit,
  and leaves the input artifact/evidence readable.
- Shell-wrapper traces and compact ledger observations remain free of raw
  `argv` or script text.
- Default validation passes and the lease is committed.

Drift boundary:

- Do not add a broad model-facing shell tool in this slice.
- Do not implement a full shell parser or make the classifier the authorization
  model for complex shell syntax.
- Do not make existing `process.read_only.v1` runner injection imply shell
  execution capability.
- Do not add approval/session grants, long-running process sessions, stdin/env
  shell behavior, or real shell runner adapters in this slice.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs`.
- Do not make live provider behavior part of default tests.

Task type: code/docs

Acceptance criteria:

- Shell-wrapper process execution records exact `input_evidence` in
  `process-input-*` before runner execution.
- Shell-wrapper result artifacts reference the input artifact and omit raw
  script payload.
- Cancellation and runner-failure tests prove input artifact durability without
  output/result artifact or action audit.
- Trace and ledger tests prove shell-wrapper metadata remains payload-free and
  references artifacts/fingerprints instead of raw script text.
- `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  and `cargo test --all` pass.

## Scope

Allowed edits:

- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/src/session.rs`
- `crates/merry-runtime/tests/runtime_flow.rs`
- `crates/merry-runtime/tests/provider_boundary.rs`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content
- broad shell command tool or real shell runner implementation
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

- Continue M2 by defining the first reusable real shell runner/profile boundary
  on top of the now-proven artifact ordering. Keep raw shell execution behind
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
