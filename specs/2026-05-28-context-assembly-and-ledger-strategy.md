# Context Assembly And Ledger Strategy

Date: 2026-05-28

## Purpose

Merry needs a context assembly strategy that can support long, tool-heavy
coding tasks without making raw chat history or provider conversation state the
source of truth.

This spec defines the first context compiler contract for stateless provider
calls, function-call continuity, runtime-owned ledger state, artifact-backed
evidence, and future task anchoring. It is intentionally not a TUI or command
spec.

The core rule is:

```text
Record aggressively. Project conservatively. Rewrite rarely.
```

## Design Goals

- Keep provider calls stateless by default.
- Preserve recent function-call continuity so the model can continue after tool
  results.
- Keep full tool outputs and exact evidence in artifacts, not long-lived prompt
  text.
- Record compact runtime facts in the ledger immediately after evidence is
  durable.
- Avoid projecting the full ledger into every model request.
- Preserve stable prefix cache behavior.
- Use model context window size to choose budgets and compaction watermarks.
- Leave task pinning as an explicit future interaction, not automatic model
  inference.

## Non-Goals

- Do not add a `/task` command in this slice.
- Do not add a TUI flow in this slice.
- Do not automatically infer or mutate the current task from arbitrary user or
  model text.
- Do not put ledger observations into prompt context just because they were
  recorded.
- Do not add a provider conversation-state dependency such as using a provider
  response ID as Merry task state.
- Do not add model-generated summaries as trusted ledger facts.
- Do not add dynamic runtime status markers to every request.

## Data Planes

### Artifact Store

The artifact store owns exact evidence:

- tool result payloads;
- process stdout/stderr/status metadata;
- patch inputs and results;
- large source snapshots or output payloads;
- denial and failure payloads.

Artifacts are durable runtime evidence. They may be large and should not be
included in prompt context by default.

### Ledger

The ledger owns structured task facts and lifecycle facts.

Function-call lifecycle facts may be recorded when the model emits a tool call:

```text
tool_call_pending: call_id, tool_name, argument_fingerprint, step
```

Observation facts are recorded only after the supporting result, denial, or
failure artifact has been written:

```text
tool_result_observation: tool_name, status, summary metadata, artifact refs
```

Ledger facts are runtime state. Recording a fact does not imply that the fact is
projected into the next model request.

### Function-Call Continuity

Function-call continuity is the uncheckpointed exact provider-visible sequence
needed for the model to continue after a tool result:

```text
function_call
function_call_output
```

It is protocol continuity, not task memory. It should be append-only until an
explicit checkpoint or compaction boundary records which older exact
continuity entries are covered. Before that boundary, all uncheckpointed
function-call continuity remains provider-visible.

### Task Anchor

A Task Anchor is an optional future runtime state created by explicit user
action, such as a TUI pin or a later `/task` command.

The first shape should stay minimal:

```text
objective: one user-authored sentence
source_event_id
revision
```

Task Anchor is not automatic task detection. The model must not silently create
or mutate it. Future model suggestions may propose a task anchor update, but
runtime state changes require explicit user confirmation.

## Context Layout

The compiler should assemble model input in cache-aware layers.

```text
Stable prefix:
  system/developer/runtime contract
  stable tool schemas
  project rules such as AGENTS.md
  pinned tool profile

Optional stable-ish task anchor:
  user-pinned objective, if present

Checkpoint segment:
  latest ledger-derived checkpoint, if present
  low-frequency, artifact/evidence refs only
  replaces older append-only body ranges only after checkpoint/compaction

Append-only body:
  post-checkpoint user messages
  post-checkpoint assistant messages
  post-checkpoint recent function_call/function_call_output continuity
  post-checkpoint ledger deltas only when an explicit context policy selects
  them

Ephemeral tail:
  current step/status/pending-call details only when strictly necessary
  avoid by default; do not use as a per-turn status marker

No default ledger projection:
  ledger facts remain queryable runtime state
  selected ledger-derived content appears only at checkpoint/compaction or when
  an explicit context policy chooses it
```

There should be no default per-turn tail marker. Recent function-call
continuity already carries the required provider-visible continuation in most
cases. The checkpoint segment is part of the assembled context shape even before
checkpoint content generation is implemented; an empty checkpoint segment should
not render prompt text.

## Recording Rules

The runtime should record state in this order:

```text
model emits function_call
-> record pending lifecycle fact

tool execution needs exact input evidence
-> record input artifact if applicable

tool completes, denies, or fails
-> record result/denial/failure artifact
-> record resolved lifecycle fact
-> reducer records compact ledger observation backed by artifact refs
-> emit externally visible result event
```

No observable event may claim an artifact-backed fact before the artifact is
durable.

Reducers must be deterministic where possible. Model-generated text may assist
user-facing summaries, but it must not become trusted ledger truth unless backed
by runtime-owned evidence.

## Projection Rules

The compiler must treat runtime state and prompt projection separately:

- Ledger facts are recorded by default.
- Ledger facts are not projected by default.
- Full artifacts are not projected by default.
- Uncheckpointed function-call continuity is projected in provider-visible
  order.
- Checkpoints are projected only from the checkpoint segment after an explicit
  checkpoint/compaction boundary.
- Ledger deltas may be appended after the latest checkpoint only when an
  explicit context policy selects them; do not render a full ledger projection
  on every request.
- Exact artifacts remain available through artifact or source-read tools when
  needed.

This separation is what allows the runtime to "check the books" without turning
the ledger into prompt history.

## Budget Strategy

Budgets should be derived from the effective model context window when known.

The resolver should prefer:

```text
explicit config override
provider model metadata
bundled model catalog
observed request-too-large feedback
conservative fallback
```

The compiler should reserve space for output and safety margin before computing
body budgets.

Recommended first policy:

```text
effective_window = resolved_context_window * effective_context_window_percent
body_budget = effective_window - stable_prefix_tokens - output_reserve
```

Use watermarks instead of step-count compaction:

```text
cost_aware:
  soft_water = body_budget * 0.60
  hard_water = body_budget * 0.80

balanced:
  soft_water = body_budget * 0.70
  hard_water = body_budget * 0.90

capacity:
  soft_water = body_budget * 0.85
  hard_water = body_budget * 0.95
```

The default should be balanced unless provider/model metadata or user config
selects another policy.

Compaction should be event- and budget-triggered, not "every N turns." A large
context window should be used rather than prematurely discarded, especially when
the provider's prompt cache makes append-only growth cheap.

## Checkpoint And Compaction

Checkpointing is the low-frequency boundary where old append-only body content
can be replaced by a compact statement. The compiled context should reserve a
checkpoint segment for the latest checkpoint. This segment is more stable than
the append-only body, but it is not part of the stable prefix because it changes
as the task progresses.

A checkpoint should be generated from runtime-owned state:

- task anchor, if present;
- ledger facts selected by policy;
- artifact refs and evidence refs;
- recent unresolved or uncheckpointed function-call continuity that must remain
  exact.

Checkpointing may use model assistance later, but the first reliable checkpoint
path should prefer deterministic reducers and artifact-backed facts.

After checkpointing, raw function-call continuity and append-only body entries
older than the checkpoint may be removed from prompt context, provided exact
evidence remains accessible through artifacts.

An implementation may first add the checkpoint segment and watermark trigger
without implementing checkpoint content generation. That partial slice must not
claim to satisfy the full checkpoint/compaction acceptance criteria until
checkpoint content, evidence refs, and removal boundaries are implemented and
tested.

## Cache Rules

Stable prefix content should change rarely:

- system/developer/runtime contract changes;
- tool schema or tool profile changes;
- project rules change;
- explicit task anchor revision changes.

The checkpoint segment should change only at checkpoint/compaction boundaries.
Dynamic body content after the checkpoint should be append-only until the next
checkpoint boundary. Avoid rendering a newly sorted, deduplicated, or rewritten
ledger projection on every request because that moves changing text earlier
than necessary and damages cache reuse.

The compiler should expose diagnostic hashes:

```text
stable_prefix_hash
tool_profile_hash
task_anchor_hash
checkpoint_hash
append_body_hash
dynamic_context_hash
```

## Acceptance Criteria

- A model request can be compiled without projecting ledger facts.
- Recent function-call continuity is preserved in provider-visible order.
- Ledger observations are recorded only after their supporting artifact exists.
- Updating ledger state alone does not change the stable prefix hash.
- Adding a Task Anchor changes context only when explicitly provided.
- Without a Task Anchor, ordinary user messages remain append-only.
- Compaction is triggered by watermarks or explicit request, not by a fixed turn
  count.
- The compiled context shape reserves a checkpoint segment; an absent
  checkpoint renders nothing.
- Checkpoint output references artifacts/evidence instead of replacing exact
  evidence.
- Provider calls remain usable with `store=false` and no provider conversation
  state as Merry runtime state.

## Open Questions

- What exact provider-neutral metadata should each built-in reducer record for
  workspace read/list/search, patch, and process execution?
- Should Task Anchor be stored in the task ledger or in a separate session
  control-plane record?
- What should the first deterministic checkpoint format look like?
- Which providers can report context window metadata directly, and how should
  stale catalog values be corrected at runtime?
