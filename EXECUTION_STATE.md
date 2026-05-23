# Execution State

Lease status: rollover

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

Level: implementation-plan-ready

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

- Convert the user-approved observability spec into a tracked implementation
  plan with concrete files, tests, sandbox behavior, config behavior, and
  execution handoff.

Goal:

- Make Merry's already-proven live coding loop diagnosable while it runs. The
  next implementation should let an operator enable logs and understand what
  happened, which tool ran, which artifact was recorded, and why a loop
  completed, failed, blocked, or cancelled.

Task queue status:

- User approved the corrected observability-first spec: completed.
- Write a tracked implementation plan for XDG TOML config, sandbox config/log
  mounting, config-backed tracing setup, and runtime/tool/process/provider
  instrumentation: completed.
- Record the credential-source decision needed for sandboxed live smoke:
  completed.
- Update continuity state and handoff: completed.
- Validate and commit: in progress.

Allowed expansion:

- Public-safe tracked implementation plan and continuity updates.
- Decision record for the implementation-plan credential source.
- No Rust implementation in this lease.

Done condition:

- `plans/2026-05-23-config-backed-observability.md` exists, maps the approved
  spec to executable tasks and tests, records no private raw-doc material, and
  the next exact action is choosing execution mode.

Drift boundary:

- Do not implement tracing/logging/config code in this lease.
- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.

Task type: implementation planning

Acceptance criteria:

- Plan starts from XDG TOML config, not new root logging flags.
- Plan preserves stdout/log separation.
- Plan specifies default log path behavior:
  `$XDG_STATE_HOME/merry/logs/merry.jsonl`, falling back to
  `~/.local/state/merry/logs/merry.jsonl`.
- Plan specifies sandbox config read-only mount and log directory read-write
  mount only when file logging is enabled.
- Plan specifies deterministic tests for config parsing, log path failures,
  sandbox mount planning, subscriber setup, runtime/process/workspace/provider
  trace capture, and redaction.
- Plan keeps live provider and bwrap checks opt-in.

## Scope

Allowed edits:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `plans/2026-05-23-config-backed-observability.md`

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

- `rg` for unresolved placeholders in the implementation plan
- `rg` for forbidden root logging-flag direction in the implementation plan
- `rg` for private-material leakage paths in tracked changes
- `git diff --check`

Validation notes:

- Placeholder scan found no unresolved plan placeholders.
- Root logging flag scan found `--log-level` / `--log-format` only in
  explicit non-goal text and the plan's "does not add" checklist.
- Private-material scan found no copied private docs. Matches are expected
  source-of-truth guardrails, ignored-path references, and fake test secret
  strings such as `sk-test`.
- XDG/TOML/tracing scan confirmed the implementation plan covers config
  discovery, default log path behavior, sandbox mounts, and runtime/process
  trace points.
- `git diff --check` passed for tracked changes; the new plan file had no
  whitespace check output when checked as a new file.

## Research

Research required: no

Research reason:

- The user approved the local spec. Repo inspection was sufficient to map the
  implementation plan to current files and tests.

Research artifact:

- Repo inspection of CLI sandbox bootstrap, live smoke config path, runtime
  agent loop, process tool execution path, workspace tool executors, provider
  tracing, and current test structure.

## Next Action

Next exact action:

- Ask the user to choose execution mode for
  `plans/2026-05-23-config-backed-observability.md`:
  1. Subagent-Driven (recommended)
  2. Inline Execution

Do not reconsider:

- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before observability exists.
- Do not require live provider tests in default `cargo test`.
