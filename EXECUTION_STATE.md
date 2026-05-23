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

- Implement the first config-backed observability slice in `merry-cli`: XDG
  TOML config, config-backed file logging, sandbox config/log mounts, and XDG
  provider config for OpenAI-compatible debug paths.

Goal:

- Let operators enable file-backed structured logs through Merry config, make
  sandboxed debug commands see the same config boundary, and remove the
  repo-local live-smoke provider config path before adding runtime/tool/process
  instrumentation.

Task queue status:

- Task 1, XDG TOML config model: completed.
- Task 2, config-backed log initialization: completed.
- Task 3, sandbox config/log mount planning: completed.
- Task 4, XDG provider config for OpenAI-compatible debug paths: completed.
- `ROADMAP.md` updated to reflect that the live debug smoke now uses XDG TOML
  provider config and rejects the legacy `--config .merry/secrets/openai.env`
  path.
- Plan progress checkboxes updated for Tasks 1-4. Task 5 remains next.
- Continuity state and handoff: completed.

Allowed expansion:

- CLI config/log/sandbox/provider-config implementation required by
  `plans/2026-05-23-config-backed-observability.md`, Tasks 1-4.
- Public-safe roadmap status update for implementation facts changed by this
  lease.
- Continuity file updates.

Done condition:

- `merry-cli` loads optional XDG TOML config, initializes configured file logs
  without changing command stdout, plans sandbox read-only config and optional
  read-write log mounts, and reads OpenAI-compatible debug provider/model/key
  source from XDG TOML instead of repo-local live-smoke config.
- Focused and full validation pass.
- Handoff updated and lease committed.

Drift boundary:

- Do not implement Task 5 runtime/process/workspace/provider tracing in this
  lease.
- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.

Task type: implementation

Acceptance criteria:

- Config discovery reads `$XDG_CONFIG_HOME/merry/config.toml`, falling back to
  `~/.config/merry/config.toml`, and missing config is non-fatal for commands
  that do not require provider settings.
- Observability config supports `enabled`, `level`, `format`, and optional
  `path`; omitted log path uses `$XDG_STATE_HOME/merry/logs/merry.jsonl` or
  `~/.local/state/merry/logs/merry.jsonl`.
- File logging creates parent directories, fails clearly on path errors, and
  leaves command stdout unchanged.
- `--with-sandbox` mounts the resolved Merry config directory read-only and
  mounts the host log directory read-write only when file logging is enabled.
- OpenAI-compatible debug paths require `MERRY_OPENAI_DEBUG=1`, provider/model
  config from XDG TOML, and `api_key_env` or config-relative `api_key_file`.
- Default tests remain deterministic/offline; live and bwrap smokes remain
  ignored/non-default.

## Scope

Allowed edits:

- `Cargo.toml`
- `Cargo.lock`
- `crates/merry-cli/Cargo.toml`
- `crates/merry-cli/src/config.rs`
- `crates/merry-cli/src/observability.rs`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `plans/2026-05-23-config-backed-observability.md`
- `ROADMAP.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content
- Task 5 runtime/process/workspace/provider instrumentation in this lease

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo test -p merry-cli config::tests -- --nocapture`
- `cargo test -p merry-cli observability::tests -- --nocapture`
- `cargo test -p merry-cli debug_writes_configured_json_log_without_changing_stdout --test debug -- --nocapture`
- `cargo test -p merry-cli sandbox_plan -- --nocapture`
- `cargo test -p merry-cli openai_debug_config_uses_xdg_toml_provider_and_secret_file -- --nocapture`
- `cargo test -p merry-cli coding_loop_live_smoke_rejects_legacy_config_flag --test debug -- --nocapture`
- `cargo test -p merry-cli`
- `cargo clippy -p merry-cli --all-targets --all-features -- -D warnings`
- `cargo fmt --all --check`
- `cargo test --all`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `git diff --check`
- scans for legacy OpenAI local config parser symbols and private-material leaks
- `git status --short`

Validation notes:

- Focused config, observability, sandbox plan, CLI log smoke, and OpenAI XDG
  provider-config tests passed during implementation.
- `cargo test -p merry-cli` passed: 51 unit tests and 35 integration tests; 2
  ignored live/bwrap smokes remain non-default.
- `cargo clippy -p merry-cli --all-targets --all-features -- -D warnings`
  passed.
- First `cargo test --all` attempt failed in the sandbox because DNS could not
  resolve `static.crates.io` for missing `futures-executor`. The command was
  rerun with approved network access, downloaded the crate, and passed all
  workspace tests. Ignored live/network/bwrap tests remained ignored.
- `cargo clippy --all-targets --all-features -- -D warnings` passed for the
  workspace.
- `cargo fmt --all --check` and `git diff --check` passed.
- Legacy parser symbols `CODING_LOOP_LIVE_SMOKE_CONFIG_PATH`,
  `LocalOpenAiConfig`, and `parse_local_openai_config` are absent from source.
  Remaining `.merry/secrets/openai.env` mentions are historical decision/plan
  context or the explicit legacy-flag rejection test.
- Private-material scan found only guardrails/ignored-path references and fake
  test secret strings such as `sk-test`; no real credentials or ignored docs
  were committed.
- Local review noted one residual path-semantics risk: arbitrary absolute
  custom log paths across host/sandbox namespaces need tightening or explicit
  documentation before broad user-facing support. The default XDG state path
  and Merry log directory mount are covered by this lease.

## Research

Research required: no

Research reason:

- The implementation plan and local repo evidence were sufficient. No external
  behavior needed lookup.

Research artifact:

- Repo inspection of CLI config loading, sandbox bootstrap, live-smoke provider
  config path, integration tests, roadmap status, and plan task requirements.

## Next Action

Next exact action:

- Continue `plans/2026-05-23-config-backed-observability.md` at Task 5:
  Runtime Loop And Process Tracing. Start with runtime trace capture tests, then
  instrument `Runtime::run_agent_loop` / step/tool execution boundaries.

Do not reconsider:

- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before observability exists.
- Do not reintroduce repo-local `.merry/secrets/openai.env` as the live-smoke
  provider config path.
