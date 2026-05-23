# Handoff

Status: rollover

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding loop.

Session milestone:

- Convert the approved observability spec into a tracked implementation plan
  and prepare the next execution handoff.

Task queue status:

- User approved `specs/2026-05-23-observability-first-coding-loop.md`.
- Implementation plan created at
  `plans/2026-05-23-config-backed-observability.md`.
- Decision record updated to explain why sandboxed live provider credentials
  should support config-relative `api_key_file` in addition to `api_key_env`.
- Continuity state updated for implementation-plan-ready status.
- Validation: passed for planning changes.
- Commit: pending.

Done condition:

- Plan is tracked, public-safe, concrete enough for subagent or inline
  execution, and the next exact action is execution-mode selection.

## What Changed

Files changed:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `plans/2026-05-23-config-backed-observability.md`

Summary:

- Added an implementation plan for XDG TOML config, config-backed log setup,
  sandbox config/log mounting, XDG provider config, runtime/process tracing,
  workspace/provider tracing, and end-to-end deterministic log smoke coverage.
- Kept the milestone focused on observability, not event CLI, REPL, or TUI.
- Recorded the sandbox credential handling decision: prefer `api_key_env`, and
  allow config-relative `api_key_file` so the sandbox does not need API keys in
  bwrap argv.

## Validation

Commands run so far:

- repo inspection for current CLI/runtime/provider/tool structure
- plan placeholder scan
- root logging flag direction scan
- private-material/reference scan
- XDG/TOML/tracing coverage scan
- `git diff --check`
- whitespace check for the new plan file

Result:

- Passed. The logging flag scan found only explicit non-goal/checklist
  references. Private-material matches are guardrails, ignored path references,
  and fake test secret strings, not copied private docs or real credentials.

## Decisions

Decisions made:

- Save the implementation plan under tracked `plans/` instead of ignored
  `docs/superpowers/plans/`.
- Use config-relative `api_key_file` as a practical sandboxed live-smoke
  credential source, while keeping secrets out of logs and runtime state.

Pending decisions:

- User must choose execution mode for the plan:
  1. Subagent-Driven (recommended)
  2. Inline Execution

## Blockers

Blockers:

- None for planning. Implementation has not started.

Next exact action:

- Present execution choices for
  `plans/2026-05-23-config-backed-observability.md`.

## Scope For Next Session

Allowed edits:

- Files named by the implementation plan.
- Continuity file updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content.
- Full-screen TUI, REPL, or multi-turn UI before observability exists.
- Event-first CLI as the primary milestone.

Do not reconsider:

- The next proof gap is logs/traces that explain real runtime behavior while
  the loop runs.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: pending

Message:

- project-continuity: plan config-backed observability

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
