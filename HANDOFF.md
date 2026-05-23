# Handoff

Status: complete

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding loop.

Session milestone:

- Implement `merry-cli` Tasks 1-4 from
  `plans/2026-05-23-config-backed-observability.md`.

Task queue status:

- XDG TOML config model: completed in `crates/merry-cli/src/config.rs`.
- Config-backed file logging setup: completed in
  `crates/merry-cli/src/observability.rs` and CLI startup.
- Sandbox config/log mount planning: completed in `crates/merry-cli/src/main.rs`.
- OpenAI-compatible debug/live-smoke provider config migration from legacy
  repo-local config to XDG TOML: completed.
- Plan checkboxes updated for Tasks 1-4.
- Roadmap status updated to reflect the completed CLI config/log/provider slice
  and the next runtime tracing gap.

Done condition:

- The CLI can load optional Merry XDG config, initialize configured file logs,
  keep stdout unchanged, plan sandbox config/log mounts, and load
  OpenAI-compatible provider/model/key source from XDG TOML.

## What Changed

Files changed:

- `Cargo.toml`
- `Cargo.lock`
- `ROADMAP.md`
- `crates/merry-cli/Cargo.toml`
- `crates/merry-cli/src/config.rs`
- `crates/merry-cli/src/observability.rs`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `plans/2026-05-23-config-backed-observability.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Summary:

- Added XDG path resolution and TOML parsing for `[global]`,
  `[observability.log]`, `[providers.default]`, and
  `[providers.openai-compatible]`.
- Added config-backed tracing subscriber setup with JSON/text file logging and
  clear log path errors.
- Wired CLI startup so logging initializes after sandbox re-exec planning and
  before command execution.
- Extended sandbox planning to read config before re-exec, mount the Merry
  config directory read-only, mount the log directory read-write only when
  logging is enabled, and create the host log directory before bwrap re-exec.
- Migrated `debug openai` and `debug coding-loop-live-smoke` provider settings
  to XDG TOML; `MERRY_OPENAI_DEBUG=1` remains the network opt-in.
- Removed the live-smoke `--config` path and replaced old `KEY=value` parser
  tests with TOML/provider config tests.
- Added the explanatory code comment for `/home/merry`: those constants are
  sandbox-child paths; host `$HOME` is resolved separately before re-exec.

## Validation

Commands run:

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
- legacy parser/path scans
- private-material/secret scans

Result:

- Passed.
- The first sandboxed `cargo test --all` attempt failed only because DNS could
  not resolve `static.crates.io` for an uncached crate download. It was rerun
  with approved network access, downloaded `futures-executor`, and passed.
- Default validation remains deterministic/offline after dependency download;
  live/network/bwrap tests are still ignored/non-default.
- No real secrets or private ignored docs were added. Secret-looking matches
  are fake test strings such as `sk-test`.

## Decisions

Decisions made:

- Tasks 1-4 were committed as one lease-sized implementation slice rather than
  one commit per plan task.
- Kept runtime/process/workspace/provider tracing out of this lease. That work
  starts at Task 5.
- Kept the sandbox path constants as sandbox-child paths (`/home/merry/...`).
  Host paths are still resolved from the real host environment and mounted
  into those child paths.

Pending decisions:

- None required before Task 5.

## Blockers

Blockers:

- None.

Residual risk:

- Config-backed sandbox logging is verified for the default XDG state log path
  and the planned Merry log directory mount. Arbitrary absolute custom log
  paths across host/sandbox namespaces should be tightened or explicitly
  documented before broad user-facing support.

Next exact action:

- Start `plans/2026-05-23-config-backed-observability.md`, Task 5: Runtime
  Loop And Process Tracing. Write trace-capture tests first, then instrument
  runtime loop/step/tool execution boundaries.

## Scope For Next Session

Allowed edits:

- `crates/merry-runtime/Cargo.toml`
- `crates/merry-runtime/src/agent_loop.rs`
- `crates/merry-runtime/src/runtime.rs`
- Follow-on Task 5 test/support files if needed
- Continuity file updates

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content.
- Full-screen TUI, REPL, or multi-turn UI before observability exists.
- Reintroducing repo-local `.merry/secrets/openai.env` as the live-smoke
  provider config path.

Do not reconsider:

- The next proof gap is logs/traces that explain real runtime behavior while
  the loop runs.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed by this lease

Message:

- feat(cli): add config-backed observability

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
