# Handoff

Status: rollover

## Current Work

Current milestone or track:

- Event-first interactive CLI design.

Session milestone:

- Use the renewed roadmap/product concern to decide the next useful milestone
  after proving live tool calls, compare event-first CLI vs runtime profile vs
  TUI research, and write the selected spec without starting implementation.

Task queue status:

- Current project state and ignored source-material guidance reviewed.
- Runtime event/artifact/agent-loop surfaces inspected.
- Locez Lens used to reframe the gap as real-use observability, not UI polish.
- Brainstorming compared three options: event-first CLI, runtime-owned profile,
  and TUI research.
- User selected option 1: event-first interactive CLI.
- Spec written at `specs/2026-05-23-event-first-interactive-cli.md`.
- Roadmap updated to make event-first CLI the next user-facing proof gap.
- Decision recorded: Event-first CLI before TUI.
- `.gitignore` updated to keep local `.superpowers/` visual companion metadata
  out of commits.
- Validation: passed for planning/spec changes.
- Commit: completed.

Done condition:

- The selected next milestone is recorded in a public-safe spec and roadmap,
  and the next implementation step is blocked on user review/approval of the
  spec per the brainstorming workflow.

## What Changed

Files changed:

- `.gitignore`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`
- `specs/2026-05-23-event-first-interactive-cli.md`

Summary:

- Added a concrete event-first interactive CLI design: command shape,
  human-readable timeline, exact `--events-jsonl`, runtime event mapping,
  artifact summaries, fail-closed gates, deterministic tests, and TUI follow-up.
- Re-anchored the roadmap so the next milestone is human-visible runtime
  interaction, while keeping the coding-loop harness as the acceptance skeleton.
- Recorded that TUI is viable but deferred until line-oriented usage shows the
  views and interactions worth building.

## Validation

Commands run so far:

- spec self-review with `rg` for placeholders/private-material references
- `git diff --check`

Result:

- Passed. The self-review found no unresolved placeholders in the new spec and
  no copied private source content. Matches in roadmap/state are expected
  guardrail references to ignored docs, raw notes, and local credentials.

Reviewer pass:

- Scope alignment: aligned with the user-selected option 1 and the
  project-continuity/brainstorming constraint to write and review a spec before
  implementation.
- Protected file check: `ROADMAP.md` was intentionally updated under the user's
  explicit roadmap-correction request; no private ignored material was moved
  into tracked files.
- State and handoff check: continuity state and handoff both point to the same
  next exact action.
- Recommendation: rollover for user spec review.
- Residual risk: no code behavior changed in this lease, so Rust checks were
  not run.

## Decisions

Decisions made:

- Choose option 1 from the brainstorm: event-first interactive CLI.
- Keep TUI as a researched follow-up, not the next implementation milestone.
- Keep implementation out of this lease until the user reviews/approves the
  spec and a written implementation plan exists.
- Store the spec under tracked `specs/` rather than ignored `docs/`, because
  repository rules keep `docs/` private and uncommitted.

Pending decisions:

- User review of `specs/2026-05-23-event-first-interactive-cli.md`.
- Whether the first implementation should reuse the live smoke assembly
  directly or extract common setup before adding the command. The spec
  recommends reuse first, extraction after the shape proves useful.

## Blockers

Blockers:

- None for design. Implementation intentionally waits for user review/approval
  of the spec.

Next exact action:

- User reviews `specs/2026-05-23-event-first-interactive-cli.md`. If approved,
  the next lease should use `superpowers:writing-plans` to create a scoped
  implementation plan for the Event-first interactive CLI.

## Scope For Next Session

Allowed edits:

- Implementation plan for the event-first CLI after user approval.
- Rust/CLI/test files named by that implementation plan.
- Continuity file updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content.
- Full-screen TUI before event-first CLI usage evidence.
- Policy taxonomy as the primary deliverable unless it directly blocks the
  interactive CLI acceptance test.

Do not reconsider:

- The next user-facing proof gap is event-first visibility into the runtime
  loop, not more invisible architecture.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- project-continuity: design event first cli

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
