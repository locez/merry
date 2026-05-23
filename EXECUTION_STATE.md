# Execution State

Lease status: complete

## Source Of Truth

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `ROADMAP.md`
- `README.md`
- `docs/design/mvp-design.md` (ignored private local design source)
- `docs/design/global-design.md` (ignored private local design source)
- `docs/product/product-strategy.md` (ignored private local product source)
- `merry-raw-docs/` (ignored original local source material; do not commit)

## Planning Maturity

Level: structured-roadmap

Current planning artifact:

- `ROADMAP.md`

## Communication

Language: Chinese

Style notes:

- Keep user-facing updates concise and direct.
- Preserve Rust/API/config names in English.

## Current Work

Current milestone or track:

- Runtime Coding Loop Harness first slice.

Session milestone:

- Implement the first deterministic Minimal Useful Coding Loop harness slice.

Goal:

- Add a focused fake-provider/fake-runner test that proves one runtime-owned loop can inspect workspace evidence, apply one constrained patch, run verification, continue through tool results, and finish with deterministic events/artifacts/ledger facts.

Task queue:

- Dispatched read-only subagent exploration for runtime loop and process/patch/sandbox entry points.
- Implemented the deterministic multi-step coding-loop harness test.
- Updated roadmap/continuity status to name exactly what the first slice proves and what remains.
- Ran focused verification.
- Commit is pending final staged review.

Allowed expansion:

- Minimal helper test code under the owned integration-test file.
- Small documentation/status updates tied to the implemented slice.

Done condition:

- A deterministic test proves the first coding-loop slice with runtime agent loop, workspace patch, process verification, tool continuations, and final completion. Continuity state and handoff reflect the implemented evidence and the next exact slice.

Drift boundary:

- Do not implement a full autonomous coding agent, real live-provider harness, broad process profile, broad CLI UX, graph memory, skill VM, or Python SDK in this lease.

Task type: implementation

Acceptance criteria:

- A focused test runs `Runtime::run_agent_loop` for at least five provider steps: inspect, read exact evidence, patch, verify, final answer.
- The test uses a fake provider and injected fake process runner, not a live provider or host process.
- The loop records exact process argv for inspection and verification.
- The patch goes through `workspace_patch_file` with low-risk patch opt-in and mutates only a temp workspace fixture.
- The loop reaches `AgentLoopStatus::Completed`, leaves no pending tool calls, and records artifact-before-resolution ordering through lifecycle facts.
- The loop performs four tool-result continuation steps before the final
  completion; each continuation carries the newly resolved tool result through
  provider-neutral runtime state.

## Scope

Allowed edits:

- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `DECISIONS.md`
- `ROADMAP.md`
- `crates/merry-tool-workspace/tests/runtime_integration.rs`
- small `README.md` status update if needed

Forbidden edits:

- Rust production runtime/provider/CLI implementation outside the test harness slice
- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `ROADMAP.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo test -p merry-tool-workspace coding_loop_harness`
- `git diff --check`

Validation status: passed

Validation notes:

- `cargo test -p merry-tool-workspace coding_loop_harness` passed.
- `cargo test -p merry-tool-workspace` passed.
- `cargo fmt --all --check` passed after formatting.
- `cargo clippy -p merry-tool-workspace --all-targets -- -D warnings` passed.
- `git diff --check` passed.

## Research

Research required: no

Research reason:

- User allowed subagents. Read-only explorer subagents are being used to confirm existing runtime loop/process/patch entry points; no external research is required.

Research artifact:

- Explorer outputs from subagent workers Feynman and Godel in this lease.

## Next Action

Next exact action:

- Add the real `bwrap` sandbox smoke for the same coding-loop shape, likely by introducing a CLI or test wrapper that can register workspace tools, use a disposable fixture, and run with the existing `--with-sandbox` handoff without inheriting secrets.

Do not reconsider:

- Do not make policy taxonomy the primary P0 output.
- Do not commit `docs/`, `merry-raw-docs/`, `.env.merry.local`, `.merry/local/`, or `.merry/secrets/`.
- Do not require live provider tests in default `cargo test`.
