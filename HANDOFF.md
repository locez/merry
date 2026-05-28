# Handoff

Status: complete

## Current Work

Current milestone or track:

- Minimal Useful Coding Loop.

Session milestone:

- Implement the first configurable disposable coding-loop task smoke with
  deterministic coverage and an opt-in real bwrap validation path.

Task queue status:

- Added `merry --with-sandbox debug coding-loop-task-smoke --task status-text`.
- Added `merry --with-sandbox debug coding-loop-task-live-smoke --task status-text`.
- Added deterministic fake-provider/fake-runner coverage for inspect ->
  failing verification -> read -> patch -> successful verification -> final.
- Added ignored real-bwrap task smoke tests for scripted and live paths.
- Fixed bwrap `/etc` planning to use file/directory helper semantics:
  create mount parents, then direct `--ro-bind`; no broad `/etc` bind, no
  staged copy fallback, and no `LD_LIBRARY_PATH`.
- Updated README/ROADMAP status and verification commands.

Done condition:

- The task smoke is now runnable/testable and advances the active coding-loop
  MVP capability rather than profile-only work.

## What Changed

Files changed:

- `README.md`
- `ROADMAP.md`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Summary:

- New CLI debug task smoke creates a disposable fixture and proves runtime
  read/patch/process verification flow.
- Sandbox plan now mirrors the user's no-copy helper semantics for `/etc`
  file/dir mounts.
- The real bwrap task smoke was confirmed by the user from an outer
  environment.

## Validation

Commands run:

- `cargo fmt --all --check`
- `cargo test -p merry-cli coding_loop_task`
- `cargo test -p merry-cli sandbox_plan_mounts_runtime_paths_and_workspace`
- `git diff --check`

User-run validation:

- `cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`

Result:

- Focused deterministic checks passed locally.
- User-run outer real-bwrap task smoke passed.

## Decisions

Decisions made:

- Keep `/etc` mounting as explicit file/dir helper binds; do not mount all of
  `/etc`.
- Do not add `LD_LIBRARY_PATH`.
- Do not stage-copy `/etc/ld.so.cache` or `/etc/resolv.conf`; the outer
  environment passes with direct bind semantics.
- Keep the current task verification on `rg done` so no process-admission
  broadening is needed.

Pending decisions:

- Whether the next slice should test the live model solving the task, or first
  make the deterministic fake-provider path less scripted by withholding exact
  patch text from the provider script.

## Blockers

Blockers:

- None.

Residual risk:

- The deterministic scripted provider still supplies exact patch arguments.
  This proves the runtime/tool/sandbox path, but not yet real coding
  intelligence.
- The live task smoke is opt-in and depends on local OpenAI-compatible config.

Next exact action:

- Exercise `debug coding-loop-task-live-smoke` with a real model, or add a
  stricter deterministic/live acceptance that proves the model infers the
  patch from file evidence instead of receiving exact `old_text`/`new_text`.

## Scope For Next Session

Allowed edits:

- `crates/merry-cli/src/main.rs` and `crates/merry-cli/tests/debug.rs` for
  task smoke tightening.
- Existing runtime/tool crates only if the coding-loop acceptance command
  exposes a concrete blocker.
- Public-safe roadmap/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content.
- Roadmap priority changes without explicit user approval.
- Broad model-facing shell tool or approval/session implementation.
- A Merry-owned subset shell parser as the authorization model.
- Full-screen TUI, REPL, or multi-turn UI.
- Broad `/etc` bind, `LD_LIBRARY_PATH`, or staged-copy sandbox fallback unless
  the user explicitly approves it.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Shell compatibility must use real shell execution under explicit profiles;
  do not revive parser-first M2.
- `process.shell.read_only.v1` must stay distinct from `process.read_only.v1`.
- Profile/session design is not the next active milestone unless it blocks the
  coding-loop smoke and the user explicitly approves the priority change.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: pending

Message:

- feat(cli): add configurable coding loop task smoke

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
