# Handoff

Status: complete

## Current Work

Current milestone or track:

- M2 Shell-Compatible Runtime Boundary.

Session milestone:

- Third implementation slice: record exact shell-wrapper input as a
  pre-execution runtime artifact before runner execution.

Task queue status:

- Added session-owned `process-input-*` runtime artifact IDs and reserved them
  from external artifact/result submission APIs.
- Shell-wrapper process execution now records a `process-input-*` JSON artifact
  before calling the process runner. It contains exact shell, flag, script text,
  script byte count, stable `fnv1a64` fingerprint, tool call id/name,
  permission profile, and intent summary/cwd.
- Shell-wrapper result artifacts reference the input artifact via
  `input_artifact` and no longer duplicate exact script text under
  `input_evidence`.
- Shell-wrapper process start/finish traces omit raw `argv` and script text;
  they record shell, flag, script byte count, script fingerprint, status, output
  byte counts, and other bounded metadata.
- Shell-wrapper compact ledger observations omit raw script text and record the
  shell profile, shell, flag, byte count, fingerprint, output byte counts,
  result artifact reference, and input artifact reference.
- Added deterministic success, runner-cancel, and runner-failure tests proving
  input artifact durability before output, evidence-ref readability, unresolved
  pending calls on no-output paths, no result artifact before output, and no
  action audit on runner cancel/failure.
- Updated `ROADMAP.md` and `DECISIONS.md` with this M2 pre-execution input
  artifact slice.

Done condition:

- The M2 shell-compatible boundary now preserves exact shell input in a
  pre-execution runtime artifact and keeps shell execution traces plus compact
  ledger observations free of raw script payloads. It still does not introduce a
  model-facing shell tool, broad shell parser, approval/session semantics, or a
  reusable real shell runner.

## What Changed

Files changed:

- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/src/session.rs`
- `crates/merry-runtime/tests/provider_boundary.rs`
- `crates/merry-runtime/tests/runtime_flow.rs`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Summary:

- Added pre-execution shell-wrapper input artifacts with exact script evidence
  and stable fingerprints.
- Removed raw script duplication from shell-wrapper process result artifacts;
  results now reference `input_artifact`.
- Removed raw shell argv/script text from shell process execution traces and
  compact ledger observations.
- Preserved input evidence on runner cancellation/failure while keeping the
  pending call unresolved and avoiding result/audit writes.

## Validation

Commands run:

- `cargo fmt --all --check`
- `cargo test -p merry-runtime read_only_shell_process --lib`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`

Result:

- Passed.
- `cargo test --all` did not run ignored live/bwrap smokes; those remain
  explicit opt-in lanes.

## Decisions

Decisions made:

- Exact shell-wrapper script text is artifact payload, not trace payload.
- Exact shell-wrapper script text belongs in a pre-execution `process-input-*`
  artifact, not the process result artifact.
- Shell-wrapper compact ledger observations use shell/profile/status/output
  metadata, script byte count, stable fingerprint, result artifact reference,
  and input artifact reference.

Pending decisions:

- Whether the first real shell runner uses the existing CLI process runner
  adapter or a runtime-owned shell runner wrapper.
- Approval/session semantics for shell commands beyond the read-only wrapper
  lane.

## Blockers

Blockers:

- None.

Residual risk:

- The error return path still does not carry partial event vectors. If a runner
  cancels/fails after input recording, callers must inspect runtime state to
  discover the already-recorded input artifact.
- No real shell runner was added in this lease; tests use fake runners.

Next exact action:

- Continue M2 by defining the first reusable real shell runner/profile boundary
  on top of the proven input/output artifact ordering.

## Scope For Next Session

Allowed edits:

- Runtime shell/process artifact and trace boundary modules.
- Focused tests for real shell runner/profile admission, output artifacts,
  cancellation, and ledger reduction.
- Public-safe roadmap/decision/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content.
- Broad model-facing shell tool before shell artifacts/traces are defined.
- A Merry-owned subset shell parser as the authorization model.
- Approval/session implementation unless explicitly chosen as the next slice.
- Full-screen TUI, REPL, or multi-turn UI.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Shell compatibility must use real shell execution under explicit profiles;
  do not revive parser-first M2.
- `process.shell.read_only.v1` must stay distinct from `process.read_only.v1`.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- feat(runtime): record shell input before runner

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
