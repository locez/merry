# Runtime Session Module Split

Date: 2026-06-07

## Purpose

`merry-runtime` session state is the runtime-owned source of truth for one
agent session. It already has some child modules, but `session.rs` still mixes
the session aggregate root, event sequencing, artifact/history recording,
tool-call lifecycle helpers, and broad unit tests.

This refactor keeps behavior unchanged while making the session subsystem look
like an intentional aggregate instead of a partially split large file.

## Scope

In scope:

- Convert `src/session.rs` into `src/session/mod.rs`.
- Keep `SessionState` as the session aggregate root.
- Move basic event sequencing into `session/events.rs`.
- Move artifact/output recording helpers into `session/recording.rs`.
- Move append-only user/assistant message helpers into `session/messages.rs`.
- Split `session/tool_calls.rs` into a small `tool_calls/` module family.
- Move session unit tests into focused files under `session/tests/`.

Out of scope:

- No public runtime API changes.
- No session resume implementation in this slice.
- No behavior changes to event ordering, artifact IDs, ledger facts, tool-call
  resolution, compaction windows, judgment recording, or memory projection.
- No changes to provider, CLI, SDK, workspace tools, or sandbox behavior.

## Target Shape

```text
crates/merry-runtime/src/session/
  mod.rs
  artifacts.rs
  checkpoint_window.rs
  context_state.rs
  events.rs
  history.rs
  judgments.rs
  messages.rs
  recording.rs
  tool_result.rs
  tool_calls/
    mod.rs
    action.rs
    result.rs
    skill.rs
  tests/
    mod.rs
    lifecycle.rs
    compaction.rs
    tool_calls.rs
    judgments.rs
    context_memory.rs
```

## Plan

1. Move `session.rs` to `session/mod.rs` and keep only aggregate root wiring.
2. Extract event methods first because other session modules depend on durable
   event sequence assignment.
3. Extract artifact/message recording while preserving artifact ID generation
   and append-only history ordering.
4. Split tool-call handling by lifecycle kind:
   - pending/query/bridge in `tool_calls/mod.rs`
   - normal/final tool results in `tool_calls/result.rs`
   - proposed/denied/guarded action audit results in `tool_calls/action.rs`
   - skill-used derived event in `tool_calls/skill.rs`
5. Move unit tests next to the domain they validate, using shared helpers in
   `session/tests/mod.rs`.
6. Run focused runtime checks, then full workspace checks if the focused checks
   pass.

## Acceptance

- `cargo fmt --all --check`
- `cargo test -p merry-runtime`
- No runtime-visible event shape, ordering, or artifact ID behavior changes.
- `session/mod.rs` remains the aggregate root rather than a new large file.
- No session source or session test file is intentionally left as another
  thousand-line catch-all.
