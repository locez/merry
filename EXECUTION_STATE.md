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

- Add and enforce the user-facing `examples/config.toml` copy-and-edit
  contract requested by the user.

Goal:

- Provide a tracked example config that users can copy to
  `$XDG_CONFIG_HOME/merry/config.toml` or `~/.config/merry/config.toml` and
  edit only necessary values, while making future config-schema changes maintain
  that example.

Task queue status:

- Task 1, XDG TOML config model: completed.
- Task 2, config-backed log initialization: completed.
- Task 3, sandbox config/log mount planning: completed.
- Task 4, XDG provider config for OpenAI-compatible debug paths: completed.
- Task 5, runtime loop and process tracing: completed.
- Task 6, workspace tool and provider trace alignment: completed.
- Task 6A, user-facing example config contract: completed.
- `examples/config.toml` is now a tracked source-of-truth file and is parsed by
  deterministic CLI config tests.
- `AGENTS.md` now requires future accepted config-key changes to update
  `examples/config.toml` in the same change unless a reason is recorded.
- Plan/roadmap/readme/continuity state updated. Task 7 remains next.

Allowed expansion:

- Example config file and schema-backed test coverage.
- Repository maintenance rule requiring future config-key changes to keep the
  example current.
- Public-safe README, roadmap, plan, and continuity status updates.

Done condition:

- `examples/config.toml` exists, contains all currently supported user-facing
  config sections/keys, and contains no real secrets.
- A deterministic CLI config test parses `examples/config.toml` with
  `MerryConfig` and asserts expected log/provider resolution.
- Repository guidance records that future config-key changes must update the
  example config.
- Focused and relevant validation pass.
- Handoff updated and lease committed.

Drift boundary:

- Do not implement Task 7 end-to-end log-enabled smoke verification in this
  lease.
- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.

Task type: implementation/docs

Acceptance criteria:

- `examples/config.toml` documents copy destination and necessary local edits.
- `examples/config.toml` includes `[global]`, `[observability.log]`,
  `[providers.default]`, and `[providers.openai-compatible]`.
- `examples/config.toml` uses placeholders or config-relative paths only; no
  real API key, host-private endpoint, or local machine path is committed.
- `cargo test -p merry-cli example_config_toml_matches_current_schema_and_resolves_user_defaults -- --nocapture`
  proves the example parses against current schema.
- Future config schema maintenance is encoded in tracked repo instructions.

## Scope

Allowed edits:

- `AGENTS.md`
- `README.md`
- `ROADMAP.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `examples/config.toml`
- `crates/merry-cli/src/config.rs`
- `plans/2026-05-23-config-backed-observability.md`

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

- `cargo test -p merry-cli example_config_toml_matches_current_schema_and_resolves_user_defaults -- --nocapture`
- `cargo test -p merry-cli config::tests -- --nocapture`
- `cargo fmt --all --check`
- `cargo clippy -p merry-cli --all-targets --all-features -- -D warnings`
- `cargo test -p merry-cli`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- `git diff --check`
- `git status --short --untracked-files=all`

Validation notes:

- The example-config test was written first and failed to compile because
  `examples/config.toml` did not exist; it passed after adding the example.
- Validation remains deterministic/offline and does not require bwrap, network,
  or live credentials.

## Research

Research required: no

Research reason:

- The local config implementation and user request were sufficient. No external
  behavior needed lookup.

Research artifact:

- Repo inspection of `merry-cli` config parsing, README/roadmap config
  contract, and existing observability plan.

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
- Do not let future config schema changes bypass `examples/config.toml`.
