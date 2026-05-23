# Handoff

Status: complete

## Current Work

Current milestone or track:

- Real bwrap coding-loop smoke.

Session milestone:

- Add an opt-in CLI smoke that runs the coding-loop shape inside the existing
  `merry --with-sandbox` bwrap handoff.

Task queue status:

- Read-only subagent exploration: completed.
- Opt-in CLI debug smoke command: implemented.
- Help/usage, non-sandbox denial, and ignored real bwrap integration tests:
  implemented.
- Roadmap/continuity status: updated.
- Validation: passed.

Done condition:

- `merry --with-sandbox debug coding-loop-smoke` runs a deterministic
  coding-loop shape inside the real CLI bwrap handoff, uses real process
  execution for inspection/verification, applies a constrained workspace patch
  to a disposable ignored fixture, and reports deterministic success.

Drift boundary:

- Do not start a full autonomous coding agent, live-provider harness, broad
  process profile, broad CLI UX, graph memory, skill VM, Python SDK, or
  arbitrary shell expansion unless a later lease explicitly selects that slice.

Acceptance criteria:

- The CLI command is explicit and non-default under debug tooling.
- The command refuses to run without validated `--with-sandbox` child handoff
  evidence.
- The smoke uses a deterministic scripted provider, not a live provider.
- The smoke uses `TokioProcessRunner` for real `rg --files` and `rg new`
  execution inside the sandbox.
- The smoke uses `workspace_patch_file` and mutates only
  `.merry/local/coding-loop-smoke`.
- The loop reaches `AgentLoopStatus::Completed`, leaves no pending tool calls,
  records four successful tool resolutions, and validates the patched fixture.
- Default `cargo test` does not require bwrap or live credentials.

## Communication

Language: Chinese

Style notes:

- Keep updates concise and technical.

## What Changed

Files changed:

- `Cargo.lock`
- `crates/merry-cli/Cargo.toml`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `ROADMAP.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `DECISIONS.md`

Summary:

- Added `merry-tool-workspace` as a `merry-cli` dependency for the opt-in
  smoke composition.
- Added `debug coding-loop-smoke`, which requires real CLI bwrap handoff
  evidence before running.
- The smoke creates a disposable fixture under `.merry/local/coding-loop-smoke`,
  builds a runtime with workspace read/patch tools plus `process_command_tool`,
  and runs inspect -> exact read -> patch -> verification -> final answer
  through `Runtime::run_agent_loop`.
- Added tests for clap parsing, usage rejection outside sandbox, help output,
  and an ignored real bwrap integration smoke.
- Updated `ROADMAP.md` so the bwrap smoke is completed and the next active work
  is reusable runtime-owned process/tool profiles plus live-provider smoke
  config.

## Subagent Evidence

Workers used:

- Parfit: read-only explorer for runtime process policy, workspace patch, and
  coding-loop blockers.
- Hypatia: read-only explorer for CLI bwrap smoke shape and test placement.

Integrated decisions:

- Keep the first real bwrap smoke deterministic-provider based. It proves the
  sandbox/process/patch/loop path without requiring credentials or live model
  behavior.
- Use `rg new` for the first real verification process instead of fixture-local
  `cargo test`; current process policy only admits the existing narrow cargo
  shape and broadening cargo would be a separate policy/profile task.
- Keep the smoke hidden behind explicit debug tooling and ignored integration
  testing so default tests remain deterministic and offline.

## Validation

Commands:

- `cargo fmt --all --check`
- `cargo clippy -p merry-cli --all-targets -- -D warnings`
- `cargo test -p merry-cli`
- `cargo test -p merry-cli debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`
- `git diff --check`

Result:

- passed

Known failures:

- none

## Decisions

Decisions made:

- The first real bwrap coding-loop smoke is a deterministic CLI debug command,
  not a live-provider test.
- The disposable smoke fixture lives under ignored local state:
  `.merry/local/coding-loop-smoke`.
- The first real process verification uses admitted `rg new`; fixture-local
  build/test verification should wait for a reusable process profile.

Pending decisions:

- How to represent the reusable runtime-owned read-only process profile for
  file listing, literal search, and exact source slices.
- Where the reusable coding-loop tool-set registration should live so upper
  layers do not assemble it ad hoc.
- Whether live smoke config should stay env-only or also support ignored local
  config files such as `.env.merry.local` or `.merry/secrets/`.

## Blockers

Blockers:

- none

Next exact action:

- Implement a reusable runtime-owned read-only process profile or tool-set
  registration layer for the coding-loop harness, covering `rg --files`,
  literal search, and exact source evidence retrieval without adding one
  command match at a time.

## Scope For Next Session

Allowed edits:

- Runtime/process policy/profile code needed for reusable read-only process
  coverage.
- Tool-set registration code needed to make coding-loop harness composition
  reusable from libraries.
- Tests proving the reusable profile/tool-set with fake runner and, if scoped,
  the existing bwrap smoke.
- Small docs/status updates tied to that slice.

Forbidden edits:

- Private raw docs.
- Real credentials.
- Broad roadmap rewrites unless the implementation exposes a blocker.
- Live-provider harness unless the next lease explicitly selects it.

Do not reconsider:

- Policy taxonomy is support work, not the current P0 deliverable.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- project-continuity: add bwrap coding loop smoke

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
