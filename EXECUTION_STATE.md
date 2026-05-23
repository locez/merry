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

## Communication

Language: Chinese

Style notes:

- Keep user-facing updates concise and direct.
- Preserve Rust/API/config names in English.

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding loop.

Session milestone:

- Implement Task 5 from `plans/2026-05-23-config-backed-observability.md`:
  runtime loop and process-action tracing with stable correlation fields.

Goal:

- Make the serial runtime agent loop and process execution path explainable in
  config-backed structured logs before extending the same trace vocabulary to
  workspace tools and provider metadata.

Task queue status:

- Task 1, XDG TOML config model: completed.
- Task 2, config-backed log initialization: completed.
- Task 3, sandbox config/log mount planning: completed.
- Task 4, XDG provider config for OpenAI-compatible debug paths: completed.
- Task 5, runtime loop and process tracing: completed.
- `ROADMAP.md` updated to reflect completed runtime-loop/process tracing and
  the remaining workspace-tool/provider trace-alignment gap.
- Plan progress checkboxes updated for Tasks 1-5. Task 6 remains next.
- Continuity state and handoff: completed.

Allowed expansion:

- Runtime loop/process tracing implementation required by
  `plans/2026-05-23-config-backed-observability.md`, Task 5.
- Public-safe roadmap status update for implementation facts changed by this
  lease.
- Continuity file updates.

Done condition:

- `Runtime::run_agent_loop` emits loop start/finish, step start, pending-tool,
  and tool execution start/finish traces with `session_id`, `step_index`,
  `tool_call_id`, `tool_name`, terminal status, artifact IDs, and diagnostic
  codes where applicable.
- The process execution path emits start/finish traces with argv/cwd, byte
  counts, truncation flags, and status, without logging stdout/stderr contents.
- Denied process actions emit a denied tool finish trace and do not emit process
  execution start/finish traces.
- Focused and full validation pass.
- Handoff updated and lease committed.

Drift boundary:

- Do not implement Task 6 workspace-tool or provider trace alignment in this
  lease.
- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.

Task type: implementation

Acceptance criteria:

- Runtime loop traces include `runtime.loop.start`, `runtime.step.start`,
  `runtime.tool.pending`, `runtime.tool.execute.start`,
  `runtime.tool.execute.finish`, and `runtime.loop.finish`.
- Process action traces include `runtime.process.execute.start` and
  `runtime.process.execute.finish` for admitted process actions only.
- Policy-denied process proposals produce one denied tool finish trace with
  `diagnostic_code = "action_policy_denied"` and no process execution trace.
- Runtime trace capture tests are deterministic/offline and stable under the
  default parallel Rust test harness.
- Workspace-wide Rust validation passes.

## Scope

Allowed edits:

- `Cargo.lock`
- `crates/merry-runtime/Cargo.toml`
- `crates/merry-runtime/src/agent_loop.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/tests/agent_loop.rs`
- `plans/2026-05-23-config-backed-observability.md`
- `ROADMAP.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content
- Task 6 workspace-tool/provider trace alignment in this lease

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo test -p merry-runtime executor_infrastructure_error_preserves_events_and_pending_call -- --nocapture`
- `cargo test -p merry-runtime denied_registered_tool_resolves_failed_and_agent_loop_continues_once -- --nocapture`
- `cargo test -p merry-runtime agent_loop_traces_loop_steps_tool_process_and_terminal_status -- --nocapture`
- `cargo test -p merry-runtime denied_process_action_traces_denied_tool_finish_without_process_execution -- --nocapture`
- `cargo test -p merry-runtime process -- --nocapture`
- `cargo test -p merry-runtime`
- `cargo clippy -p merry-runtime --all-targets --all-features -- -D warnings`
- `cargo fmt --all --check`
- `cargo test --all`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `git diff --check`
- `git status --short`

Validation notes:

- The new trace tests first exposed missing/error-path trace coverage and then
  passed after instrumentation.
- Trace capture uses a process-global JSON subscriber plus per-session marker
  filtering so tests remain stable under the default parallel Rust harness.
- Focused loop/process tests passed for completed, denied, and executor-error
  paths. The completed process trace test asserts stdout/stderr byte counts and
  verifies stdout content is not logged.
- `cargo test -p merry-runtime` passed.
- `cargo clippy -p merry-runtime --all-targets --all-features -- -D warnings`
  passed.
- First `cargo test --all` attempt failed in the sandbox because DNS could not
  resolve `static.crates.io` for missing cached dependencies. The command was
  rerun with approved network access and passed all workspace tests. Ignored
  live/network/bwrap tests remained ignored.
- `cargo clippy --all-targets --all-features -- -D warnings` passed for the
  workspace.
- `cargo fmt --all --check` and `git diff --check` passed.
- Review found one duplicate denied-tool finish trace risk; the loop now skips
  its own generic finish event when the lower denied-action path already emitted
  `status = "denied"`.

## Research

Research required: no

Research reason:

- The implementation plan and local repo evidence were sufficient. No external
  behavior needed lookup.

Research artifact:

- Repo inspection of runtime agent loop, process-action execution, trace
  capture behavior, deterministic tests, roadmap status, and plan task
  requirements.

## Next Action

Next exact action:

- Continue `plans/2026-05-23-config-backed-observability.md` at Task 6:
  Workspace Tool And Provider Trace Alignment. Start with workspace trace
  capture tests, then instrument workspace read/list/search/patch and provider
  metadata traces without logging file contents, raw provider payloads, or API
  keys.

Do not reconsider:

- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before observability exists.
- Do not reintroduce repo-local `.merry/secrets/openai.env` as the live-smoke
  provider config path.
- Do not reopen Task 5 unless a regression appears.
