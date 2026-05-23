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
- Run the credentialed live LLM smoke: passed in the user's trusted configured
  environment.
- Fix provider request metadata exposed by the live smoke: implemented
  `User-Agent`.
- Commit or record why the lease cannot be committed: completed.

Allowed expansion:

- Minimal runtime agent-loop continuation fix needed for real LLM reliability.
- Minimal CLI helper code for local ignored config parsing, live provider
  runtime construction, prompt/task text, and event/result validation.
- Minimal provider HTTP metadata fix exposed by the successful live smoke.
- Small documentation/status updates tied to the implemented slice.

Done condition:

- The CLI exposes an explicit opt-in live LLM smoke command that refuses to run
  outside a validated sandbox handoff and OpenAI debug opt-in, reads credentials
  from ignored local config or env where safe, uses a real OpenAI-compatible
  provider to choose tools, executes the coding-loop shape inside bwrap with
  real process runner and workspace patch tooling, and reports success only
  when runtime events prove the LLM-driven tool sequence and fixture result.

Live proof status:

- The user reported that `cargo run -p merry-cli -- --with-sandbox debug
  coding-loop-live-smoke` passed against their trusted configured server. This
  satisfies the live LLM proof lane for this milestone. This agent sandbox did
  not rerun the trusted external request because elevated external egress was
  rejected by the approval policy.

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
- `crates/merry-provider-openai/src/provider.rs`
- `crates/merry-provider-openai/tests/provider_stream.rs`
- `crates/merry-tool-workspace/tests/runtime_integration.rs`

Forbidden edits:

- Rust production runtime/provider/CLI implementation outside the live smoke,
  agent-loop continuation slice, and provider HTTP request metadata fix
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
- `cargo clippy -p merry-provider-openai --all-targets -- -D warnings`
- `cargo test -p merry-provider-openai`
- `cargo test -p merry-provider-openai builds_responses_http_request_without_network`
- `cargo test -p merry-provider-openai stream_model_posts_responses_request_and_streams_events -- --ignored`
- `git diff --check`
- user-run opt-in live LLM bwrap smoke against trusted configured server

Validation notes:

- The user reported that the credentialed live smoke passed against their
  trusted configured server. That satisfies the live LLM proof lane outside this
  agent sandbox.
- The live run exposed a provider HTTP request metadata gap: Merry did not set
  a `User-Agent` header. This lease adds a provider-layer fix and deterministic
  request-construction coverage.
- Provider request-construction, loopback header integration, full provider
  tests, provider clippy, formatting, and diff whitespace checks passed.

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

- Commit the completed lease.

Do not reconsider:

- Do not make policy taxonomy the primary P0 output.
- Do not commit `docs/`, `merry-raw-docs/`, `.env.merry.local`,
  `.merry/local/`, or `.merry/secrets/`.
- Do not require live provider tests in default `cargo test`.
