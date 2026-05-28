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

- Roadmap drift correction back to Minimal Useful Coding Loop.

Session milestone:

- User-requested planning correction: audit `ROADMAP.md` history, identify
  where profile/session work displaced the coding-loop MVP, restore the next
  step to a testable coding capability, and document roadmap change control.

Goal:

- Prevent further drift by making the next milestone a configurable
  disposable coding-loop smoke and by requiring explicit user approval before
  agents change roadmap priority or milestone ordering.

Task queue status:

- Audited `ROADMAP.md` history and identified the drift point: commit `e945ebc`
  correctly rejected a subset shell parser as authorization, but promoted shell
  boundary/profile work into `Next Active`; later M2 slices continued that
  support work.
- Designed the next task as a configurable disposable coding-loop smoke: a
  user-selectable or user-supplied small fixture task where the model must
  inspect, read exact evidence, patch through `workspace_patch_file`, run
  verification, and answer.
- Updated roadmap language so profile/session/classifier work is support for
  coding-loop acceptance, not the primary next milestone.
- Added roadmap change-control rules to `AGENTS.md` and `PROJECT_LEAD.md`.
- Recorded the drift correction in `DECISIONS.md`.

Allowed expansion:

- Public-safe roadmap, decision, lead, and continuity updates.
- No runtime implementation in this correction lease unless needed to validate
  documentation consistency.

Done condition:

- `ROADMAP.md` identifies the drift point and restores `Next Active` to a
  concrete coding-loop capability task.
- `AGENTS.md` records that agents must not unilaterally change roadmap
  priority or milestone ordering.
- `DECISIONS.md` records the drift correction and next task rationale.
- Continuity files point the next session at the configurable coding-loop smoke
  rather than profile/session design.
- Documentation validation passes and the lease is committed.

Drift boundary:

- Do not implement runtime code in this correction slice.
- Do not continue profile/session design as the next active task.
- Do not change roadmap priority beyond the user-requested correction back to
  coding-loop capability.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs`.
- Do not make live provider behavior part of default tests.

Task type: docs/planning correction

Acceptance criteria:

- `ROADMAP.md` `Next Active` names the configurable disposable coding-loop
  smoke and includes a concrete deterministic and live acceptance path.
- `ROADMAP.md` includes a drift audit naming the relevant commits.
- `AGENTS.md` and `PROJECT_LEAD.md` forbid unilateral roadmap priority changes.
- `DECISIONS.md` records why the roadmap is corrected.
- `cargo fmt --all --check` and `git diff --check` pass.

## Scope

Allowed edits:

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `README.md`
- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content
- runtime implementation files
- profile/session implementation
- full-screen TUI, REPL, or multi-turn UI scope

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `cargo fmt --all --check`
- `git diff --check`

Validation notes:

- Validation passed for this correction lease.

## Research

Research required: no

Research reason:

- User explicitly requested local `ROADMAP.md` history review before correction.

Research artifact:

- None.

## Next Action

Next exact action:

- Implement the configurable disposable coding-loop smoke. The first slice
  should expose a non-default CLI/debug command or ignored integration test that
  creates/selects a tiny fixture task, gives the model a natural task
  description without exact `old_text`/`new_text`, requires
  inspect/read/patch/verify/final-answer through runtime tools, and keeps live
  provider behavior explicit opt-in.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Do not make a Merry-owned subset shell parser the authorization model.
- Do not merge `process.shell.read_only.v1` into `process.read_only.v1`.
- Do not make event-first CLI the primary next milestone.
- Do not make profile/session design the next milestone unless the coding-loop
  task is blocked by it and the user explicitly approves the priority change.
- Do not start TUI or REPL before the coding-loop proof is more user-testable.
- Do not move private Codex/raw-doc findings into tracked source text.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
