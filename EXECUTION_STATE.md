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

- Minimal Useful Coding Loop.

Session milestone:

- Fix sandboxed live task smoke opt-in propagation so the user's
  `MERRY_OPENAI_DEBUG=1 merry --with-sandbox debug coding-loop-task-live-smoke`
  command reaches the live debug path, then correct the exposed
  OpenAI-compatible config schema to use plain `api_key`/`api_key_file`
  sources instead of the rejected `api_key_env` design.

Goal:

- Preserve only the non-secret `MERRY_OPENAI_DEBUG=1` marker across sandbox
  self-reexec, while keeping API keys out of bwrap argv and generic environment
  inheritance.
- Treat `config.toml` as the credential source of truth: require exactly one of
  `[providers.openai-compatible].api_key` or `api_key_file`.

Task queue status:

- Added `debug coding-loop-task-smoke --task status-text` with deterministic
  scripted provider steps: `rg --files`, failing `rg done`, exact
  `workspace_read_file`, constrained `workspace_patch_file`, successful
  `rg done`, then final answer.
- Added `debug coding-loop-task-live-smoke --task status-text` as the
  opt-in live-provider lane using the same disposable fixture and validation
  expectations.
- Fixed sandbox self-reexec to pass `MERRY_OPENAI_DEBUG=1` into the bwrap
  child only when the outer value is exactly `1`; non-opt-in values and API key
  environment variables remain excluded.
- Added sandbox-plan regression tests for preserving the live-debug opt-in and
  rejecting non-opt-in values.
- Replaced `api_key_env` config support with plain `api_key` and strict
  `api_key`/`api_key_file` exclusivity, per user correction.
- Updated `examples/config.toml`, README, and ROADMAP so public config docs no
  longer describe environment-based credential priority.
- Added CLI tests for help output, sandbox-required usage behavior, clap
  parsing, deterministic fake-runner task completion, and ignored real-bwrap
  task smoke paths.
- Corrected the bwrap `/etc` mount construction to match the user's existing
  helper semantics: file and directory allowlist paths create mount target
  parents first, then use direct read-only bind. No whole-`/etc` bind,
  no staged copy, and no `LD_LIBRARY_PATH` fallback were kept.
- Updated `README.md` and `ROADMAP.md` to describe the bwrap file/directory
  helper semantics and record the new task smoke status.

Allowed expansion:

- `crates/merry-cli/src/config.rs`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `examples/config.toml`
- Public-safe README/roadmap/continuity updates

Done condition:

- Focused deterministic task-smoke tests pass.
- Sandbox plan tests prove live-debug opt-in propagation without secret env
  propagation.
- Config tests prove `api_key` and `api_key_file` are exclusive, redact inline
  secrets, and reject blank/control-character keys before provider setup.
- `target/debug/merry` is rebuilt so the user's direct command uses the fix.
- Sandbox plan tests assert file helper semantics instead of broad `/etc`
  binding.
- User can run the real-bwrap ignored task smoke from an outer environment.
- Continuity files point the next session at live/model coding capability,
  not profile/session design.
- Changes are committed.

Drift boundary:

- Do not broaden process admission to generic `cargo check`.
- Do not mount the whole host `/etc`.
- Do not add `LD_LIBRARY_PATH` or staged copy fallbacks without explicit user
  approval.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs`.
- Do not make live provider behavior part of default tests.

Task type: runtime/CLI implementation

Acceptance criteria:

- `cargo fmt --all --check` passes.
- `cargo build -p merry-cli` passes.
- `cargo test -p merry-cli sandbox_plan_preserves_openai_debug_opt_in_without_secret_env`
  passes.
- `cargo test -p merry-cli sandbox_plan_does_not_preserve_non_opt_in_openai_debug_values`
  passes.
- `cargo test -p merry-cli config::tests` passes.
- `cargo test -p merry-cli debug_openai` passes.
- `cargo test -p merry-cli coding_loop_task` passes.
- `cargo test -p merry-cli sandbox_plan_mounts_runtime_paths_and_workspace`
  passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.
- `cargo test --all` passes.
- `git diff --check` passes.
- Outer-environment real bwrap validation passes:
  `cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`.

## Scope

Allowed edits:

- `crates/merry-cli/src/config.rs`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `examples/config.toml`
- `README.md`
- `ROADMAP.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content
- process-admission broadening unrelated to the task smoke
- profile/session implementation
- full-screen TUI, REPL, or multi-turn UI scope

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`
- `AGENTS.md`
- `PROJECT_LEAD.md`

## Validation

Validation command:

- `cargo fmt --all --check`
- `cargo build -p merry-cli`
- `cargo test -p merry-cli sandbox_plan_preserves_openai_debug_opt_in_without_secret_env`
- `cargo test -p merry-cli sandbox_plan_does_not_preserve_non_opt_in_openai_debug_values`
- `cargo test -p merry-cli config::tests`
- `cargo test -p merry-cli debug_openai`
- `cargo test -p merry-cli coding_loop_task`
- `cargo test -p merry-cli sandbox_plan_mounts_runtime_paths_and_workspace`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- `git diff --check`
- User-run outer validation: `cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`

Validation notes:

- Focused live-debug opt-in and deterministic task-smoke checks passed
  locally for the current change.
- Focused config and debug OpenAI tests passed locally for the current
  `api_key`/`api_key_file` schema correction.
- Full clippy and test suite passed locally for the current change.
- The user reported the outer real-bwrap task smoke passed. Earlier nested
  runs from inside this agent's environment failed on `/etc`/dynamic-linker
  behavior, so they are not treated as the authoritative outer-environment
  result.
- A local no-network attempt to run the live task smoke from this nested agent
  environment still hit the known second-level bwrap `/etc/ld.so.cache` bind
  limitation before reaching the child. The fix is therefore verified by the
  sandbox plan regression tests and must be live-checked from the user's outer
  environment.

## Research

Research required: yes

Research reason:

- User asked to compare behavior against Codex/local bwrap scripts and then
  supplied prior helper snippets. The implementation decision was whether to
  broaden `/etc`, stage-copy special files, add `LD_LIBRARY_PATH`, or preserve
  direct file bind semantics.

Research artifact:

- Local code evidence from `.merry/codex/codex-rs/linux-sandbox/src/bwrap.rs`
  and the user's prior shell helper snippets. No private raw findings were
  copied into tracked source beyond the public-safe helper behavior summary.

## Next Action

Next exact action:

- From the outer environment, rerun:
  `MERRY_OPENAI_DEBUG=1 ./target/debug/merry --with-sandbox debug coding-loop-task-live-smoke --task status-text`.
  It should now retain the live-debug opt-in inside bwrap; remaining failures,
  if any, should be real config/network/model behavior rather than immediate
  opt-in usage/help.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Do not make a Merry-owned subset shell parser the authorization model.
- Do not merge `process.shell.read_only.v1` into `process.read_only.v1`.
- Do not make profile/session design the next milestone unless the coding-loop
  task is blocked by it and the user explicitly approves the priority change.
- Do not mount all of `/etc`, add `LD_LIBRARY_PATH`, or add staged copy
  fallbacks without explicit approval.
- Do not start TUI or REPL before the coding-loop proof is more user-testable.
- Do not move private Codex/raw-doc findings into tracked source text.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
