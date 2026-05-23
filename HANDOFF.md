# Handoff

Status: complete

## Current Work

Current milestone or track:

- LLM-driven sandbox coding-loop smoke.

Session milestone:

- Add an opt-in live OpenAI-compatible smoke that runs the coding-loop shape
  inside the existing `merry --with-sandbox` bwrap handoff with real LLM tool
  selection.

Task queue status:

- Runtime continuation keeps the original task visible after tool results:
  implemented.
- Ignored local OpenAI-compatible config for sandboxed live smoke:
  implemented.
- `debug coding-loop-live-smoke` CLI command using `OpenAiProvider`:
  implemented.
- Parser/gating/default-offline/ignored-live-smoke tests: implemented.
- Credentialed live LLM proof: passed in the user's trusted configured
  environment.
- Provider HTTP request `User-Agent`: implemented in this lease.
- Validation: passed.
- Commit: completed.

Done condition:

- The live smoke reaches `AgentLoopStatus::Completed` inside the CLI bwrap
  sandbox, leaves no pending tool calls, validates the patched fixture content,
  and runtime events prove real `rg --files`, exact source read,
  `workspace_patch_file`, and real `rg fixed-by-live-llm` verification calls
  chosen by the live model.

Live proof status:

- The user reported that `cargo run -p merry-cli -- --with-sandbox debug
  coding-loop-live-smoke` passed against their trusted configured server. That
  satisfies the live LLM proof lane for this milestone.
- This agent sandbox did not rerun the trusted external request because
  elevated external egress was rejected by the approval policy.
- The successful live run exposed one provider-layer gap: requests did not set
  a `User-Agent` header.

## What Changed

Files changed:

- `crates/merry-provider-openai/src/provider.rs`
- `crates/merry-provider-openai/tests/provider_stream.rs`
- `EXECUTION_STATE.md`
- `HANDOFF.md`

Summary:

- Added `User-Agent: merry/<crate version>` to OpenAI-compatible Responses
  request headers.
- Added deterministic request-construction coverage for the `User-Agent`
  header.
- Added loopback integration coverage asserting the actual HTTP request carries
  the same `User-Agent` header.
- Updated continuity state to record that the live smoke passed in the user's
  trusted configured environment.

## Validation

Commands run so far:

- `cargo test -p merry-provider-openai builds_responses_http_request_without_network`
- `cargo test -p merry-provider-openai stream_model_posts_responses_request_and_streams_events -- --ignored`
- `cargo fmt --all --check`
- `cargo clippy -p merry-provider-openai --all-targets -- -D warnings`
- `cargo test -p merry-provider-openai`
- `git diff --check`

Result:

- Request-construction test passed after first failing red test.
- Loopback ignored test passed with escalated local TCP permission.
- Formatting, provider clippy, full provider test, and diff checks passed.

## Decisions

Decisions made:

- `User-Agent` belongs in `merry-provider-openai` request construction, not in
  runtime or CLI smoke code.
- Use the crate version for the value: `merry/<crate version>`.
- Keep live LLM proof opt-in and credentialed; do not move it into default
  tests.

Pending decisions:

- None for this lease after validation.

## Blockers

Blockers:

- none

Next exact action:

- Continue the next project-continuity lease from the reusable runtime-owned
  process profile and coding-loop tool-set registration work.

## Scope For Next Session

Allowed edits:

- Follow-up fixes only if provider validation exposes a problem.
- Status updates tied directly to this lease.

Forbidden edits:

- Private raw docs.
- Real credentials.
- Broad roadmap rewrites.
- Full autonomous coding agent, Python SDK, graph memory, or live judgment
  harness.

Do not reconsider:

- Policy taxonomy is support work, not the current P0 deliverable.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: committed

Message:

- project-continuity: add openai user agent

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
