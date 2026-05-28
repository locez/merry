# Handoff

Status: complete

## Current Work

Current milestone or track:

- Roadmap drift correction back to Minimal Useful Coding Loop.

Session milestone:

- User-requested planning correction: audit ROADMAP history, identify where
  profile/session work displaced the coding-loop MVP, restore the next step to
  a testable coding capability, and document roadmap change control.

Task queue status:

- Audited `ROADMAP.md` history. The drift point is `e945ebc`: the correction
  away from a subset shell parser was right, but it moved `Next Active` toward
  shell profile/session work. Follow-up M2 commits then made supporting profile
  work look primary.
- Designed the next task as a configurable disposable coding-loop smoke: a
  user-selectable or user-supplied tiny fixture task where the model must
  inspect, read exact evidence, patch through `workspace_patch_file`, run
  verification, and answer.
- Updated `ROADMAP.md` to restore the next active track to coding-loop
  capability and to record the drift audit.
- Added roadmap change-control rules to `AGENTS.md` and `PROJECT_LEAD.md`.
- Recorded the correction in `DECISIONS.md`.

Done condition:

- The active roadmap is corrected back to a runnable/testable coding-loop
  capability. Agents are now explicitly prohibited from unilaterally changing
  roadmap priority or promoting profile/policy/classifier work into `Next
  Active` without user approval or a tracked change request.

## What Changed

Files changed:

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Summary:

- Identified where ROADMAP drifted from coding-loop proof into shell/profile
  design.
- Restored `Next Active` to a configurable disposable coding-loop smoke.
- Wrote roadmap change-control guardrails into the repository contract.

## Validation

Commands run:

- `cargo fmt --all --check`
- `git diff --check`

Result:

- Passed.

## Decisions

Decisions made:

- The active next step is a configurable disposable coding-loop smoke, not
  shell profile/session design.
- ROADMAP priority changes require explicit user approval or a tracked change
  request.

Pending decisions:

- Exact CLI flags and fixture-task schema for the configurable coding-loop
  smoke.

## Blockers

Blockers:

- None.

Residual risk:

- This lease is documentation/planning correction only. It does not yet add the
  configurable coding-loop command.

Next exact action:

- Implement the configurable disposable coding-loop smoke. Start with a
  non-default command or ignored integration test that creates/selects a tiny
  Rust fixture task, gives the model a natural task description without exact
  patch text, and validates inspect/read/patch/verify/final-answer through
  runtime events and artifacts.

## Scope For Next Session

Allowed edits:

- `crates/merry-cli/src/main.rs` and `crates/merry-cli/tests/debug.rs` for the
  first configurable smoke.
- Existing runtime/tool crates only if the coding-loop acceptance command
  exposes a concrete blocker.
- Public-safe roadmap/decision/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content.
- Roadmap priority changes without explicit user approval.
- Broad model-facing shell tool or approval/session implementation.
- A Merry-owned subset shell parser as the authorization model.
- Full-screen TUI, REPL, or multi-turn UI.

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

Status: committed

Message:

- docs: restore roadmap focus to coding loop

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
