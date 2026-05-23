# Execution State

Lease status: rollover

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

- LLM-driven sandbox coding-loop smoke.

Session milestone:

- Add an opt-in live OpenAI-compatible smoke that runs the coding-loop shape
  inside the existing `merry --with-sandbox` bwrap handoff with real LLM tool
  selection.

Goal:

- Prove a real LLM can drive the inspect -> exact evidence -> constrained patch
  -> verification -> final loop shape through Merry runtime tools inside the
  CLI bwrap sandbox, while default tests remain deterministic/offline and
  credentials stay ignored/local.

Task queue status:

- Fix agent-loop continuation input so real LLM steps keep the original task
  goal visible after tool results: implemented.
- Add ignored local config loading for live smoke credentials inside the bwrap
  workspace: implemented.
- Add an explicit non-default live LLM coding-loop smoke command requiring
  `--with-sandbox` and OpenAI debug opt-in: implemented.
- Add tests for argument parsing, credential/config gating, default-suite
  non-network behavior, and ignored live smoke execution: implemented.
- Update roadmap/continuity status with what the live LLM slice proves and
  what remains: implemented.
- Run the credentialed live LLM smoke: pending local `.merry/secrets/openai.env`.
- Commit or record why the lease cannot be committed: completed.

Allowed expansion:

- Minimal runtime agent-loop continuation fix needed for real LLM reliability.
- Minimal CLI helper code for local ignored config parsing, live provider
  runtime construction, prompt/task text, and event/result validation.
- Small documentation/status updates tied to the implemented slice.

Done condition:

- The CLI exposes an explicit opt-in live LLM smoke command that refuses to run
  outside a validated sandbox handoff and OpenAI debug opt-in, reads credentials
  from ignored local config or env where safe, uses a real OpenAI-compatible
  provider to choose tools, executes the coding-loop shape inside bwrap with
  real process runner and workspace patch tooling, and reports success only
  when runtime events prove the LLM-driven tool sequence and fixture result.

Rollover reason:

- The implementation and deterministic verification are complete, but this
  environment does not have `.merry/secrets/openai.env`, so the true live LLM
  proof has not run. The current live command fails before any network attempt
  with a missing-config usage error, which is the intended safe gate.

Drift boundary:

- Do not implement a full autonomous coding agent, broad process profile, broad
  CLI UX, graph memory, skill VM, Python SDK, arbitrary shell expansion, or a
  live-provider judgment path in this lease.

Task type: implementation

Acceptance criteria:

- The CLI command is explicit and non-default, under debug/smoke tooling.
- The command requires the real `--with-sandbox` child handoff evidence before
  running local workspace effect verification.
- The command requires `MERRY_OPENAI_DEBUG=1` or an equivalent ignored local
  config opt-in before any network attempt.
- Credentials and base URL can be supplied from ignored local files that remain
  available inside the sandbox; secrets are not passed through bwrap argv or
  committed env.
- The smoke uses `OpenAiProvider`, not a deterministic scripted provider, for
  the model decisions.
- The smoke uses `TokioProcessRunner` for real `rg --files` and verification
  process execution inside the sandbox.
- The smoke uses `workspace_patch_file` for the edit and mutates only a
  disposable fixture under ignored local state.
- Runtime events prove at least one process inspection call, one exact source
  read/search call, one workspace patch call, and one process verification call
  resolved successfully before final completion.
- The smoke reaches `AgentLoopStatus::Completed`, leaves no pending tool calls,
  and validates the patched fixture content.
- Default `cargo test` does not require bwrap, network, or live credentials.

## Scope

Allowed edits:

- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `DECISIONS.md`
- `ROADMAP.md`
- `README.md`
- `crates/merry-runtime/src/agent_loop.rs`
- `crates/merry-runtime/tests/agent_loop.rs`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `crates/merry-tool-workspace/tests/runtime_integration.rs`

Forbidden edits:

- Rust production runtime/provider/CLI implementation outside the live smoke
  and agent-loop continuation slice
- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `ROADMAP.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo fmt --all --check`
- `cargo clippy -p merry-cli --all-targets -- -D warnings`
- `cargo clippy -p merry-runtime --all-targets -- -D warnings`
- `cargo test -p merry-cli`
- `cargo test -p merry-runtime agent_loop`
- `cargo test -p merry-tool-workspace coding_loop_harness`
- `cargo test -p merry-cli debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`
- `git diff --check`
- opt-in live LLM bwrap smoke command when local credentials are configured

Validation notes:

- Focused deterministic tests, clippy, formatting, diff whitespace, and the
  deterministic real bwrap ignored smoke passed.
- The live command was attempted with
  `cargo run -p merry-cli -- --with-sandbox debug coding-loop-live-smoke` and
  failed before network because `.merry/secrets/openai.env` does not exist.

## Research

Research required: yes

Research reason:

- This lease touches live provider behavior and runtime agent-loop semantics.
  Repo evidence is sufficient; no external research is required.

Research artifact:

- Local inspection of OpenAI provider rendering/parsing, runtime agent loop
  continuation, CLI bwrap env clearing, and workspace/process tool boundaries.

## Next Action

Next exact action:

- Create `.merry/secrets/openai.env` locally with debug opt-in, API key, model,
  and optional base URL, then run
  `cargo run -p merry-cli -- --with-sandbox debug coding-loop-live-smoke`.
  If the live model deviates, inspect the runtime error/events and make the
  smallest prompt/tool-contract fix.

Do not reconsider:

- Do not make policy taxonomy the primary P0 output.
- Do not commit `docs/`, `merry-raw-docs/`, `.env.merry.local`,
  `.merry/local/`, or `.merry/secrets/`.
- Do not require live provider tests in default `cargo test`.
