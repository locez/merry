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

- Repair continuity state after the committed config-backed observability
  implementation plan was still recorded as commit-pending in the handoff.

Goal:

- Make durable project state match the current git evidence so the next lease
  can start implementation of configuration-backed observability without
  reopening the completed planning work.

Task queue status:

- User approved the corrected observability-first spec: completed.
- Write a tracked implementation plan for XDG TOML config, sandbox config/log
  mounting, config-backed tracing setup, and runtime/tool/process/provider
  instrumentation: completed.
- Record the credential-source decision needed for sandboxed live smoke:
  completed.
- Update continuity state and handoff: completed.
- Validate and commit planning lease: completed in commit `7694561
  project-continuity: plan config-backed observability`.
- Repair stale continuity state that still said the planning commit was pending:
  completed.

Allowed expansion:

- Continuity-state repair only.
- No Rust implementation in this lease.

Done condition:

- `plans/2026-05-23-config-backed-observability.md` exists, maps the approved
  spec to executable tasks and tests, records no private raw-doc material, and
  the committed planning lease is reflected accurately in `EXECUTION_STATE.md`
  and `HANDOFF.md`.

Drift boundary:

- Do not implement tracing/logging/config code in this lease.
- Do not add TUI, REPL, or interactive CLI scope.
- Do not move private ignored notes into tracked files.
- Do not commit `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/`.

Task type: repair

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
- `git status --short`
- `git log --oneline -5`

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
- Current repair inspection confirmed the worktree was clean before repair and
  commit `7694561 project-continuity: plan config-backed observability` contains
  the implementation plan, decision record, and prior continuity updates.

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

- Start implementation from
  `plans/2026-05-23-config-backed-observability.md`, Task 1: XDG TOML Config
  Model. Use `superpowers:subagent-driven-development` unless the user
  explicitly chooses inline execution.

Do not reconsider:

- Do not make event-first CLI the primary next milestone.
- Do not start TUI or REPL before observability exists.
- Do not require live provider tests in default `cargo test`.
