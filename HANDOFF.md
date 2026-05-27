# Handoff

Status: complete

## Current Work

Current milestone or track:

- M2 Shell-Compatible Runtime Boundary.

Session milestone:

- Direction correction slice: shell compatibility must not depend on a
  Merry-owned subset shell parser or argv allowlist.

Task queue status:

- Corrected `ROADMAP.md`: M2 is now a shell-compatible runtime boundary, not a
  command-string parser over structured argv.
- Corrected `DECISIONS.md`: static classifiers are hard-deny/advisory evidence,
  not broad shell authorization.
- Corrected `EXECUTION_STATE.md`: next action is to define profile/session
  admission and artifact-backed shell input/output behavior.

Done condition:

- Public-safe roadmap/decision/continuity files now block parser-first drift.

## What Changed

Files changed:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Summary:

- Replaced parser-first M2 wording with a real shell boundary:
  shell syntax belongs to a real shell runner under explicit permission/session
  profiles and sandbox constraints.
- Preserved structured argv as the narrow typed lane for known process intents.
- Recorded that pipelines/control flow are legitimate shell mechanisms and
  should not be forced into separate model tool calls just to fit an argv
  allowlist.
- Stated that static classifiers may hard-deny or advise, but must not be the
  authority that allows complex shell syntax.

## Validation

Commands run:

- `git diff --check`

Result:

- Passed.
- This was docs-only; no Rust behavior changed.

## Decisions

Decisions made:

- Do not build a subset shell parser as the shell authorization model.
- Structured argv and shell-compatible execution are sibling runtime lanes.
- Shell-compatible execution should be admitted by profiles, sandbox/session
  constraints, artifacts, audit, cancellation, reducers, and approvals.

Pending decisions:

- Exact first shell permission/session profile surface.
- Exact shell input artifact schema and trace metadata.
- Whether the first shell-compatible runner uses the existing CLI `bwrap`
  handoff or a narrower runtime-owned sandbox adapter.
- Whether to add an explicit stable-prefix change reason event or metadata in
  the next cache-observability slice.

## Blockers

Blockers:

- None.

Residual risk:

- M2 still needs a concrete vertical slice. The corrected direction avoids the
  parser trap, but the first shell boundary must stay small and testable.
- The CLI `bwrap` profile remains an opt-in smoke boundary, not a complete
  sandbox proof.

Next exact action:

- Start M2 by defining and testing the first shell-compatible runtime boundary:
  artifact-backed command/script input, explicit permission/session admission,
  runner cancellation, output artifacts, compact ledger reduction, and
  payload-free traces. Do not start by splitting shell strings into argv
  allowlists.

## Scope For Next Session

Allowed edits:

- Runtime process/shell boundary modules and focused tests.
- Runtime/tool specs needed for a shell-compatible model-facing command tool
  only after the runtime boundary is clear.
- Existing deterministic agent-loop/coding-loop tests that consume the new tool.
- Public-safe roadmap/decision/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content.
- Merry-owned subset shell parser as the authorization model.
- Approval sessions, long-running process sessions, or broad shell execution
  before the first profile/session boundary is proven.
- Full-screen TUI, REPL, or multi-turn UI before reusable runtime/tool
  registration is clearer.

Do not reconsider:

- Observability-first Task 7 is complete.
- Base instructions are included in the stable prefix cache boundary.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
- Process permission profiles now route admission before execution.
- Shell compatibility must use a real shell boundary under explicit profiles;
  do not revive parser-first M2.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- docs: reframe shell boundary away from parser allowlists

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
