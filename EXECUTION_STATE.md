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

- Second implementation slice: keep exact shell-wrapper script evidence in
  artifacts while making process execution traces and compact ledger
  observations payload-free.

Goal:

- Prove that an admitted read-only shell-wrapper process preserves exact
  command/script input in runtime-owned artifact content, while traces and
  compact ledger observations carry only shell/profile/status/output metadata,
  script byte counts, fingerprints, and artifact references.

Task queue status:

- Added runtime-local shell input evidence helpers that derive shell, flag,
  script byte count, and stable `fnv1a64` fingerprint from the already-validated
  shell-wrapper process intent.
- Extended process result artifact JSON for shell-wrapper executions with
  exact `input_evidence` containing shell, flag, script text, script byte count,
  and script fingerprint; shell artifacts omit duplicate `intent.argv` so the
  exact script appears once in the provider-visible result payload.
- Split process execution trace helpers so shell-wrapper start/finish traces do
  not log raw `argv` or script text; they log shell, flag, script byte count,
  script fingerprint, cwd, output limits, status, and output byte counts.
- Updated shell process compact ledger observations so they omit raw script
  text and include permission profile, shell, flag, script byte count,
  fingerprint, output byte counts, and the result artifact reference.
- Added deterministic runtime tests proving artifact exactness and trace/ledger
  payload omission for `bash -lc "rg ProcessRunner | wc -l"`.
- Updated `ROADMAP.md` and `DECISIONS.md` to record this M2 evidence/metadata
  slice and its remaining pre-execution artifact gap.

Allowed expansion:

- Focused runtime process/shell artifact, trace, ledger, cancellation, and
  evidence plumbing and tests.
- Public-safe roadmap/decision/continuity updates.

Done condition:

- Admitted shell-wrapper result artifacts include exact script input evidence
  and stable script fingerprint metadata without duplicating the script in
  `intent.argv`.
- Shell-wrapper process start/finish traces do not include raw `argv` or script
  text.
- Shell-wrapper compact ledger observations do not include raw script text and
  include script byte/fingerprint metadata plus result artifact reference.
- Default validation passes and the lease is committed.

Drift boundary:

- Do not add a broad model-facing shell tool in this slice.
- Do not implement a full shell parser or make the classifier the authorization
  model for complex shell syntax.
- Do not make existing `process.read_only.v1` runner injection imply shell
  execution capability.
- Do not add approval/session grants, long-running process sessions, stdin/env
  shell behavior, or real shell runner adapters in this slice.
- Do not claim this slice added a standalone pre-execution shell input artifact.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs`.
- Do not make live provider behavior part of default tests.

Task type: code/docs

Acceptance criteria:

- Shell-wrapper process result artifacts include exact `input_evidence` for the
  script and its stable fingerprint, while omitting duplicate `intent.argv`.
- Trace tests prove shell-wrapper execution logs metadata without raw argv or
  script text.
- Ledger projection tests prove compact observations include artifact
  references and fingerprint metadata without raw script text.
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

Research required: no

Research reason:

- This slice follows the already-recorded M2 direction from local Codex source
  inspection; no new research was required.

Research artifact:

- None.

## Next Action

Next exact action:

- Continue M2 by deciding and implementing the pre-execution shell input
  artifact boundary: record exact command/script input before runner execution,
  preserve payload-free trace/ledger metadata, and add cancellation tests for
  the path where execution is cancelled or fails before an output artifact is
  produced. Keep real shell runner/admission profile work separate until that
  artifact ordering is proven.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Do not make a Merry-owned subset shell parser the authorization model.
- Do not merge `process.shell.read_only.v1` into `process.read_only.v1`.
- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before reusable runtime/process/tool profiles are
  clearer.
- Do not move private Codex/raw-doc findings into tracked source text.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
