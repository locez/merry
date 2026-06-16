# Session State Resume Store

Date: 2026-06-14

## Purpose

Define the first Merry session resume contract.

Merry already has structured runtime-owned session state. The missing product
capability is not a new session runner, a Codex-style rollout replay system, or
a provider conversation continuation. The missing capability is that a runtime
session with a stable `SessionId` can save its recoverable `SessionState` and a
later runtime can load that state, receive freshly injected construction-time
dependencies, and continue normal provider reasoning from the next user input.

The first slice should optimize for a clean resume semantic and a small
implementation surface. It may lose in-flight work, but it must not restore a
half tool exchange that blocks or misdirects the next provider step.

## Decision

Persistence belongs to `SessionState`.

The intended shape is:

```rust
impl SessionState {
    fn save_to(&self, store: &FileSessionStore) -> Result<()>;
    fn load_from(store: &FileSessionStore, session_id: &SessionId) -> Result<Self>;
}
```

The public product/API shape should remain runtime-owned and friendly:

```rust
Runtime::resume(session_id)
```

or, where provider/tool/profile injection must stay explicit:

```rust
Runtime::builder(session_id)
    .with_profile(profile)?
    .resume_from_default_store()?
    .build()
```

The exact Rust method names can change during implementation. The important
contract is that users and SDK callers resume a runtime session; they do not
assemble persisted session internals themselves.

Do not introduce a public `SessionResumeSnapshot` concept. If serialization
needs a private document/envelope type, keep it as an implementation detail such
as `StoredSessionState` or `SessionStateDocument`. Do not use type names such as
`V1` as a substitute for a thoughtful format contract. If a file format version
is needed, store it as a field in the persisted document.

## Store Location

The default store is filesystem-backed.

Use XDG state paths:

```text
$XDG_STATE_HOME/merry/sessions/<session_id>/
fallback: ~/.local/state/merry/sessions/<session_id>/
```

`SessionId` validation already makes ids safe as a single path component.

Initial layout:

```text
<state_home>/merry/sessions/<session_id>/
  state.json
```

`state.json` is the latest complete session document. The filesystem store may
use same-directory temporary files while saving, but it must not keep
per-artifact payload files, snapshot histories, or pack files in the first
slice.

The first slice does not need SQLite, an abstract `SessionStore` trait, remote
storage, listing/search indexes, archives, or a global session catalog. Add
those only after there is a real second store or query use case.

## Recoverable State

The persisted state should restore the session aggregate needed for the next
provider request and runtime inspection APIs.

Persist:

- `session_id`
- `next_sequence`
- `session_started`
- task ledger state
- artifact registry metadata and exact artifact content
- compacted checkpoint state
- context entries
- judgment registry
- summary draft promotion registry
- action audit registry
- transcript
- resolved tool call ids
- usage snapshot

Do not persist:

- provider instances
- model configs
- tool registry
- process runners
- permission admission sources
- subagent manager handles
- cancellation tokens
- active-step state
- in-flight provider streams
- in-flight tool or process execution
- `project_rules`
- `skill_catalog`
- pending tool calls
- activated memories

`project_rules` and `skill_catalog` are construction/profile inputs and should
be injected by the builder on the resumed runtime. This lets a resumed session
use the current project instructions and available skill metadata instead of
stale persisted startup context.

`activated_memories` are a per-step projection and should be recomputed on the
next provider step.

The current `memory_store` is not part of the first resume surface. It can be
added later after memory persistence and public memory-write semantics are
designed.

## Savepoint Semantics

Save only resume-safe session states.

Complete boundaries are:

- a text-output model step after assistant output and `StepCompleted` have been
  durably incorporated and no tool call is pending
- a complete tool exchange after a tool result has been durably incorporated
  into session memory state, including its artifact content, transcript item,
  ledger/audit facts, resolved tool-call id, and emitted result event state
- a final-output tool call after the structured output artifact and
  `FinalOutputRecorded` state have been durably incorporated

Do not save a state where an LLM-returned tool call is pending or a tool/process
is executing. If the process crashes or the host restarts during that interval,
the resumed runtime returns to the previous saved state and loses the
incomplete work.

A model tool-call step is not complete at `ToolCallPending`; it becomes
resume-safe only when the matching tool result has been recorded. This is not a
broader turn-level replay contract; it only records stable session state that
can be used by the next provider step.

The resume-safe predicate is:

- no active step is being persisted
- no pending tool calls are persisted
- every artifact referenced by persisted state has exact content available
- transcript, ledger, checkpoint/context, audits, resolved ids, and counters
  are internally consistent

Automatic savepoints are best-effort side effects after the runtime has already
committed a complete boundary to `SessionState`. A failed automatic savepoint
must not turn an otherwise successful tool result, final-output record, or model
step completion into a failed runtime operation, and it must not hide already
committed journal events from the caller. Explicit `Runtime::save_session`
remains strict: save failure is the primary result of that API.

## Write Ordering

The filesystem store must avoid exposing a partially written state as the
latest resumable state.

Required ordering:

1. Serialize the complete session document, including artifact content.
2. Write the document bytes to a same-directory temporary file.
3. Flush the temporary file enough that the next `state.json` is not a partial
   document under normal process restart.
4. Atomically replace `state.json` with the temporary file.

If a write fails, the previous `state.json` remains the latest resumable state.
It is acceptable to leave an unreferenced temporary file; a future cleanup pass
can remove it.

## Resume Semantics

Loading a session reconstructs a `SessionState` from `state.json`.

After loading:

- `active_step` is false because it belongs to `RuntimeInner`, not
  `SessionState`
- pending tool calls are empty
- activated memories are empty
- provider/tools/runners/profile/config are supplied by the new builder
- the next user input is the current instruction
- previous transcript content is historical context/evidence, not a live
  instruction to continue an interrupted tool execution

If a prior process stopped after the model requested a tool but before the tool
result was saved, that tool call must not gate the resumed provider step.

## Error Handling

Loading should fail clearly when:

- the session directory does not exist
- `state.json` is absent
- the persisted document has an unsupported format version
- the session id inside the document does not match the requested `SessionId`
- persisted context evidence cannot be resolved from persisted artifacts
- persisted data cannot be validated into `SessionState`

Saving should fail clearly when:

- the store root cannot be created
- the state document cannot be serialized or atomically replaced

Do not silently fall back to an ephemeral empty session on resume failure.

## Non-Goals

- Do not implement a Codex-style append-only rollout replay system in the first
  slice.
- Do not add SQLite or a session search/list index.
- Do not add a remote store or host-provided store trait until there is a real
  second implementation.
- Do not persist provider conversation state.
- Do not use OpenAI `previous_response_id` or provider-side storage as Merry
  resume state.
- Do not persist construction-time project rules or skill metadata.
- Do not persist in-flight work, pending tool calls, active cancellation state,
  or activated memories.
- Do not make `SessionResumeSnapshot` a public API or product concept.
- Do not change the one-active-step-per-runtime rule.

## Acceptance

Deterministic runtime coverage should prove:

- A session with user/assistant transcript, artifacts, ledger facts, context,
  checkpoint state, audit facts, resolved tool calls, counters, and usage can
  save and load through `FileSessionStore`.
- A resumed runtime can compile the next provider request from restored session
  state after provider/tools/profile are freshly injected.
- A complete tool exchange is saved only after the tool result artifact,
  transcript update, ledger/audit facts, and resolved tool-call state are
  incorporated.
- A pending tool call is not persisted; restoring from the previous saved state
  does not gate the next provider step.
- `project_rules` and `skill_catalog` are not restored from persisted state and
  are supplied by resumed runtime construction.
- `activated_memories` are empty after load and can be recomputed on the next
  provider step.
- Artifact records without restorable inline content make resume fail with an
  actionable error.
- `state.json` replacement is atomic enough that a failed save leaves the
  previous saved state loadable.

Relevant checks for the implementation slice:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p merry-runtime
```

Run broader workspace checks if the implementation touches CLI, facade, or SDK
resume APIs.
