# Execution State

Lease status: rollover

## Source Of Truth

- `AGENTS.md`
- `PROJECT_LEAD.md`
- `ROADMAP.md`
- `README.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- `docs/design/mvp-design.md` (ignored private local design source)
- `docs/design/global-design.md` (ignored private local design source)
- `docs/product/product-strategy.md` (ignored private local product source)
- `merry-raw-docs/` (ignored original local source material; do not commit)

## Planning Maturity

Level: structured-roadmap

Current planning artifact:

- `ROADMAP.md`
- `DECISIONS.md`
- `specs/2026-05-23-observability-first-coding-loop.md`

## Communication

Language: Chinese

Style notes:

- Keep user-facing updates concise and direct.
- Preserve Rust/API/config names in English.

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding-loop design.

Session milestone:

- Correct the previous event-first CLI direction into a configuration-backed
  observability milestone: XDG TOML config, sandbox config mounting, and
  structured logs/traces at runtime, tool, process, provider, artifact,
  sandbox, and loop boundaries before any new interactive CLI/TUI.

Goal:

- Make Merry's already-proven live coding loop diagnosable while it runs. The
  next implementation should let an operator enable logs and understand what
  happened, which tool ran, which artifact was recorded, and why a loop
  completed, failed, blocked, or cancelled.

Task queue status:

- User clarified that event CLI is not the real need; the real need is
  observability/logging for key actions and multi-turn debugging: completed.
- User clarified that logging should be configured through XDG TOML config, not
  new `--log-level` / `--log-format` flags, and that `--with-sandbox` should
  mount the Merry config directory: completed.
- User clarified that when logging is enabled but no path is configured, the
  default should be `$XDG_STATE_HOME/merry/logs/merry.jsonl`, falling back to
  `~/.local/state/merry/logs/merry.jsonl`, with clear failure on open/create
  errors: completed.
- Use Locez Lens to reframe the issue from "render runtime events" to "make
  action boundaries observable and correlated": completed.
- Inspect existing tracing/runtime/CLI state: completed. Provider has localized
  `tracing`; runtime/tool/process smoke path lacks a coherent log contract.
- Move and rewrite spec from event-first CLI to observability-first coding
  loop: completed.
- Revise observability spec to make XDG TOML config and sandbox config mounting
  part of the milestone: completed.
- Update roadmap and decision record away from event-first CLI: completed.
- Update continuity state and handoff: in progress.
- Validate and commit: pending.

Allowed expansion:

- Public-safe tracked planning/spec updates for the corrected milestone.
- Continuity updates.
- No Rust implementation in this lease.

Done condition:

- Public tracked state consistently names observability-first logging/tracing as
  the next milestone, names XDG TOML config and sandbox config mounting as
  first-class requirements, the old event-first CLI spec path is gone, and the
  next exact action is user review of the corrected observability spec.

Drift boundary:

- Do not implement tracing/logging code in this lease.
- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.

Task type: planning/design correction

Acceptance criteria:

- Spec names logs/traces as the milestone center, not event CLI.
- Spec defines XDG config discovery, TOML provider/model/log settings, sandbox
  config mounting, stable correlation fields, action boundaries, redaction,
  default log path behavior, tests, and live/manual smoke expectations.
- Roadmap and decisions no longer point to event-first CLI as the next
  milestone.
- Continuity handoff gives the next exact action.

## Scope

Allowed edits:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `ROADMAP.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- removal of `specs/2026-05-23-event-first-interactive-cli.md`

Forbidden edits:

- Rust source implementation
- private ignored source material under `docs/` or `merry-raw-docs/`
- real credentials or generated build artifacts
- `.superpowers/` visual companion artifacts

Protected files:

- `SESSION_RUNBOOK.md`
- `AGENT_ROLES.md`
- `ROADMAP.md`
- `CHANGE_REQUESTS.md`

## Validation

Validation command:

- `rg` for stale event-first milestone references in tracked planning/spec files
- `rg` for stale logging CLI flag direction in tracked planning/spec files
- `rg` for XDG/TOML config references in tracked planning/spec files
- `rg` for placeholders/private-material leakage in the new spec
- `git diff --check`

Validation notes:

- Stale-reference scan found no tracked planning state that still makes the
  event-first CLI the next milestone. Remaining old spec path mentions are
  historical move/removal notes in continuity files.
- Placeholder/private-material scan found no unresolved placeholders or copied
  private source material. Matches are expected guardrail/config examples.
- Stale logging flag scan found `--log-level` / `--log-format` only in
  explicit non-goal text.
- XDG/TOML scan confirmed config discovery, log settings, and sandbox config
  mounting are represented in spec, roadmap, README, decisions, and continuity.
- `git diff --check` passed.

## Research

Research required: no

Research reason:

- The user supplied the product correction. Repo inspection was sufficient to
  confirm the implementation direction: `tracing` already exists in workspace
  dependencies and provider code, but runtime/tool/process logging is not
  consistently instrumented.

Research artifact:

- Repo inspection of current `tracing` usage, runtime events, smoke commands,
  and `ok` output behavior.

## Next Action

Next exact action:

- User reviews `specs/2026-05-23-observability-first-coding-loop.md`. If
  approved, the next lease should create an implementation plan for the first
  observability slice: XDG TOML config discovery, sandbox config/log path
  mounting, config-backed tracing setup, and deterministic config/tracing tests.

Do not reconsider:

- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before observability exists.
- Do not require live provider tests in default `cargo test`.
