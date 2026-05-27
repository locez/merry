# Handoff

Status: complete

## Current Work

Current milestone or track:

- M1 Structured Process Boundary MVP.

Session milestone:

- Context/cache instrumentation slice: stable provider-neutral request prefix
  metadata includes runtime-owned base instructions and tool profile, while
  compiled context, user input, and continuations remain dynamic.

Task queue status:

- Completed this slice. `ModelRequest` now records
  `stable_prefix_message_count`, `tool_profile_hash`, `stable_prefix_hash`, and
  `dynamic_context_hash`.
- Runtime request compilation now inserts a minimal stable base system message
  before dynamic compiled context and user input.
- Runtime provider request traces include the new hashes and stable prefix
  message count without prompt text or provider wire payloads.

Done condition:

- Focused and full validation passed, status files are updated, and this lease
  is ready to commit.

## What Changed

Files changed:

- `DECISIONS.md`
- `EXECUTION_STATE.md`
- `HANDOFF.md`
- `crates/merry-llm/src/lib.rs`
- `crates/merry-llm/src/request.rs`
- `crates/merry-llm/tests/protocol.rs`
- `crates/merry-runtime/src/runtime.rs`
- `crates/merry-runtime/src/step.rs`
- `crates/merry-runtime/tests/agent_loop.rs`
- `crates/merry-runtime/tests/provider_boundary.rs`
- `crates/merry-tool-workspace/tests/runtime_integration.rs`

Summary:

- Added `RequestContentHash` and explicit stable-prefix metadata to
  `merry-llm::ModelRequest`.
- Added `ModelRequest::new_with_continuations_and_stable_prefix` for callers
  that know the runtime-owned stable prefix boundary.
- Validated that stable prefix messages must be leading system messages.
- Hashed stable prefix content from leading base/system messages plus
  canonicalized tool specs; dynamic context hash covers the remaining messages
  and ordered tool continuations.
- Runtime provider request compilation now adds `You are Merry.` as a minimal
  stable base instruction message and marks exactly that one message as the
  stable prefix.
- Runtime provider request tracing now records `stable_prefix_message_count`,
  `tool_profile_hash`, `stable_prefix_hash`, and `dynamic_context_hash`.
- Updated runtime, agent-loop, and workspace integration tests for the new
  base-message position.
- Recorded the decision that base instructions are part of the cacheable stable
  prefix, while ledger/evidence/user context remains dynamic.

## Validation

Commands run:

- `cargo test -p merry-llm --test protocol stable_prefix`
- `cargo test -p merry-runtime --test provider_boundary stable_prefix`
- `cargo test -p merry-runtime --test provider_boundary`
- `cargo test -p merry-runtime --test agent_loop`
- `cargo test -p merry-llm --test protocol`
- `cargo test -p merry-runtime --lib`
- `cargo test -p merry-tool-workspace --test runtime_integration coding_loop_harness_inspects_patches_verifies_and_completes`
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all`

Result:

- Passed.
- The first full `cargo test --all` pass failed only on message-index
  assertions that predated the base instruction message; those were updated and
  the full suite passed.
- Live provider and real `bwrap` smokes were not run for this slice.

## Decisions

Decisions made:

- Stable prefix hash includes runtime-owned base instructions and the
  model-visible tool profile.
- Dynamic context hash owns compiled context, current user input, and ordered
  tool continuations.
- The initial default base instruction is intentionally minimal; the contract is
  the stable prefix boundary, not a finalized long-term prompt.

Pending decisions:

- Exact permission-profile id surface and stable-prefix change-reason event or
  metadata shape.
- How far M1 should take process artifact reducers before adding the
  shell-compatible model tool in M2.

## Blockers

Blockers:

- None.

Residual risk:

- Changing the default base instruction text later will deliberately change
  `stable_prefix_hash`. That should be treated as a cache-lane change, not a
  dynamic context change.
- The current default base instruction is too small for long-term behavior
  steering; it exists to establish the runtime-owned prefix boundary.

Next exact action:

- Continue M1 by routing classified process intents through read-only and
  workspace-write/sandbox permission profiles, starting with known read-only
  inspection commands and local workspace verification commands.

## Scope For Next Session

Allowed edits:

- Runtime process classifier/profile/admission modules and focused tests.
- Runtime/provider-neutral metadata only if needed for permission profile id or
  prefix-change reasons.
- Existing CLI/debug smoke wiring only as needed to consume the runtime-owned
  process boundary.
- Public-safe roadmap/decision/continuity updates.

Forbidden edits:

- Private raw docs.
- Real credentials.
- `.superpowers/`, `.merry/`, `docs/`, or `merry-raw-docs/` content.
- Full-screen TUI, REPL, or multi-turn UI before reusable runtime/tool
  registration is clearer.

Do not reconsider:

- Observability-first Task 7 is complete.
- Base instructions are included in the stable prefix cache boundary.
- Dynamic ledger/evidence/user context remains outside the stable prefix.
- Default tests remain deterministic/offline; live provider and bwrap smoke are
  opt-in.

## Commit

Status: pending in this lease

Message:

- feat(llm): record stable request prefix hash

No-commit reason:

- none

## Next Session

Run this in a fresh session:

```text
/goal $project-continuity
```
