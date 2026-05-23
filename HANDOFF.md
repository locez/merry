# Handoff

Status: rollover

## Current Work

Current milestone or track:

- LLM-driven sandbox coding-loop smoke.

Session milestone:

- Add an opt-in live OpenAI-compatible smoke that runs the coding-loop shape
  inside the existing `merry --with-sandbox` bwrap handoff with real LLM tool
  selection.

Task queue status:

- Runtime continuation keeps the original task visible after tool results:
  implemented.
- Ignored local OpenAI-compatible config for sandboxed live smoke:
  implemented.
- `debug coding-loop-live-smoke` CLI command using `OpenAiProvider`:
  implemented.
- Parser/gating/default-offline/ignored-live-smoke tests: implemented.
- Roadmap, decisions, and continuity state: updated.
- Credentialed live LLM proof: pending local `.merry/secrets/openai.env`.

Done condition:

- The live smoke reaches `AgentLoopStatus::Completed` inside the CLI bwrap
  sandbox, leaves no pending tool calls, validates the patched fixture content,
  and runtime events prove real `rg --files`, exact source read,
  `workspace_patch_file`, and real `rg fixed-by-live-llm` verification calls
  chosen by the live model.

Drift boundary:

- Do not start a full autonomous coding agent, broad process profile, broad CLI
  UX, graph memory, skill VM, Python SDK, arbitrary shell expansion, or a
  live-provider judgment path unless a later lease explicitly selects that
  slice.

Acceptance criteria:

- The CLI command is explicit and non-default under debug tooling.
- The command refuses to run without validated `--with-sandbox` child handoff
  evidence before reading config or attempting network.
- The command requires `MERRY_OPENAI_DEBUG=1` via ignored local config or env.
- The smoke uses `OpenAiProvider`, not a deterministic scripted provider, for
  model decisions.
- The smoke uses `TokioProcessRunner` for real `rg --files` and
  `rg fixed-by-live-llm` execution inside the sandbox.
- The smoke uses `workspace_patch_file` and mutates only
  `.merry/local/coding-loop-live-smoke`.
- Default `cargo test` does not require bwrap, network, or live credentials.

## Communication

Language: Chinese

Style notes:

- Keep updates concise and technical.

## What Changed

Files changed:

- `crates/merry-runtime/src/agent_loop.rs`
- `crates/merry-runtime/tests/agent_loop.rs`
- `crates/merry-tool-workspace/tests/runtime_integration.rs`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `README.md`
- `ROADMAP.md`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Summary:

- `Runtime::run_agent_loop` now includes the original user task in continuation
  inputs after tool results. This addresses the real-model failure mode where
  later turns only saw "Continue after tool result."
- Added `merry --with-sandbox debug coding-loop-live-smoke`, which builds a
  live OpenAI-compatible runtime with workspace read/patch tools and
  `process_command_tool`.
- Added ignored local config parsing for `.merry/secrets/openai.env` with
  supported OpenAI-compatible keys only; host env still works for non-sandboxed
  provider debug paths, but bwrap live smoke should use the file because the
  sandbox clears env.
- The live smoke creates a disposable fixture under
  `.merry/local/coding-loop-live-smoke`, asks the model to run one tool per
  step, validates the patched source, and checks resolved artifacts for real
  process inspection and verification evidence.
- Updated deterministic bwrap smoke fixture values to match the live smoke
  target string: `"unfixed"` -> `"fixed-by-live-llm"`.
- Updated roadmap/decision docs so scripted-provider success and live-provider
  proof are tracked separately.

## Validation

Commands run during the lease:

- `cargo fmt --all --check`
- `cargo clippy -p merry-cli --all-targets -- -D warnings`
- `cargo clippy -p merry-runtime --all-targets -- -D warnings`
- `cargo test -p merry-cli`
- `cargo test -p merry-runtime agent_loop`
- `cargo test -p merry-tool-workspace coding_loop_harness`
- `cargo test -p merry-cli debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`
- `cargo run -p merry-cli -- --with-sandbox debug coding-loop-live-smoke`
- `git diff --check`

Result:

- Deterministic checks passed.
- The live command exited 2 before network because `.merry/secrets/openai.env`
  is missing.

Live proof status:

- Not passed yet. `cargo run -p merry-cli -- --with-sandbox debug
  coding-loop-live-smoke` currently fails before network with missing
  `.merry/secrets/openai.env`, which is the intended credential gate.

## Decisions

Decisions made:

- Live LLM coding-loop proof is a separate acceptance gate from deterministic
  scripted-provider bwrap proof.
- Sandboxed live smoke should read ignored local `KEY=value` config from
  `.merry/secrets/openai.env` because bwrap clears host environment before CLI
  execution.
- Agent-loop continuation input should carry the original task text; runtime
  state remains structured, but real model turns still need the objective.

Pending decisions:

- Whether the live prompt/tool contract is sufficient once a real credentialed
  run is attempted.
- How to represent reusable runtime-owned read-only process profiles and
  coding-loop tool-set registration outside ad hoc CLI assembly.

## Blockers

Blockers:

- No tracked blocker. The next step needs local ignored credentials/config that
  should not be committed.

Next exact action:

- Create `.merry/secrets/openai.env` locally:

```text
MERRY_OPENAI_DEBUG=1
MERRY_OPENAI_API_KEY=<local secret>
MERRY_OPENAI_MODEL=<model>
MERRY_OPENAI_BASE_URL=<optional OpenAI-compatible base URL>
```

- Then run:

```bash
cargo run -p merry-cli -- --with-sandbox debug coding-loop-live-smoke
```

- If it fails after reaching the live provider, inspect the error/runtime
  events and make the smallest prompt, schema, continuation, or process-profile
  correction needed for a passing live loop.

## Scope For Next Session

Allowed edits:

- Small live-smoke prompt/tool-contract/runtime-continuation fixes driven by
  the actual credentialed run.
- Runtime/process profile code needed if live failure proves the profile is too
  narrow.
- Tests and docs tied directly to the live-smoke proof.

Forbidden edits:

- Private raw docs.
- Real credentials.
- Broad roadmap rewrites unless the live proof exposes a blocker.
- Full autonomous coding agent, Python SDK, graph memory, or live judgment
  harness.

Do not reconsider:

- Policy taxonomy is support work, not the current P0 deliverable.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- project-continuity: add live coding loop smoke

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
