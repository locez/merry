# Handoff

Status: complete

## Current Work

Current milestone or track:

- Runtime Coding Loop Harness first slice.

Session milestone:

- Implement the first deterministic Minimal Useful Coding Loop harness slice.

Task queue status:

- Read-only subagent exploration: completed.
- Deterministic coding-loop harness test: implemented.
- Roadmap/continuity status: updated.
- Validation: passed.

Done condition:

- A deterministic test proves the first coding-loop slice with runtime agent
  loop, workspace patch, process verification, tool continuations, and final
  completion. Continuity state and handoff reflect the implemented evidence and
  the next exact slice.

Drift boundary:

- Do not start a full autonomous coding agent, live-provider harness, broad
  process profile, broad CLI UX, graph memory, skill VM, or Python SDK unless a
  later lease explicitly selects that slice.

Acceptance criteria:

- `Runtime::run_agent_loop` runs at least five provider steps: inspect, read
  exact evidence, patch, verify, final answer.
- The test uses a fake provider and injected fake process runner.
- Exact process argv is recorded for inspection and verification.
- `workspace_patch_file` applies one constrained temp-workspace patch.
- The loop completes, leaves no pending tool calls, and checks
  artifact-before-resolution ledger ordering.
- Four tool-result continuation steps occur before final completion.

## Communication

Language: Chinese

Style notes:

- Keep updates concise and technical.

## What Changed

Files changed:

- `crates/merry-tool-workspace/tests/runtime_integration.rs`
- `ROADMAP.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Summary:

- Added `coding_loop_harness_inspects_patches_verifies_and_completes`, a
  deterministic fake-provider/fake-runner integration test that builds a
  runtime with workspace read/patch tools plus `process_command_tool`.
- The test runs inspect -> exact read -> patch -> verification -> final answer
  through the runtime agent loop.
- The test verifies temp fixture mutation, exact process argv, continuation
  flow, pending-call cleanup, and ledger artifact-before-resolution ordering.
- `ROADMAP.md` now records that the first deterministic slice is complete while
  bwrap/live lanes remain future opt-in work.

## Subagent Evidence

Workers used:

- Feynman: read-only explorer for runtime agent loop/test entry points.
- Godel: read-only explorer for process policy/runner, workspace patch, and
  CLI bwrap interfaces.

Integrated decision:

- Use `merry-tool-workspace` integration tests for the first slice because it
  can prove real temp-workspace read/patch behavior without making
  `merry-runtime` depend on the workspace tool crate.

## Validation

Commands:

- `cargo test -p merry-tool-workspace coding_loop_harness`
- `cargo test -p merry-tool-workspace`
- `cargo fmt --all --check`
- `cargo clippy -p merry-tool-workspace --all-targets -- -D warnings`
- `git diff --check`

Result:

- passed

Known failures:

- Initial sandboxed `cargo test -p merry-tool-workspace coding_loop_harness`
  could not resolve `index.crates.io`; rerunning with approved cargo network
  access succeeded.

## Decisions

Decisions made:

- First slice proves deterministic runtime value with fake provider/fake runner
  and a real temp-workspace patch.
- Real `bwrap` process smoke and live provider smoke remain separate opt-in
  lanes.

Pending decisions:

- Exact CLI/test command shape for the real bwrap coding-loop smoke.
- Whether to load live smoke config from ignored files or require exported env
  vars only.
- How to represent a reusable read-only process profile for file slices such as
  `sed -n RANGE FILE` or an equivalent typed read tool.

## Blockers

Blockers:

- none

Next exact action:

- Add the real `bwrap` sandbox smoke for the same coding-loop shape against a
  disposable fixture repository.

## Scope For Next Session

Allowed edits:

- Runtime/CLI test or harness files needed for the bwrap smoke.
- Minimal CLI dependency/wiring if needed to register workspace tools inside the
  smoke.
- Small docs/status updates tied to that slice.

Forbidden edits:

- Private raw docs.
- Real credentials.
- Broad roadmap rewrites unless the implementation exposes a blocker.

Do not reconsider:

- Policy taxonomy is support work, not the current P0 deliverable.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- project-continuity: add coding loop harness

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
