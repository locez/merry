# Handoff

Status: complete

## Current Work

Current milestone or track:

- Roadmap/MVP recalibration around a real sandboxed coding-agent runtime loop.

Session milestone:

- Re-anchor the current P0 to a minimal useful coding-loop MVP and initialize project-continuity state.

Task queue status:

- Continuity artifacts: created in this lease.
- Roadmap P0: updated to Minimal Useful Coding Loop.
- Entry points: `README.md` and `AGENTS.md` aligned with the corrected P0.
- Validation: passed.

Done condition:

- Continuity files exist, roadmap current phase points to the real sandboxed coding-loop MVP, local credential/config handling is documented, and next session has one exact implementation action.

Drift boundary:

- Do not start Rust implementation, full skill VM, graph memory, Python SDK, or live-provider harness implementation in this lease.

Acceptance criteria:

- `SESSION_RUNBOOK.md`, `AGENT_ROLES.md`, `EXECUTION_STATE.md`, and `HANDOFF.md` exist.
- `ROADMAP.md` current phase identifies the real sandboxed coding loop as P0.
- Ignored local credentials and raw source material are protected from commit.

## Communication

Language: Chinese

Style notes:

- Keep updates concise and technical.

## What Changed

Files changed:

- `.gitignore`
- `AGENTS.md`
- `README.md`
- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `EXECUTION_STATE.md`
- `DECISIONS.md`
- `HANDOFF.md`
- `ROADMAP.md`

Summary:

- Initialized project-continuity state, protected ignored raw/local secret
  paths, and re-anchored the public roadmap on the Minimal Useful Coding Loop
  as the current P0.

## Validation

Command:

- `git diff --check`

Result:

- passed

Known failures:

- none

## Decisions

Decisions made:

- Current P0 should prove runtime usefulness with a real sandboxed coding-loop MVP, not policy-only progress.

Pending decisions:

- Exact command shape for the opt-in live-provider harness.
- Whether to add file-based env loading or require users to export env vars manually.

## Blockers

Blockers:

- none

Next exact action:

- Implement the first Runtime Coding Loop Harness slice.

## Scope For Next Session

Allowed edits:

- Runtime/CLI tests or harness files needed for the first coding-loop slice.
- Small docs/status updates tied to that slice.

Forbidden edits:

- Private raw docs.
- Real credentials.
- Broad roadmap rewrites unless the implementation exposes a blocker.

Do not reconsider:

- Policy taxonomy is support work, not the current P0 deliverable.
- Default tests remain deterministic/offline; live provider and bwrap smoke are opt-in.

## Commit

Status: committed

Message:

- project-continuity: recalibrate mvp roadmap

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
