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

- Implement Task 6 from `plans/2026-05-23-config-backed-observability.md`:
  workspace tool and OpenAI-compatible provider trace alignment.

Goal:

- Extend the config-backed structured trace vocabulary from runtime/process
  paths to workspace read/list/search/patch tools and provider request metadata
  without logging file contents, raw search queries, patch text, provider wire
  payloads, prompt text, API keys, or response payloads.

Task queue status:

- Task 1, XDG TOML config model: completed.
- Task 2, config-backed log initialization: completed.
- Task 3, sandbox config/log mount planning: completed.
- Task 4, XDG provider config for OpenAI-compatible debug paths: completed.
- Task 5, runtime loop and process tracing: completed.
- Task 6, workspace tool and provider trace alignment: completed.
- `ROADMAP.md` updated to reflect completed workspace/provider trace alignment
  and the next log-enabled smoke verification gap.
- Plan progress checkboxes updated for Tasks 1-6. Task 7 remains next.
- Continuity state and handoff: completed.

Allowed expansion:

- Workspace tool trace instrumentation and tests required by
  `plans/2026-05-23-config-backed-observability.md`, Task 6.
- OpenAI-compatible provider metadata trace instrumentation and tests required
  by Task 6.
- Public-safe roadmap status update for implementation facts changed by this
  lease.
- Continuity file updates.

Done condition:

- Workspace read/list/search/patch executors emit `runtime.workspace_tool.start`
  and `runtime.workspace_tool.finish` traces with stable tool/call fields,
  bounded action summaries, statuses, diagnostic codes, and output byte counts
  where an outcome exists.
- Workspace traces cover success, domain failure, invalid arguments, and
  cancellation after start; implementation paths also emit infrastructure-error
  finish traces without logging invalid payloads, file contents, raw search
  query text, or patch text.
- OpenAI-compatible provider tracing emits safe request metadata at
  `runtime.provider.request` and uses a distinct `runtime.provider.stream`
  span for stream setup metadata.
- Focused and full validation pass.
- Reviewer pass has no blocking findings.
- Handoff updated and lease committed.

Drift boundary:

- Do not implement Task 7 end-to-end log-enabled smoke verification in this
  lease.
- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.

Task type: implementation

Acceptance criteria:

- Workspace tool traces include `runtime.workspace_tool.start` and
  `runtime.workspace_tool.finish`.
- Workspace finish traces distinguish `succeeded`, `failed`, `cancelled`, and
  `error` statuses and carry existing diagnostic codes where applicable.
- Workspace trace summaries are bounded and safe: no file contents, raw search
  query text, patch old/new text, invalid argument payload, or absolute host
  root leakage through trace fields.
- Provider traces include provider name, model, message/tool/continuation
  counts, `max_output_tokens`, `allow_parallel_tool_calls`, and endpoint path
  without API keys, prompts, provider wire payloads, or response payloads.
- Workspace-wide Rust validation passes.

## Scope

Allowed edits:

- `Cargo.lock`
- `crates/merry-tool-workspace/Cargo.toml`
- `crates/merry-tool-workspace/src/lib.rs`
- `crates/merry-provider-openai/src/provider.rs`
- `plans/2026-05-23-config-backed-observability.md`
- `ROADMAP.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content
- Task 7 CLI log-smoke implementation in this lease

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo test -p merry-tool-workspace workspace_tool_invalid_arguments_trace_failed_without_payload -- --nocapture`
- `cargo test -p merry-tool-workspace workspace_ -- --nocapture`
- `cargo test -p merry-provider-openai provider_ -- --nocapture`
- `cargo test -p merry-tool-workspace`
- `cargo test -p merry-provider-openai`
- `cargo clippy -p merry-tool-workspace --all-targets --all-features -- -D warnings`
- `cargo clippy -p merry-provider-openai --all-targets --all-features -- -D warnings`
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- `git diff --check`
- `git status --short`

Validation notes:

- The invalid-arguments trace test was written first and failed because no
  `runtime.workspace_tool.finish` record existed for parse failures; it passed
  after adding payload-free invalid-argument tracing.
- Focused workspace trace tests passed for read/list/search/patch redaction,
  bounded path summary, invalid arguments, cancellation after start, and domain
  failure paths.
- Provider focused tests passed for metadata helper redaction, actual
  request-render metadata emission, and stream span safe fields.
- Full package tests for `merry-tool-workspace` and `merry-provider-openai`
  passed.
- Package and workspace clippy passed with `-D warnings`.
- Workspace `cargo test --all` passed. Ignored live/network/bwrap tests remained
  ignored/non-default.
- `cargo fmt --all --check` and `git diff --check` passed.
- First reviewer pass found missing finish traces for cancelled/error paths,
  unbounded path summaries, provider span/event naming ambiguity, and a weak
  provider render-path test; those were fixed.
- Final reviewer pass found invalid-arguments parse failures bypassed workspace
  traces; this was fixed with a red/green regression test.

## Research

Research required: no

Research reason:

- The implementation plan and local repo evidence were sufficient. No external
  behavior needed lookup.

Research artifact:

- Repo inspection of workspace tool executors, provider request rendering,
  trace capture behavior, deterministic tests, roadmap status, and plan task
  requirements.

## Next Action

Next exact action:

- Continue `plans/2026-05-23-config-backed-observability.md` at Task 7:
  End-To-End Log-Enabled Smoke Verification. Start with a deterministic CLI log
  smoke that enables file-backed JSON logs from XDG TOML config and asserts the
  log contains runtime loop, provider request, workspace tool, process
  execution, artifact/tool resolution, diagnostic, and final loop status
  records without secrets or raw payload contents.

Do not reconsider:

- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before observability exists.
- Do not reintroduce repo-local `.merry/secrets/openai.env` as the live-smoke
  provider config path.
- Do not reopen Task 6 unless a regression appears.
