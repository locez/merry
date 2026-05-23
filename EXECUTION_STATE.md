# Execution State

Lease status: complete

## Source Of Truth

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `ROADMAP.md`
- `README.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `plans/2026-05-23-config-backed-observability.md`
- `examples/config.toml`
- `docs/design/mvp-design.md` (ignored private local design source)
- `docs/design/global-design.md` (ignored private local design source)
- `docs/product/product-strategy.md` (ignored private local product source)
- `merry-raw-docs/` (ignored original local source material; do not commit)

## Planning Maturity

Level: implementation-in-progress

Current planning artifact:

- `ROADMAP.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `plans/2026-05-23-config-backed-observability.md`
- `examples/config.toml`

## Communication

Language: Chinese

Style notes:

- Keep user-facing updates concise and direct.
- Preserve Rust/API/config names in English.

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding loop.

Session milestone:

- Task 7, end-to-end log-enabled smoke verification.

Goal:

- Prove that config-backed JSON logs can capture the deterministic coding-loop
  smoke end to end: runtime loop, provider request, workspace tool, process
  execution, artifact record, tool resolution, diagnostic-code, and completed
  terminal status records, without raw prompt, source, process stdout, provider
  wire payload, model final text, or secret-like content.

Task queue status:

- Task 1, XDG TOML config model: completed.
- Task 2, config-backed log initialization: completed.
- Task 3, sandbox config/log mount planning: completed.
- Task 4, XDG provider config for OpenAI-compatible debug paths: completed.
- Task 5, runtime loop and process tracing: completed.
- Task 6, workspace tool and provider trace alignment: completed.
- Task 6A, user-facing example config contract: completed.
- Task 7, end-to-end log-enabled smoke verification: completed.
- Runtime now emits provider-neutral `runtime.provider.request` metadata before
  any provider call and `runtime.artifact.record` after artifact state is
  written.
- CLI tests isolate default integration-test XDG roots from host user config.

Allowed expansion:

- Runtime/provider-neutral trace fields required to make deterministic smoke
  logs cover the accepted observability contract.
- CLI deterministic test support and integration-test XDG isolation.
- Public-safe README, roadmap, plan, decision, and continuity status updates.

Done condition:

- Deterministic CLI-crate coding-loop log smoke enables file-backed JSON logs
  from XDG TOML config and asserts the combined log contains loop, provider,
  tool, workspace, process, artifact, diagnostic-code, and final status records.
- The deterministic log smoke asserts raw prompt/source/stdout/model final
  output/provider wire/secret-like payloads are absent.
- CLI integration tests cover the default XDG state log path and clear failure
  when the log parent cannot be created.
- Runtime trace tests assert provider request and artifact record events.
- Real deterministic `bwrap` coding-loop smoke with temporary XDG log config
  passes and the log tail contains the expected combined records.
- Focused and full default validation pass.
- Handoff updated and lease committed.

Drift boundary:

- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.
- Do not make live provider behavior part of default tests.

Task type: implementation/docs

Acceptance criteria:

- `coding_loop_smoke_writes_configured_json_log_records_without_payloads`
  covers deterministic coding-loop log content from XDG TOML config.
- `debug_command_writes_runtime_action_logs_to_default_xdg_state_path` proves
  omitted log path resolves to the XDG state fallback.
- `debug_command_fails_clearly_when_default_log_parent_cannot_be_created`
  proves logging setup fails clearly before writing command stdout.
- `agent_loop_traces_loop_steps_tool_process_and_terminal_status` covers
  provider request and artifact record events in runtime trace capture.
- `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  and `cargo test --all` pass.

## Scope

Allowed edits:

- `README.md`
- `ROADMAP.md`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `plans/2026-05-23-config-backed-observability.md`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `crates/merry-runtime/src/agent_loop.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/src/session.rs`
- `crates/merry-runtime/tests/agent_loop.rs`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content
- full-screen TUI, REPL, or multi-turn UI scope

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo test -p merry-cli coding_loop_smoke_writes_configured_json_log_records_without_payloads -- --nocapture`
- `cargo test -p merry-cli debug_command --test debug -- --nocapture`
- `cargo test -p merry-runtime agent_loop_traces_loop_steps_tool_process_and_terminal_status -- --nocapture`
- `cargo test -p merry-cli`
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- `target/debug/merry --with-sandbox debug coding-loop-smoke` with temporary
  XDG config enabling JSON logs
- `git diff --check`
- `git status --short --untracked-files=all`

Validation notes:

- The focused coding-loop log smoke failed first because successful log records
  omitted `diagnostic_code`; the runtime loop/tool finish logs now emit an
  empty diagnostic code on successful paths and focused validation passes.
- First `cargo test` attempts needed network escalation to download missing
  crates. After dependencies were cached, default checks ran normally.
- The real deterministic `bwrap` smoke passed with temporary XDG log config and
  stdout `coding-loop-smoke: ok`.
- Validation remains deterministic/offline by default; live provider and bwrap
  smoke lanes remain opt-in/manual.

## Research

Research required: yes

Research reason:

- User allowed subagents; a read-only researcher checked which existing trace
  events and fields Task 7 could rely on, and identified the deterministic
  provider-request gap.

Research artifact:

- Subagent finding: runtime loop/tool/process/workspace trace points already
  existed; deterministic `CodingLoopSmokeProvider` did not naturally emit
  `runtime.provider.request`, so provider-neutral runtime request tracing was
  needed.

## Next Action

Next exact action:

- Start the next lease from `ROADMAP.md` Next Active: replace one-off process
  classification growth with a runtime-owned read-only process profile for
  reusable workspace inspection and exact evidence retrieval. Initial coverage
  should include `rg --files`, literal `rg <pattern>`, and a file-slice shape
  such as `sed -n RANGE FILE` or an equivalent typed read tool.

Do not reconsider:

- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before reusable runtime/process/tool profiles are
  clearer.
- Do not reintroduce repo-local `.merry/secrets/openai.env` as the live-smoke
  provider config path.
- Do not let future config schema changes bypass `examples/config.toml`.
