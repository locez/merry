# Handoff

Status: complete

## Current Work

Current milestone or track:

- Configuration-backed observability-first coding loop.

Session milestone:

- Implement Task 6 from `plans/2026-05-23-config-backed-observability.md`:
  workspace tool and OpenAI-compatible provider trace alignment.

Task queue status:

- Tasks 1-5 remain complete from prior observability slices.
- Task 6 is complete: workspace read/list/search/patch and
  OpenAI-compatible provider request metadata now emit safe structured traces.
- Plan checkboxes updated for Tasks 1-6.
- Roadmap status updated to reflect completed workspace/provider trace
  alignment and the remaining Task 7 log-enabled smoke verification gap.

Done condition:

- Workspace/provider traces expose correlation and status metadata without
  logging file contents, raw search queries, patch text, invalid argument
  payloads, provider wire payloads, prompts, API keys, or response payloads.

## What Changed

Files changed:

- `Cargo.lock`
- `ROADMAP.md`
- `crates/merry-provider-openai/src/provider.rs`
- `crates/merry-tool-workspace/Cargo.toml`
- `crates/merry-tool-workspace/src/lib.rs`
- `plans/2026-05-23-config-backed-observability.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Summary:

- Added `tracing` and `tracing-subscriber` coverage for
  `merry-tool-workspace`.
- Added workspace tool `runtime.workspace_tool.start` and
  `runtime.workspace_tool.finish` traces for read/list/search/patch.
- Added bounded path summaries, search `query_bytes`, patch preimage and
  replacement byte counts, outcome byte counts, status labels, and diagnostic
  codes.
- Added payload-free invalid-argument traces so parse failures still produce a
  failed tool trace without logging raw bad arguments.
- Added provider `runtime.provider.request` metadata event and separate
  `runtime.provider.stream` span with safe provider/model/request fields.
- Added deterministic trace-capture and redaction tests for workspace tools and
  provider metadata/render paths.
- Incorporated reviewer feedback for cancelled/error finish traces, bounded path
  summaries, provider span/event naming, render-path coverage, and
  invalid-argument trace coverage.

## Validation

Commands run:

- `cargo test -p merry-tool-workspace workspace_tool_invalid_arguments_trace_failed_without_payload -- --nocapture`
- `cargo test -p merry-tool-workspace workspace_ -- --nocapture`
- `cargo test -p merry-provider-openai provider_ -- --nocapture`
- `cargo test -p merry-tool-workspace`
- `cargo test -p merry-provider-openai`
- `cargo clippy -p merry-tool-workspace --all-targets --all-features -- -D warnings`
- `cargo clippy -p merry-provider-openai --all-targets --all-features -- -D warnings`
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`
- `git diff --check`

Result:

- Passed.
- The invalid-arguments regression test failed first because parse failures
  emitted no workspace tool trace; it passed after the payload-free trace fix.
- Ignored live/network/bwrap tests remained ignored/non-default.
- No private ignored docs, credentials, or generated build artifacts were added.

## Decisions

Decisions made:

- Invalid workspace arguments emit tool-only start/finish traces with
  `workspace_invalid_arguments`; traces intentionally do not include invalid
  argument payload fields.
- Workspace trace summaries are bounded to 96 characters for path-like fields.
- Search traces record query byte count, not query text.
- Patch traces record path plus byte counts, not old/new patch text or file
  contents.
- Provider request metadata is an event named `runtime.provider.request`; the
  provider stream setup span is `runtime.provider.stream` to avoid conflating
  span setup with rendered-request metadata.
- Workspace executors do not log artifact IDs directly because artifact IDs are
  runtime-owned; Task 5 runtime tool resolution traces already record tool
  artifact IDs once the runtime records the outcome.

Pending decisions:

- None required before Task 7.

## Blockers

Blockers:

- None.

Residual risk:

- Task 6 has deterministic unit/package/workspace coverage, but the combined
  smoke log is not asserted yet. Task 7 should verify the existing coding-loop
  smoke writes the expected config-backed JSON log records end to end.

Next exact action:

- Start `plans/2026-05-23-config-backed-observability.md`, Task 7:
  End-To-End Log-Enabled Smoke Verification. Add a deterministic CLI log smoke
  with XDG TOML observability enabled and assert the log contains runtime loop,
  provider request, workspace tool, process execution, artifact/tool resolution,
  diagnostic, and final status records without secrets or raw payload contents.

## Scope For Next Session

Allowed edits:

- `crates/merry-cli/tests/debug.rs`
- `README.md` only if implemented command behavior changes public usage text
- Follow-on Task 7 test/support files if needed
- Continuity file updates

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content.
- Full-screen TUI, REPL, or multi-turn UI before observability exists.
- Reintroducing repo-local `.merry/secrets/openai.env` as the live-smoke
  provider config path.

Do not reconsider:

- The next proof gap is log-enabled smoke verification on top of completed
  config/log, runtime/process trace, and workspace/provider trace slices.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed by this lease

Message:

- feat: trace workspace tools and provider metadata

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
