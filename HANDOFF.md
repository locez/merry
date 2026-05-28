# Handoff

Status: complete

## Current Work

Current milestone or track:

- M2 Shell-Compatible Runtime Boundary.

Session milestone:

- Fourth implementation slice: make the first real shell runner adapter
  runtime-owned and prove it against the shell input artifact boundary.

Task queue status:

- Added `merry-runtime::TokioProcessRunner`, a runtime-owned
  `tokio::process::Command` adapter for the existing `ProcessRunner` trait.
- The runner clears inherited environment, closes stdin, captures bounded
  UTF-8 stdout/stderr, maps process status to `ProcessExitStatus`, and handles
  cooperative cancellation by killing the child process.
- Removed the duplicate CLI-private `TokioProcessRunner`; CLI shell/debug
  paths now reuse the runtime-owned adapter.
- Added a runtime provider-boundary test using real `bash -lc "echo
  ProcessRunner | wc -l"` under explicit `process.shell.read_only.v1` opt-in.
  The test proves `process-input-*` is recorded before the result artifact and
  the result references `input_artifact` without duplicating raw script text.
- Updated README, `ROADMAP.md`, and `DECISIONS.md` with this reusable real
  runner adapter slice.

Done condition:

- The M2 shell-compatible boundary now has a reusable runtime-owned real process
  runner adapter and preserves the pre-execution shell input artifact ordering
  when that adapter executes a real read-only shell wrapper. It still does not
  introduce a model-facing shell tool, broad shell parser, approval/session
  semantics, or broad shell admission.

## What Changed

Files changed:

- `Cargo.toml`
- `README.md`
- `crates/merry-cli/src/main.rs`
- `crates/merry-runtime/src/lib.rs`
- `crates/merry-runtime/src/process_runner.rs`
- `crates/merry-runtime/tests/provider_boundary.rs`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Summary:

- Moved the real Tokio process runner adapter into runtime and exported it.
- Made CLI shell/debug paths reuse the runtime adapter.
- Proved a real read-only shell wrapper pipeline still records
  `process-input-*` first and result `input_artifact` second.

## Validation

Commands run:

- `cargo fmt --all --check`
- `cargo test -p merry-runtime --test provider_boundary tokio_process_runner_executes_read_only_shell_wrapper_with_input_artifact`
- `cargo test -p merry-cli shell_`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`

Result:

- Passed.
- `cargo test --all` did not run ignored live/bwrap smokes; those remain
  explicit opt-in lanes.

## Decisions

Decisions made:

- `TokioProcessRunner` belongs to runtime, not CLI.
- Runner availability is not authorization; permission/profile opt-in still
  controls whether a process or shell action executes.

Pending decisions:

- Approval/session semantics for shell commands beyond the read-only wrapper
  lane.
- How broader shell syntax is admitted without turning the read-only classifier
  into a general authorization model.

## Blockers

Blockers:

- None.

Residual risk:

- `TokioProcessRunner` is not a sandbox and does not enforce filesystem/network
  policy by itself. Sandbox/profile admission must remain outside the adapter.
- The real shell test depends on host `bash` and `wc`; it skips when either is
  unavailable.

Next exact action:

- Continue M2 by defining the next shell permission/session boundary for
  broader shell syntax and approval semantics.

## Scope For Next Session

Allowed edits:

- Runtime shell/process artifact and trace boundary modules.
- Focused tests for shell permission/session admission, output artifacts,
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

- feat(runtime): add tokio process runner adapter

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
