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

- M2 Shell-Compatible Runtime Boundary.

Session milestone:

- Direction correction slice: prevent the next milestone from becoming a
  hand-rolled subset shell parser and reframe shell compatibility around real
  shell execution under explicit permission/session profiles.

Goal:

- Capture the corrected M2 boundary in tracked docs before implementation:
  structured argv remains the narrow typed lane, while shell-compatible
  behavior must use a real shell runner under runtime-owned profiles,
  artifacts, audit, cancellation, ledger reducers, and approvals.

Task queue status:

- Reframed `ROADMAP.md` M2 away from "parse simple shell strings into argv" and
  toward a shell-compatible runtime boundary.
- Added roadmap invariants that Merry must not build a subset shell parser as
  authorization, and that pipes/control flow are legitimate shell mechanisms.
- Recorded the decision in `DECISIONS.md` that static classifiers are hard-deny
  or advisory evidence only, not broad shell authorization.
- Updated `HANDOFF.md` to make the next action a design/implementation slice
  for shell profile/session admission rather than command-string parsing.

Allowed expansion:

- Public-safe roadmap/decision/continuity updates.
- No Rust behavior changes in this correction lease unless needed to keep docs
  consistent.

Done condition:

- `ROADMAP.md`, `DECISIONS.md`, `EXECUTION_STATE.md`, and `HANDOFF.md` all
  state that shell compatibility must not depend on a Merry-owned subset parser
  allowlist.
- The next exact action points at defining the shell runtime boundary and
  profile/session admission contract.
- Diff is documentation-only, passes `git diff --check`, and is committed.

Drift boundary:

- Do not add TUI, REPL, or interactive CLI scope.
- Do not implement shell string parsing or pipelines in this correction lease.
- Do not claim shell execution is safe because a classifier recognizes a string
  shape.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs`.
- Do not make live provider behavior part of default tests.
- Do not expand this slice into approval/session implementation.

Task type: docs

Acceptance criteria:

- Roadmap M2 is renamed/reframed as a shell-compatible runtime boundary.
- Decision log records why subset shell parsing is not the authorization model.
- Handoff next action is implementation-neutral enough to avoid parser-first
  drift.
- `git diff --check` passes.

## Scope

Allowed edits:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`

Forbidden edits:

- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs` content
- Rust behavior changes
- shell-compatible model tool implementation, pipelines, scripts, or approval
  session behavior
- full-screen TUI, REPL, or multi-turn UI scope

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `git diff --check`
- `git status --short --untracked-files=all`

Validation notes:

- This is a docs-only direction correction. Rust validation from commit
  `efd2d99` remains the last full code validation. No new Rust behavior is
  introduced in this lease.

## Research

Research required: no

Research reason:

- This lease used existing roadmap/status/code evidence. No internet or ignored
  private source research was needed.

Research artifact:

- None.

## Next Action

Next exact action:

- Start M2 from `ROADMAP.md`: define the first shell-compatible runtime
  boundary and acceptance test around artifact-backed command/script input,
  explicit permission/session admission, runner cancellation, output artifacts,
  compact ledger reduction, and payload-free traces. Do not start by splitting
  shell strings into argv allowlists.

Do not reconsider:

- M1 Structured Process Boundary MVP is complete.
- Do not make a Merry-owned subset shell parser the authorization model.
- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before reusable runtime/process/tool profiles are
  clearer.
- Do not move private Codex/raw-doc findings into tracked source text.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
