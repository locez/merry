# Real Streaming Retry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace buffered whole-attempt retry with a cancellable live stream that retries only before observable model output.

**Architecture:** `RetryingModelProvider` will return a bounded-channel stream immediately and run retry attempts in one Tokio producer task. The producer suppresses provider-attempt `Started` events, emits one outer `Started`, retries setup/stream errors before committed output, and forwards post-output errors without replay.

**Tech Stack:** Rust 2024, Tokio, `futures-util`, `tokio::sync::mpsc`, cancellation tokens.

---

### Task 1: Lock The Streaming Contract With Failing Tests

**Files:**
- Modify: `crates/merry-llm/src/retry.rs`

- [ ] **Step 1: Replace the replay expectation with post-output failure behavior**

Change the existing `retrying_provider_replays_whole_turn_after_stream_error`
test so the first attempt emits `Started`, one text delta, then an unavailable
error. Assert that the consumer receives the outer `Started`, the original
delta, and the error, while the second scripted attempt remains unused:

```rust
#[tokio::test]
async fn retrying_provider_does_not_retry_after_visible_output() {
    // first attempt: Started, delta("partial"), unavailable error
    // second attempt: a complete response that must remain unused
    // assert events[0] == Started, events[1] == delta("partial")
    // assert events[2] is Unavailable and attempts_remaining() == 1
}
```

- [ ] **Step 2: Add a gated provider that cannot finish until the test releases it**

Add a test-only provider whose stream reads `Result<ModelEvent, ModelError>`
items from `tokio::sync::mpsc::Receiver`. Store the receiver in
`Arc<Mutex<Option<Receiver<_>>>>` so one provider attempt owns one stream.

```rust
struct GatedProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    receiver: Arc<Mutex<Option<mpsc::Receiver<Result<ModelEvent, ModelError>>>>>,
}
```

- [ ] **Step 3: Add the first-delta latency test**

Create the retry stream, send `Started` and `OutputTextDelta("live")`, and use
`tokio::time::timeout` to assert the two outer events arrive before sending the
final `Completed` event.

```rust
assert_eq!(next_with_timeout(&mut stream).await, ModelEvent::Started);
assert_eq!(
    next_with_timeout(&mut stream).await,
    ModelEvent::OutputTextDelta { delta: "live".to_owned() }
);
```

- [ ] **Step 4: Add pre-output retry and dropped-consumer tests**

Add one test where attempt one fails before output and attempt two succeeds,
asserting one outer `Started` and one final response. Add another provider that
sets an `Arc<AtomicBool>` drop guard in its attempt stream; drop the outer
stream and assert the guard becomes true without advancing a second attempt.

- [ ] **Step 5: Run the focused tests and verify failure**

Run:

```bash
cargo test -p merry-llm retry::tests -- --nocapture
```

Expected: the live-delta and no-post-output-retry assertions fail because the
current wrapper calls `collect_successful_attempt` before returning a stream.

### Task 2: Implement The Live Retry Producer

**Files:**
- Modify: `crates/merry-llm/src/retry.rs`

- [ ] **Step 1: Remove whole-attempt collection**

Delete `collect_successful_attempt`, `collect_attempt_events`, and the
`stream::iter(events)` replay path. Add a bounded model-event channel constant:

```rust
const RETRY_STREAM_BUFFER: usize = 16;
```

- [ ] **Step 2: Return a receiver stream immediately**

Clone the provider and context into a spawned producer. Convert the receiver to
`ModelEventStream` with `futures_util::stream::unfold`:

```rust
let (sender, receiver) = tokio::sync::mpsc::channel(RETRY_STREAM_BUFFER);
tokio::spawn(run_retry_stream(inner, policy, request, context, sender));
let stream = stream::unfold(receiver, |mut receiver| async move {
    receiver.recv().await.map(|item| (item, receiver))
});
Ok(Box::pin(stream))
```

- [ ] **Step 3: Implement committed-output state**

The producer sends exactly one `Ok(ModelEvent::Started)` before attempt one.
For every attempt it suppresses inner `Started`. Set `committed = true` before
forwarding text deltas, tool calls, or completion:

```rust
fn commits_output(event: &ModelEvent) -> bool {
    matches!(
        event,
        ModelEvent::OutputTextDelta { .. }
            | ModelEvent::ToolCallRequested { .. }
            | ModelEvent::Completed { .. }
    )
}
```

If an error or EOF occurs with `committed == true`, send it once and stop. If
it occurs before committed output, apply the existing retryability, attempt,
delay, and elapsed-budget checks.

- [ ] **Step 4: Make every blocking point consumer-aware and cancellable**

Select on all three relevant signals while reading an attempt or sleeping:

```rust
tokio::select! {
    biased;
    () = token.cancelled() => return send_cancelled(&sender).await,
    () = sender.closed() => return,
    item = stream.next() => { /* process item */ }
}
```

During backoff, select on `token.cancelled()`, `sender.closed()`, and
`tokio::time::sleep(delay)`. A closed consumer ends silently; explicit token
cancellation emits `ModelError::Cancelled` when the receiver still exists.

- [ ] **Step 5: Preserve retry telemetry**

Emit `AttemptStarted`, `RetryScheduled`, and `RetryExhausted` at the same
decision points as before. Do not emit a retry event after output has committed.

- [ ] **Step 6: Run focused tests**

Run:

```bash
cargo test -p merry-llm retry::tests -- --nocapture
```

Expected: all retry tests pass and the latency test observes the first delta
before the producer receives completion.

### Task 3: Prove Runtime Delta Propagation

**Files:**
- Modify: `crates/merry-runtime/tests/provider_boundary.rs`

- [ ] **Step 1: Add a provider whose completion is externally gated**

Use an MPSC-backed `ModelEventStream`. Build a runtime with retry enabled and
start a step event stream.

- [ ] **Step 2: Assert the public delta arrives before completion**

Send a model delta but not completion. Read runtime events with a timeout until
`RuntimeJournalPayload::AssistantOutputDelta` appears. Only then send the
provider completion and drain the step.

```rust
assert!(matches!(
    event.payload,
    RuntimeJournalPayload::AssistantOutputDelta { ref delta } if delta == "live"
));
```

- [ ] **Step 3: Run the integration test**

Run:

```bash
cargo test -p merry-runtime --test provider_boundary live_delta -- --nocapture
```

Expected: PASS, proving retry, runtime journaling, and public event projection
do not add whole-response buffering.

### Task 4: Verify The Slice

**Files:**
- Verify only

- [ ] **Step 1: Run formatting and focused linting**

```bash
cargo fmt --all --check
cargo clippy -p merry-llm -p merry-runtime --all-targets --all-features -- -D warnings
```

Expected: both commands exit zero.

- [ ] **Step 2: Run focused crate tests**

```bash
cargo test -p merry-llm
cargo test -p merry-runtime --test provider_boundary
```

Expected: both commands exit zero.

- [ ] **Step 3: Record the slice without absorbing unrelated worktree changes**

Review:

```bash
git diff -- crates/merry-llm/src/retry.rs crates/merry-runtime/tests/provider_boundary.rs
git diff --check
```

Do not stage or commit pre-existing changes from other files.
