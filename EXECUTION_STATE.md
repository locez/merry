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

- Implement the first configurable disposable coding-loop task smoke so the
  user can run a visible task completion path, while preserving the roadmap
  guardrail against drifting back into profile-only work.

Goal:

- Add a non-default `merry --with-sandbox debug coding-loop-task-smoke` path
  that creates a disposable task fixture, runs inspect/read/patch/verify/final
  through runtime tools, and has deterministic default coverage plus an
  explicit real-bwrap smoke.

Task queue status:

- Added `debug coding-loop-task-smoke --task status-text` with deterministic
  scripted provider steps: `rg --files`, failing `rg done`, exact
  `workspace_read_file`, constrained `workspace_patch_file`, successful
  `rg done`, then final answer.
- Added `debug coding-loop-task-live-smoke --task status-text` as the
  opt-in live-provider lane using the same disposable fixture and validation
  expectations.
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

- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
- Public-safe README/roadmap/continuity updates

Done condition:

- Focused deterministic task-smoke tests pass.
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
- `cargo test -p merry-cli coding_loop_task` passes.
- `cargo test -p merry-cli sandbox_plan_mounts_runtime_paths_and_workspace`
  passes.
- `git diff --check` passes.
- Outer-environment real bwrap validation passes:
  `cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`.

## Scope

Allowed edits:

- `crates/merry-cli/src/main.rs`
- `crates/merry-cli/tests/debug.rs`
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
- `cargo test -p merry-cli coding_loop_task`
- `cargo test -p merry-cli sandbox_plan_mounts_runtime_paths_and_workspace`
- `git diff --check`
- User-run outer validation: `cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored`

Validation notes:

- All listed focused deterministic checks passed locally.
- The user reported the outer real-bwrap task smoke passed. Earlier nested
  runs from inside this agent's environment failed on `/etc`/dynamic-linker
  behavior, so they are not treated as the authoritative outer-environment
  result.

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

- Exercise the live/model task smoke path or replace the deterministic exact
  patch script with a stricter fake/live sequence that proves the model can
  infer the patch from read evidence rather than receiving exact
  `old_text`/`new_text` from the scripted provider.

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
