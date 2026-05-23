# Handoff

Status: rollover

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding-loop design.

Session milestone:

- Correct the previous event-first CLI direction into a configuration-backed
  observability milestone that records key runtime/tool/process/provider/
  artifact actions before adding any new interactive CLI/TUI surface.

Task queue status:

- User clarified the event CLI was the wrong center; the real need is
  observability/logging for key actions and later multi-turn debugging.
- User clarified that logging should not be controlled by new command-line
  flags. Merry should default to XDG TOML config, keep logs off by default,
  configure log enablement/level/format/path there, and mount the config
  directory in `--with-sandbox`.
- User clarified the default log file behavior: if logging is enabled and no
  path is configured, use `$XDG_STATE_HOME/merry/logs/merry.jsonl`, falling
  back to `~/.local/state/merry/logs/merry.jsonl`; failing to create/open it
  should be a clear error, not a silent stderr fallback.
- Current tracing/runtime/CLI state inspected: provider has localized tracing;
  runtime/tool/process smoke behavior still lacks a coherent log contract.
- Spec moved from `specs/2026-05-23-event-first-interactive-cli.md` to
  `specs/2026-05-23-observability-first-coding-loop.md` and rewritten.
- Spec revised to make XDG TOML config, provider/model settings, observability
  settings, and sandbox config/log mounting first-class requirements.
- Spec revised to pin the default log file path and open/create failure
  behavior.
- Roadmap updated to make observability-first logging/tracing the next
  milestone.
- Decision record corrected from Event-First CLI Before TUI to Observability
  Before Interactive CLI Or TUI.
- Continuity state updated.
- Validation: passed for planning/spec correction.
- Commit: pending.

Done condition:

- Tracked planning state consistently says the next milestone is structured
  observability/logging for the coding loop, not event-first CLI, and gives the
  next exact action as user review of the corrected observability spec.

## What Changed

Files changed:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `README.md`
- `ROADMAP.md`
- `specs/2026-05-23-observability-first-coding-loop.md`
- removed `specs/2026-05-23-event-first-interactive-cli.md`

Summary:

- Replaced the event-first CLI design with observability-first design.
- Defined configuration-backed structured logging/tracing as the next value
  slice: XDG TOML config, sandbox config mounting, runtime loop, provider
  boundary, tool calls, process execution, workspace tools, artifact recording,
  errors, cancellation, redaction, and stable correlation fields.
- Deferred event renderers, REPL, TUI, and multi-turn UI until logs make the
  current loop understandable.

## Validation

Commands run so far:

- repo inspection for current tracing/runtime event/smoke surfaces
- stale event-first planning-reference scan
- stale logging CLI flag direction scan
- XDG/TOML config direction scan
- placeholder/private-material scan for the new spec
- `git diff --check`

Result:

- Passed. Stale-reference scan found no tracked planning state that still makes
  event-first CLI the next milestone; remaining old spec path mentions are
  historical move/removal notes in continuity files. Placeholder/private-material
  scan found no unresolved placeholders or copied private source material.
  Logging flag scan found `--log-level` / `--log-format` only as explicit
  non-goals, and XDG/TOML scan confirmed the corrected config direction.

## Decisions

Decisions made:

- Treat the user's message as a requested correction to the written spec, not
  as approval to implement the old event-first CLI.
- Configuration-backed observability/logging is the milestone center; event
  JSONL and future CLI/TUI are downstream consumers.
- Keep implementation out of this lease; next lease should create an
  implementation plan for the observability slice after user review/approval.

Pending decisions:

- Exact first implementation breakdown for XDG TOML config, sandbox config/log
  mounting, config-backed subscriber setup, and runtime/tool instrumentation.
  The next lease should write this plan before code changes.

## Blockers

Blockers:

- None for the design correction.

Next exact action:

- User reviews `specs/2026-05-23-observability-first-coding-loop.md`. If
  approved, create an implementation plan for the first configuration-backed
  observability slice.

## Scope For Next Session

Allowed edits:

- Implementation plan for the observability-first coding loop.
- Rust/CLI/test files named by that implementation plan.
- Continuity file updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content.
- Full-screen TUI, REPL, or multi-turn UI before observability exists.
- Event-first CLI as the primary milestone.

Do not reconsider:

- The next proof gap is not a UI surface. It is logs/traces that explain real
  runtime behavior while the loop runs.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: pending

Message:

- project-continuity: correct next milestone to observability

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
