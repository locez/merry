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

- First implementation slice: add a separate read-only shell-wrapper admission
  lane modeled after Codex's narrow plain pipeline classifier, without turning
  shell parsing into the authorization model.

Goal:

- Prove that a plain read-only shell pipeline can be recognized and routed to a
  shell-specific permission profile only when a dedicated shell runner lane is
  explicitly opted in, while complex or mutating shell forms remain denied
  without runner calls.

Task queue status:

- Added `process.shell.read_only.v1` as a distinct runtime-owned process
  permission profile.
- Added a narrow plain shell-wrapper classifier for `bash`/`sh`/`zsh -c|-lc`
  command text joined by `|`, `&&`, `||`, or `;`, requiring every segment to
  match the direct read-only process classifier.
- Added `RuntimeBuilder::allow_read_only_shell_process_actions` so shell wrapper
  execution cannot be admitted by the existing structured read-only argv runner.
- Added action-policy and runtime tests proving shell read-only proposals are
  denied without shell opt-in, execute with the shell profile when opted in, and
  reject redirects, command substitution, and mutating pipeline segments without
  runner calls.
- Updated `ROADMAP.md` and `DECISIONS.md` to record this M2 slice and its
  guardrails.

Allowed expansion:

- Focused runtime process/shell classifier, admission, audit/artifact/profile
  plumbing and tests.
- Public-safe roadmap/decision/continuity updates.

Done condition:

- `bash -lc "rg ProcessRunner | wc -l"` derives
  `process.shell.read_only.v1`.
- The same proposal is denied when only structured low-risk process actions are
  enabled.
- The proposal executes only when `allow_read_only_shell_process_actions` is
  configured and records the shell profile in artifact/audit evidence.
- Complex or mutating shell forms are denied without runner calls.
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

- Read-only shell-wrapper classification and profile derivation are covered by
  deterministic unit tests.
- Runtime admission tests keep structured read-only argv, read-only shell
  wrapper, and local workspace bwrap lanes distinct.
- Process result artifacts/audit evidence include
  `process.shell.read_only.v1` for admitted shell-wrapper execution.
- `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  and `cargo test --all` pass.

## Scope

Allowed edits:

- `crates/merry-runtime/src/process.rs`
- `crates/merry-runtime/src/action_policy.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/src/lib.rs`
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

Research required: yes

Research reason:

- The user explicitly asked to compare Codex's shell pipeline/read-only
  handling before implementing this direction.

Research artifact:

- Local source inspection of `.merry/codex` only. No internet research was used.
- Key finding: Codex runs real shell commands but uses a narrow plain-command
  classifier for `bash -lc`/pipeline evidence; every segment must be known safe,
  and sandbox/approval/policy remain the real execution boundary.

## Next Action

Next exact action:

- Continue M2 by defining shell execution input/output artifacts and payload-free
  trace metadata for the future shell runner: exact command/script artifact,
  script byte/hash metadata, stdout/stderr/status artifacts, compact ledger
  reduction, and cancellation behavior. Keep the real runner/admission profile
  separate from broad approval/session work.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Do not make a Merry-owned subset shell parser the authorization model.
- Do not merge `process.shell.read_only.v1` into `process.read_only.v1`.
- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before reusable runtime/process/tool profiles are
  clearer.
- Do not move private Codex/raw-doc findings into tracked source text.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
