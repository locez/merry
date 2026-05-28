# Handoff

Status: complete

## Current Work

Current milestone or track:

- M2 Shell-Compatible Runtime Boundary.

Session milestone:

- Second implementation slice: preserve exact shell-wrapper script evidence in
  artifacts while keeping traces and compact ledger observations payload-free.

Task queue status:

- Added shell input evidence helpers for shell-wrapper process intents.
- Shell-wrapper result artifacts now include exact `input_evidence`: shell,
  flag, script text, script byte count, and stable `fnv1a64` fingerprint.
- Shell-wrapper result artifacts omit duplicate `intent.argv`, so exact shell
  script text appears once in the provider-visible tool result payload.
- Shell-wrapper process start/finish traces omit raw `argv` and script text;
  they record shell, flag, script byte count, script fingerprint, status, output
  byte counts, and other bounded metadata.
- Shell-wrapper compact ledger observations omit raw script text and record the
  shell profile, shell, flag, byte count, fingerprint, output byte counts, and
  result artifact reference.
- Added deterministic tests proving artifact exactness and trace/ledger payload
  omission for `bash -lc "rg ProcessRunner | wc -l"`.
- Updated `ROADMAP.md` and `DECISIONS.md` with this M2 evidence/metadata slice.

Done condition:

- The M2 shell-compatible boundary now preserves exact shell input in artifact
  content and keeps shell execution traces plus compact ledger observations free
  of raw script payloads. It still does not introduce a model-facing shell tool,
  broad shell parser, standalone pre-execution input artifact, or real shell
  runner.

## What Changed

Files changed:

- `crates/merry-runtime/src/process.rs`
- `crates/merry-runtime/src/runtime.rs`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Summary:

- Added shell-wrapper input evidence metadata and stable fingerprints.
- Kept exact shell script text in the process result artifact.
- Avoided duplicating shell script text inside the same result artifact.
- Removed raw shell argv/script text from shell process execution traces and
  compact ledger observations.
- Recorded that this is still the existing result artifact path; a standalone
  pre-execution command artifact remains the next M2 decision.

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
- Shell-wrapper compact ledger observations use shell/profile/status/output
  metadata, script byte count, stable fingerprint, and artifact reference.
- The current implementation uses the process result artifact for exact shell
  input evidence; it does not yet create a standalone command-start artifact.

Pending decisions:

- Whether shell command/script input should be recorded as a standalone
  pre-execution artifact before runner execution.
- Whether the first real shell runner uses the existing CLI process runner
  adapter or a runtime-owned shell runner wrapper.
- Approval/session semantics for shell commands beyond the read-only wrapper
  lane.

## Blockers

Blockers:

- None.

Residual risk:

- If a future shell runner is cancelled or fails before output, the current
  result-artifact-only input evidence path cannot prove command-start evidence.
  That is why the next slice should decide the standalone pre-execution input
  artifact boundary.
- No real shell runner was added in this lease; tests use fake runners.

Next exact action:

- Continue M2 by deciding and implementing the pre-execution shell input
  artifact boundary, then add cancellation tests for execution cancelled or
  failed before output. Keep real shell runner/admission profile work separate
  until artifact ordering is proven.

## Scope For Next Session

Allowed edits:

- Runtime shell/process artifact and trace boundary modules.
- Focused tests for pre-execution command/script input evidence, output
  artifacts, cancellation, and ledger reduction.
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

- feat(runtime): add shell input evidence metadata

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
