# Session Runbook

## Startup

Read in order:

1. `SESSION_RUNBOOK.md`
2. `AGENT_ROLES.md`
3. `EXECUTION_STATE.md`
4. `HANDOFF.md`
5. Source-of-truth files listed in `EXECUTION_STATE.md`

Use the communication language recorded in `EXECUTION_STATE.md`. If the current user request clearly uses another language, the current request wins and state must be updated before the lease ends.

If files disagree, follow this priority:

1. `SESSION_RUNBOOK.md`
2. `EXECUTION_STATE.md`
3. Source-of-truth files
4. `HANDOFF.md`
5. Chat context

## Lease

Execute one bounded lease. A lease advances one session milestone, then writes `HANDOFF.md`, commits or records a no-commit reason, reports status, and stops.

Continue inside the same lease only while the next task advances the same session milestone, uses the same validation plan, and stays inside the allowed edits and drift boundary.

## Research Triggers

Use a Researcher role only when acceptance criteria are unclear, repo evidence conflicts, the task affects public API/data/security/provider behavior, or the session milestone depends on unknown external behavior.

Research must name the implementation decision or acceptance test it unblocks.

## Change Request Triggers

Append `CHANGE_REQUESTS.md` and stop when a material roadmap/backlog/source-of-truth change is needed without explicit user approval.

The May 23, 2026 roadmap recalibration was explicitly requested by the user and may be applied directly in this lease.

## Review Triggers

Run a reviewer pass before ending when code behavior changed, validation failed or was skipped, protected files were touched, or a non-trivial decision was recorded.

## Completion Requirements

Before ending:

- update `EXECUTION_STATE.md`
- write `HANDOFF.md`
- record non-trivial decisions in `DECISIONS.md`
- run validation or document why it was not run
- create a git commit unless a no-commit reason is recorded

The next session command is always:

```text
/goal $project-continuity
```
