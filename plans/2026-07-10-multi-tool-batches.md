# Multi-Tool Batch Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let one model turn request multiple tools, execute explicit parallel-safe waves with exclusive barriers, and continue only after the complete ordered result batch exists.

**Architecture:** `merry-llm` validates non-empty ordered model batches and complete continuations. `merry-core` adds durable batch identity and journal fields. `merry-runtime` atomically records a pending batch, schedules calls from registered-tool concurrency metadata, resolves all members, then compiles one ordered continuation.

**Tech Stack:** Rust 2024, Tokio, `futures-util`, Serde, Schemars, Merry session journal and artifact registry.

---

### Task 1: Add Model-Facing Batch Types

**Files:**
- Modify: `crates/merry-llm/src/tool.rs`
- Modify: `crates/merry-llm/src/request.rs`
- Modify: `crates/merry-llm/src/lib.rs`
- Modify: `crates/merry-llm/tests/protocol.rs`

- [ ] **Step 1: Write failing batch invariant tests**

Test that `ModelToolCallBatch::new` rejects an empty vector and duplicate call
IDs. Test that `ModelToolBatchContinuation::new` rejects missing, unknown, and
duplicate results and returns results in call order even when supplied out of
order.

- [ ] **Step 2: Add validated types**

Add these provider-neutral types with private fields, borrowed accessors,
`Serialize`, strict custom `Deserialize`, and `JsonSchema`:

```rust
pub struct ModelToolCallBatch {
    calls: Vec<ModelToolCall>,
}

pub struct ModelToolBatchContinuation {
    batch: ModelToolCallBatch,
    results: Vec<ModelToolResult>,
}
```

Constructors validate non-empty calls, unique IDs, and exactly one result for
every call. Sort results by the call order during construction.

- [ ] **Step 3: Generalize ModelRequest input validation**

Replace the strict alternating call/result parser with grouped validation:
consecutive `ToolCall` input items form one assistant batch and must be followed
by the same number of `ToolResult` items. Build
`ModelToolBatchContinuation` values, while keeping `continuations()` as a
flattened single-call compatibility accessor and adding
`batch_continuations()`.

- [ ] **Step 4: Run protocol tests**

```bash
cargo test -p merry-llm --test protocol batch -- --nocapture
```

Expected: PASS with JSON round trips and all invalid shapes rejected.

### Task 2: Add Durable Batch Identity And Events

**Files:**
- Modify: `crates/merry-core/src/id.rs`
- Modify: `crates/merry-core/src/tool.rs`
- Modify: `crates/merry-core/src/journal.rs`
- Modify: `crates/merry-core/src/runtime_event.rs`
- Modify: `crates/merry-core/src/lib.rs`
- Add tests in the existing modules

- [ ] **Step 1: Write failing core round-trip tests**

Cover a valid non-empty `PendingToolCallBatch`, duplicate call rejection, and
journal/public event JSON containing `batch_id` and ordered calls.

- [ ] **Step 2: Add `ToolCallBatchId` and pending batch**

Define `ToolCallBatchId` with the existing ID macro and add:

```rust
pub struct PendingToolCallBatch {
    id: ToolCallBatchId,
    calls: Vec<PendingToolCall>,
}
```

The constructor rejects empty calls and duplicate `ToolCallId` values.

- [ ] **Step 3: Replace single pending event payloads**

Add `ToolCallBatchPending { batch }` and
`BridgeToolCallBatchRequested { batch }` journal payloads. Project each batch
to one public `ToolCallBatchStarted` event containing its ID and ordered calls.
Keep deserialization of already persisted resolved single-call transcript data
unchanged; no pending batch is resume-safe.

- [ ] **Step 4: Run core tests**

```bash
cargo test -p merry-core
```

Expected: PASS.

### Task 3: Record And Reconstruct Batches Atomically

**Files:**
- Modify: `crates/merry-runtime/src/session/mod.rs`
- Modify: `crates/merry-runtime/src/session/tool_calls/mod.rs`
- Modify: `crates/merry-runtime/src/session/tool_calls/action.rs`
- Modify: `crates/merry-runtime/src/session/tool_calls/result.rs`
- Modify: `crates/merry-runtime/src/session/transcript.rs`
- Modify: `crates/merry-runtime/src/runtime/model_output.rs`
- Modify: `crates/merry-runtime/src/runtime/provider_step.rs`
- Modify: `crates/merry-runtime/src/step.rs`
- Test: `crates/merry-runtime/src/session/tests/tool_calls.rs`
- Test: `crates/merry-runtime/src/runtime/tests/provider_step_flow.rs`

- [ ] **Step 1: Write failing atomic-recording tests**

Given two valid model calls, assert one session mutation records one ordered
batch and emits its complete event set. Given a duplicate ID, assert no pending
call, transcript item, ledger entry, or sequence increment remains.

- [ ] **Step 2: Store pending batch membership**

Store pending batches in session state and derive `pending_tool_calls()` from
their unresolved members. Resolving a call updates its batch; resolving the
last member removes the pending batch. Persistence continues to reject any
unresolved batch.

- [ ] **Step 3: Collect all streamed and completed calls**

Replace `Option<PendingToolCall>` with an ordered map/vector keyed by call ID.
Validate that completed response calls exactly match the streamed calls, then
record one `PendingToolCallBatch`. Text commentary may precede the batch, as it
does for one call today.

- [ ] **Step 4: Compile grouped continuations**

Project transcript history as all calls from one batch followed by all ordered
results from that batch. Reject an incomplete historical batch before provider
request construction.

- [ ] **Step 5: Run session and provider-step tests**

```bash
cargo test -p merry-runtime session::tests::tool_calls -- --nocapture
cargo test -p merry-runtime runtime::tests::provider_step_flow -- --nocapture
```

Expected: PASS.

### Task 4: Declare Tool Concurrency

**Files:**
- Modify: `crates/merry-runtime/src/tool.rs`
- Modify: `crates/merry-runtime/src/lib.rs`
- Modify: built-in tool registrations under `crates/merry-runtime/src/`
- Modify: read-only registrations under `crates/merry-tool-workspace/src/`
- Test: existing tool registration tests

- [ ] **Step 1: Write default and opt-in tests**

Assert `RegisteredTool::new`, `read_only`, and `bridge` default to
`ToolConcurrency::Exclusive`. Assert `.with_parallel_safe_execution()` changes
only concurrency metadata and does not alter provider-visible `ToolSpec` JSON.

- [ ] **Step 2: Add the explicit contract**

```rust
pub enum ToolConcurrency {
    ParallelSafe,
    Exclusive,
}
```

Store it on `RegisteredTool`; expose `concurrency()` and the opt-in builder.
Mark only deterministic read-only workspace read/list/search and equivalent
runtime reads parallel-safe. Process, patch, permission, runtime-control,
network, MCP, and bridge tools stay exclusive initially.

- [ ] **Step 3: Run registration tests**

```bash
cargo test -p merry-runtime tool::tests -- --nocapture
cargo test -p merry-tool-workspace
```

Expected: PASS.

### Task 5: Implement The Runtime Batch Scheduler

**Files:**
- Create: `crates/merry-runtime/src/runtime/tool_batch.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/src/runtime/builder.rs`
- Modify: `crates/merry-runtime/src/agent_loop.rs`
- Modify: `crates/merry-runtime/src/interactive.rs`
- Test: `crates/merry-runtime/tests/agent_loop.rs`
- Test: `crates/merry-runtime/tests/interactive_agent_loop.rs`

- [ ] **Step 1: Write failing wave-order tests**

Register scripted executors that record start/end markers. Cover four
parallel-safe calls with limit two, the sequence `parallel, parallel,
exclusive, parallel`, out-of-order parallel completion, one failed result, and
batch cancellation.

- [ ] **Step 2: Add the builder limit**

Store a `NonZeroUsize` maximum tool concurrency in `RuntimeInner`, defaulting to
four, with `RuntimeBuilder::max_parallel_tool_calls`.

- [ ] **Step 3: Build deterministic waves**

Partition the ordered batch without inspecting arguments. Run a consecutive
parallel-safe wave with `stream::iter(calls).map(...).buffer_unordered(limit)`;
collect by original index. Run each exclusive item alone between waves.

- [ ] **Step 4: Integrate agent and interactive loops**

Replace `MultiplePendingToolCalls` blocking with a pending-batch outcome. Both
loops execute or bridge the full batch, publish each durable resolution event,
and start one continuation model turn only after every member resolves.

- [ ] **Step 5: Resolve cancellation deterministically**

Cancel all active child tokens, skip queued calls, then submit failed artifacts
and `tool_execution_cancelled` diagnostics for every unresolved member in
original order. Persist only after the batch is empty.

- [ ] **Step 6: Enforce final-output exclusivity**

If the configured final-output tool shares a batch with any other call, emit a
stable protocol diagnostic and execute none of the calls. A one-item final
output batch keeps existing behavior.

- [ ] **Step 7: Run loop tests**

```bash
cargo test -p merry-runtime --test agent_loop batch -- --nocapture
cargo test -p merry-runtime --test interactive_agent_loop batch -- --nocapture
```

Expected: PASS.

### Task 6: Support Multiple Bridge Results

**Files:**
- Modify: `crates/merry-runtime/src/agent_loop.rs`
- Modify: `crates/merry-runtime/src/interactive.rs`
- Modify: `crates/merry-runtime/src/interactive/handles.rs`
- Modify: `crates/merry-py/src/runtime.rs`
- Test: `crates/merry-runtime/src/runtime/tests/bridge_tool_flow.rs`
- Test: `crates/merry-py/tests/bindings.rs`

- [ ] **Step 1: Write out-of-order bridge tests**

Request two bridge calls, submit the second result first, then the first. Assert
both calls resolve, the continuation preserves model order, and an unknown or
duplicate ID is rejected without damaging the remaining batch.

- [ ] **Step 2: Match bridge commands by call ID**

Keep a pending-ID set for the active batch. Accept commands in any order, submit
each valid result immediately, and finish only when the set is empty. Project
one public batch-start event so SDKs can dispatch all calls.

- [ ] **Step 3: Run bridge and binding tests**

```bash
cargo test -p merry-runtime bridge_tool_flow -- --nocapture
cargo test -p merry-py --test bindings bridge -- --nocapture
```

Expected: PASS.

### Task 7: Verify The Slice

**Files:**
- Verify only

- [ ] **Step 1: Run format, lint, and affected workspace tests**

```bash
cargo fmt --all --check
cargo clippy -p merry-core -p merry-llm -p merry-runtime -p merry-tool-workspace -p merry-py --all-targets --all-features -- -D warnings
cargo test -p merry-core -p merry-llm -p merry-runtime -p merry-tool-workspace -p merry-py
git diff --check
```

Expected: all commands exit zero.
