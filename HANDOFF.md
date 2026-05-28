# Handoff

Status: complete

## Current Work

Current milestone or track:

- Minimal Useful Coding Loop.

Session milestone:

- Fix the sandboxed live task smoke opt-in propagation bug, then correct the
  exposed OpenAI-compatible config schema after the user rejected
  `api_key_env`.

Task queue status:

- Added `merry --with-sandbox debug coding-loop-task-smoke --task status-text`.
- Added `merry --with-sandbox debug coding-loop-task-live-smoke --task status-text`.
- Added deterministic fake-provider/fake-runner coverage for inspect ->
  failing verification -> read -> patch -> successful verification -> final.
- Added ignored real-bwrap task smoke tests for scripted and live paths.
- Fixed sandbox self-reexec so `MERRY_OPENAI_DEBUG=1` survives into the bwrap
  child as a non-secret opt-in marker; non-`1` values and API key env vars are
  not propagated.
- Added sandbox-plan regression tests for the opt-in propagation behavior.
- Replaced `api_key_env` config support with plain `api_key`; config now
  requires exactly one of `api_key` or `api_key_file`.
- Updated `examples/config.toml`, README, and ROADMAP to remove
  environment-based credential priority from the public config contract.
- Fixed bwrap `/etc` planning to use file/directory helper semantics:
  create mount parents, then direct `--ro-bind`; no broad `/etc` bind, no
  staged copy fallback, and no `LD_LIBRARY_PATH`.
- Updated README/ROADMAP status and verification commands.

Done condition:

- The task smoke is now runnable/testable and advances the active coding-loop
  MVP capability rather than profile-only work.

## What Changed

Files changed:

- `README.md`
- `ROADMAP.md`
- `examples/config.toml`
- `crates/merry-cli/src/config.rs`
- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Summary:

- New CLI debug task smoke creates a disposable fixture and proves runtime
  read/patch/process verification flow.
- Sandboxed live task smoke no longer loses `MERRY_OPENAI_DEBUG=1` during
  bwrap re-exec.
- OpenAI-compatible credentials are now config-owned via `api_key` or
  `api_key_file`, with no `api_key_env` priority path.
- Sandbox plan now mirrors the user's no-copy helper semantics for `/etc`
  file/dir mounts.
- The real bwrap task smoke was confirmed by the user from an outer
  environment.

## Validation

Commands run:

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

User-run validation:

- `cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`

Result:

- Focused deterministic checks passed locally.
- Full clippy and test suite passed locally.
- `target/debug/merry` was rebuilt with the opt-in propagation fix.
- User-run outer real-bwrap task smoke passed.

## Decisions

Decisions made:

- Keep `/etc` mounting as explicit file/dir helper binds; do not mount all of
  `/etc`.
- Do not add `LD_LIBRARY_PATH`.
- Do not stage-copy `/etc/ld.so.cache` or `/etc/resolv.conf`; the outer
  environment passes with direct bind semantics.
- Keep the current task verification on `rg done` so no process-admission
  broadening is needed.
- Preserve only the exact `MERRY_OPENAI_DEBUG=1` marker across sandbox
  re-exec; keep credentials in XDG config/key files rather than argv or broad
  environment inheritance.
- Treat `config.toml` as the credential source of truth; `api_key` and
  `api_key_file` are mutually exclusive and there is no implicit env/file
  priority.

Pending decisions:

- Whether the next slice should test the live model solving the task, or first
  make the deterministic fake-provider path less scripted by withholding exact
  patch text from the provider script.

## Blockers

Blockers:

- None.

Residual risk:

- The deterministic scripted provider still supplies exact patch arguments.
  This proves the runtime/tool/sandbox path, but not yet real coding
  intelligence.
- The live task smoke is opt-in and depends on local OpenAI-compatible config.

Next exact action:

- Rerun
  `MERRY_OPENAI_DEBUG=1 ./target/debug/merry --with-sandbox debug coding-loop-task-live-smoke --task status-text`
  from the outer environment. It should now retain the live-debug opt-in
  inside bwrap; any remaining failure should be config/network/model behavior,
  not immediate usage/help from a missing opt-in.

## Scope For Next Session

Allowed edits:

- `crates/merry-cli/src/main.rs` and `crates/merry-cli/tests/debug.rs` for
  task smoke tightening.
- Existing runtime/tool crates only if the coding-loop acceptance command
  exposes a concrete blocker.
- Public-safe roadmap/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content.
- Roadmap priority changes without explicit user approval.
- Broad model-facing shell tool or approval/session implementation.
- A Merry-owned subset shell parser as the authorization model.
- Full-screen TUI, REPL, or multi-turn UI.
- Broad `/etc` bind, `LD_LIBRARY_PATH`, or staged-copy sandbox fallback unless
  the user explicitly approves it.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Shell compatibility must use real shell execution under explicit profiles;
  do not revive parser-first M2.
- `process.shell.read_only.v1` must stay distinct from `process.read_only.v1`.
- Profile/session design is not the next active milestone unless it blocks the
  coding-loop smoke and the user explicitly approves the priority change.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: pending

Message:

- fix(cli): simplify OpenAI debug credential config

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
