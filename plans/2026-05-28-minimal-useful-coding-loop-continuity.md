# Minimal Useful Coding Loop Continuity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the live coding-loop task behave like a real stateless-provider coding agent by preserving uncheckpointed function-call continuity, keeping ledger/artifact state out of default prompt projection, improving patch reliability, and adding budget/checkpoint guardrails without turning provider conversation state into Merry runtime state.

**Architecture:** Runtime remains the source of truth for session state, artifacts, ledger facts, continuations, context compilation, and projection policy. Provider calls stay stateless by default with `store=false`; Merry replays uncheckpointed function-call/output continuity required by the provider protocol while exact evidence remains artifact-backed. Context projection is allowlisted: ledger observations and artifact payloads do not enter prompt context by default, ordinary summaries cannot become an implicit reducer projection channel, and checkpoint/context-policy projections must be explicit low-frequency boundaries. The work is split into small vertical slices so each milestone has deterministic tests before live smoke validation.

**Tech Stack:** Rust 2024, Tokio, `merry-runtime`, `merry-llm`, `merry-tool-workspace`, `merry-cli`, OpenAI-compatible Responses provider, deterministic fake providers/runners, opt-in `bwrap` and live-provider debug smokes.

---

## Design Inputs

- Roadmap focus: `ROADMAP.md` keeps P0 on the Minimal Useful Coding Loop.
- Context strategy: `specs/2026-05-28-context-assembly-and-ledger-strategy.md`.
- Current live smoke command: `MERRY_OPENAI_DEBUG=1 ./target/debug/merry --with-sandbox debug coding-loop-task-live-smoke --task status-text`.
- Current core rule: record aggressively, project conservatively, rewrite rarely.
- Non-negotiable boundary: do not use provider `previous_response_id` or `store=true` as Merry runtime state.
- Non-negotiable projection boundary: recording ledger facts, artifacts, or tool-result summaries is not permission to project them into model context.

## Review Corrections Applied

The first version of this plan left too much room for a context projection bypass. This revision makes those boundaries explicit:

- `ContextSummary` must not become an implicit reducer output channel that gets rendered into every prompt.
- `CompiledContext` may project only explicit checkpoint/context-policy content, explicit manual context, or independently justified runtime projections such as activated memory.
- Ordinary ledger observations and artifact payloads must stay out of prompt context by default.
- Append-only user/assistant body is either implemented in this plan or explicitly marked out-of-scope before implementation; it must not remain ambiguous.
- `AGENTS.md`/project rules need a stable-prefix path if they are treated as durable project instructions.
- The context compiler must reserve a checkpoint segment between TaskAnchor and append-only body; this plan may leave it empty but must not omit it from the structure.
- Budget calculations must subtract `stable_prefix_tokens` before deriving body watermarks.
- Checkpoint trigger primitives plus an empty checkpoint slot do not implement checkpoint content generation or compaction semantics.

## File Structure

- Modify `crates/merry-runtime/src/session.rs`: rename the stored function-call continuity sequence to uncheckpointed tool continuations and preserve resolved tool continuations until a future checkpoint/compaction boundary covers them.
- Modify `crates/merry-runtime/src/runtime.rs`: stop consuming tool continuations when the model emits the next tool call; do not clear provider-visible continuity at terminal assistant completion; checkpoint/compaction owns future trimming.
- Modify `crates/merry-runtime/tests/provider_boundary.rs`: replace tests that asserted consume-on-success or consume-on-new-pending with uncheckpointed retention/retry coverage.
- Modify `crates/merry-runtime/src/context.rs`: define projection policy boundaries for explicit checkpoint/context-policy projections, project rules, checkpoint segment assembly, append-only body assembly, and budget/checkpoint primitives.
- Modify `crates/merry-runtime/src/step.rs`: assemble requests as stable runtime instructions plus stable project rules plus append-only body plus uncheckpointed continuations; avoid default ledger/artifact projection and avoid unrestricted `ContextSummary` projection.
- Modify `crates/merry-llm/src/request.rs`: expose any missing request diagnostics needed by compiler/cache tests, such as dynamic message hash versus continuation hash if required.
- Modify `crates/merry-runtime/tests/agent_loop.rs`: add deterministic multi-tool continuity tests and context/cache diagnostic tests.
- Modify `crates/merry-cli/src/main.rs`: keep debug smoke runtime-event printing for the live task smoke and tighten live-smoke assertions around realistic coding behavior.
- Modify `crates/merry-cli/tests/debug.rs`: update deterministic CLI smoke tests when live-smoke acceptance changes affect shared helper behavior.
- Modify `crates/merry-tool-workspace/src/lib.rs`: upgrade `workspace_patch` parser/planner/executor behavior for multi-file patch and disambiguated hunks.
- Modify `crates/merry-tool-workspace/tests/runtime_integration.rs`: add runtime-level patch integration coverage for repeated text, multi-file patching, failure behavior, and cumulative uncheckpointed continuation replay in the coding-loop harness.
- Optional modify `crates/merry-provider-openai/src/render.rs`: only if rendering tests show the provider adapter loses function-call/output order.
- Optional modify `examples/config.toml`: only if accepted config keys change. This plan should not introduce config keys until the budget policy slice.
- Do not modify ignored `docs/` or `merry-raw-docs/`.
- Do not update `ROADMAP.md` unless the user explicitly approves a roadmap/status edit.

## Milestones

1. **M1: Uncheckpointed Function-Call Continuity**
   Preserve exact uncheckpointed tool call/output pairs across multiple model steps under `store=false`.
2. **M2: Context Compiler Layer Contract**
   Lock in projection permissions, stable-prefix versus dynamic-body behavior, append-only message history, project rules, and proof that ledger/artifact updates are not projected by default.
3. **M3: Live Coding Smoke Feedback Loop**
   Use the realistic `status-text` task to verify inspect/read/patch/check/test/final behavior and print enough runtime events to debug failures.
4. **M4: Diff-Style Workspace Patch Reliability**
   Make `workspace_patch` robust for localized edits, repeated text, and multi-file patches.
5. **M5: Checkpoint Segment And Trigger Skeleton**
   Reserve the checkpoint segment in the compiler shape and add model-window-aware trigger decisions without claiming checkpoint content generation or compaction support.

Each milestone must be committed separately unless the user says otherwise.

## Implementation Status Sync: 2026-05-29

This plan was partially stale after manual corrections and implementation work.
Use this sync section to avoid redoing completed smoke/patch work:

- **M1 is complete.** Uncheckpointed tool continuation retention and
  terminal-completion retention were implemented in
  `beaa63a fix(runtime): preserve coding loop continuity`.
- **M3 live/task smoke feedback is mostly complete.** Runtime JSONL reporting
  for task live smoke exists in `write_coding_loop_task_live_smoke_report`,
  and `assert_coding_loop_task_live_smoke_tool_sequence` checks required tools,
  `AGENTS.md`, `src/lib.rs`, `workspace_patch`, `cargo check -p`, and
  `cargo test -p`. Remaining optional tightening: explicitly require
  `tests/status.rs` reads and add a compact failure summary with loop status,
  step count, pending-call state, missing observations, and fixture path.
- **M4 patch/fixture realism is substantially implemented.** The model-visible
  patch tool is `workspace_patch`; standard patch envelope alias, multi-file
  unit execution, ambiguous/stale preimage failures, localized patch size
  checks, natural task live prompt, and repeated-`todo` status fixture are
  covered in current code/tests, mainly across `897184e`,
  `2adf538`, and `c8077a6`.
- **M2 context/request assembly guardrails are partly complete.** The runtime
  now has deterministic tests proving ledger observations and artifact payloads
  are not projected into prompt messages by default, and provider request
  assembly now replays a minimal runtime-owned append-only user/assistant body.
  Agent-loop continuation control prompts are compiled for the current step but
  are not recorded as user conversation history. No dedicated project-rules
  stable-prefix layer, checkpoint segment, or context budget/window skeleton has
  been implemented in this slice.
- **Do not continue M3/M4 as the next milestone by default.** The next useful
  implementation slice is the remaining M2 stable project-rules/checkpoint
  boundary work unless a new live-smoke failure exposes a concrete blocker.
- **Still open from this plan:** Task 6 project-rules stable prefix and
  Task 13/14/15 budget/window/checkpoint skeleton. Dedicated parser-only
  duplicate-file tests and runtime-level multi-file patch integration remain
  optional hardening, not the next active blocker.

## Acceptance Commands

Run these after each milestone that touches Rust behavior:

```bash
cargo fmt --all --check
cargo test -p merry-runtime agent_loop
cargo test -p merry-tool-workspace workspace_patch
cargo test -p merry-cli coding_loop_task
```

Run broader checks before reporting the plan fully implemented:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Manual opt-in checks remain non-default:

```bash
cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored
MERRY_OPENAI_DEBUG=1 ./target/debug/merry --with-sandbox debug coding-loop-task-live-smoke --task status-text
```

If a command cannot run in the current environment, record the exact reason in the final report and keep the deterministic tests as the blocking evidence.

## Task 1: Add A Failing Multi-Tool Continuity Test

**Files:**
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [x] **Step 1: Add the deterministic test**

Add this test near `agent_loop_executes_one_tool_and_continues_to_final_completion`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_preserves_uncheckpointed_tool_continuations_until_compaction() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-first",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-second",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final after two tools"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-uncheckpointed-continuity",
        provider.clone(),
        executor,
    );

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search twice, then answer.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.steps_run(), 3);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].continuations().is_empty());

    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-first"
    );

    assert_eq!(requests[2].continuations().len(), 2);
    assert_eq!(
        requests[2]
            .continuations()
            .iter()
            .map(|continuation| continuation.call().id().as_str())
            .collect::<Vec<_>>(),
        ["call-first", "call-second"]
    );
    assert!(
        requests[1].dynamic_context_hash() != requests[2].dynamic_context_hash(),
        "adding the second uncheckpointed continuation should change only dynamic request context"
    );
    assert_eq!(
        requests[1].stable_prefix_hash(),
        requests[2].stable_prefix_hash(),
        "uncheckpointed continuation growth must not move the cacheable stable prefix"
    );
}
```

- [x] **Step 2: Run the focused test and confirm it fails**

Run:

```bash
cargo test -p merry-runtime agent_loop_preserves_uncheckpointed_tool_continuations_until_compaction
```

Expected before implementation: FAIL because the third provider request only includes the latest continuation or otherwise does not include both prior call/result pairs.

## Task 2: Preserve Uncheckpointed Continuations Until Compaction

**Files:**
- Modify: `crates/merry-runtime/src/session.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [x] **Step 1: Clarify session continuation APIs**

In `crates/merry-runtime/src/session.rs`, replace the old consume-oriented naming with uncheckpointed continuity naming:

```rust
    pub(crate) fn uncheckpointed_tool_continuation_snapshots(
        &self,
    ) -> Result<Vec<ResolvedToolContinuationSnapshot>, ArtifactError> {
        ...
    }
```

The private field should be named `uncheckpointed_tool_continuations`. Do not keep `consume_tool_continuations` or a terminal-completion clear method in this slice; those names encode the wrong lifecycle.

- [x] **Step 2: Use uncheckpointed snapshots when compiling the provider request**

In `crates/merry-runtime/src/runtime.rs`, replace the old consume-oriented snapshot read:

```rust
let continuations = match session.unconsumed_tool_continuation_snapshots() {
```

with:

```rust
let continuations = match session.uncheckpointed_tool_continuation_snapshots() {
```

This makes the provider-visible replay boundary explicit: continuations are retained until a checkpoint/compaction task defines trimming.

- [x] **Step 3: Stop clearing continuations when the model asks for another tool**

In `send_tool_call_pending_event`, remove the `sent_continuation_count` parameter from the function signature and call sites. Replace this block:

```rust
match session.record_tool_call_pending(call) {
    Ok(event) => {
        session.consume_tool_continuations(sent_continuation_count);
        Ok(event)
    }
    Err(diagnostic) => Err(diagnostic),
}
```

with:

```rust
session.record_tool_call_pending(call)
```

This is the core fix: a new pending tool call should not erase earlier resolved tool continuations while they are still uncheckpointed.

- [x] **Step 4: Do not clear continuity at terminal assistant completion**

In `send_assistant_text_output_completed_events`, remove any continuation consume/clear call. Terminal assistant completion records assistant output and step completion; it is not a checkpoint boundary.

- [x] **Step 5: Remove now-unused count plumbing**

Remove continuation count plumbing from function signatures that no longer need it. Keep the local count for provider request tracing:

```rust
trace_provider_request(provider.name().as_str(), &request, sent_continuation_count);
```

- [x] **Step 6: Run focused runtime tests**

Run:

```bash
cargo test -p merry-runtime agent_loop_preserves_uncheckpointed_tool_continuations_until_compaction
cargo test -p merry-runtime agent_loop_executes_one_tool_and_continues_to_final_completion
cargo test -p merry-runtime unregistered_tool_resolves_failed_and_continues_once
cargo test -p merry-runtime denied_registered_tool_resolves_failed_and_agent_loop_continues_once
cargo test -p merry-runtime --test provider_boundary continuation
cargo test -p merry-tool-workspace --test runtime_integration coding_loop_harness_inspects_patches_verifies_and_completes
```

Expected: all PASS.

- [x] **Step 7: Commit**

Completed in `beaa63a fix(runtime): preserve coding loop continuity`.

```bash
git add crates/merry-runtime/src/session.rs crates/merry-runtime/src/runtime.rs crates/merry-runtime/tests/agent_loop.rs
git commit -m "fix(runtime): preserve uncheckpointed tool continuations"
```

## Task 3: Prove Terminal Completion Does Not Clear Uncheckpointed Continuations

**Files:**
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [x] **Step 1: Document the lifecycle boundary in the test name and assertion**

Terminal assistant completion is not a checkpoint. Exact function-call continuity remains uncheckpointed until a checkpoint/compaction boundary records what it covers.

- [x] **Step 2: Add a post-final request test**

Add this test after the previous continuity test:

```rust
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_keeps_uncheckpointed_continuations_after_final_answer() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-first",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("first final"))],
        vec![Ok(completed_text_event("second final"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-uncheckpointed-continuity-final",
        provider.clone(),
        executor,
    );

    let first = run_default_loop(&runtime, "Search once.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);

    let second = run_default_loop(&runtime, "Answer without compaction.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[2].continuations().len(),
        1,
        "terminal assistant completion is not compaction; old continuations remain uncheckpointed"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        "call-first"
    );
}
```

- [x] **Step 3: Run the test**

Run:

```bash
cargo test -p merry-runtime agent_loop_keeps_uncheckpointed_continuations_after_final_answer
```

Expected: PASS.

- [x] **Step 4: Commit if this test was not included in Task 2**

Included in the M1 continuation commit `beaa63a`.

```bash
git add crates/merry-runtime/tests/agent_loop.rs
git commit -m "test(runtime): cover uncheckpointed continuation retention"
```

Skip this commit if Task 2 already committed this test.

## Task 4: Lock Context Projection Boundaries

**Files:**
- Modify: `crates/merry-runtime/src/context.rs`
- Modify: `crates/merry-llm/src/request.rs`
- Modify: `crates/merry-runtime/src/step.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [x] **Step 1: Write down the projection contract in code comments**

Add or update comments near `ContextCompiler` and `compile_step_model_request` so the implementation contract is explicit:

```text
Context entries are projection inputs only when they come from an explicit checkpoint/context-policy path or an independently justified runtime projection such as activated memory.
Reducers must not write ordinary tool-result summaries into prompt context through ContextSummary.
Artifacts and ledger facts remain queryable runtime state and are not projected just because they were recorded.
```

If the current `record_context_summary` API remains public, document it as a manual/explicit context API, not a reducer default path.

- [x] **Step 2: Check whether extra hashes are needed**

Inspect `ModelRequest`:

```bash
rg -n "stable_prefix_hash|dynamic_context_hash|stable_prefix_message_count|continuations" crates/merry-llm/src/request.rs crates/merry-runtime/src/step.rs
```

If `stable_prefix_hash` and `dynamic_context_hash` are enough for the tests, do not add new public fields. Add extra `append_body_hash` or `continuation_hash` only if a deterministic test cannot state the required behavior clearly with existing hashes.

Implementation note: existing `stable_prefix_hash`,
`dynamic_context_hash`, and `stable_prefix_message_count` diagnostics were
enough. No new public request hash fields were added.

- [x] **Step 3: Add a test that ledger observation alone is not prompt projection**

Use a simple one-tool agent loop and inspect `provider.recorded_requests()[1].messages()`:

```rust
assert_eq!(requests[1].stable_prefix_message_count(), 1);
assert!(
    requests[1]
        .messages()
        .iter()
        .all(|message| !message.content().as_text().contains("tool_result_observation"))
);
assert!(
    requests[1]
        .messages()
        .iter()
        .all(|message| !message.content().as_text().contains("Ledger"))
);
```

The exact forbidden strings can be adjusted to current ledger wording. The point is to lock the principle: recording is not projection.

- [x] **Step 4: Add a test that artifact recording alone is not prompt projection**

Create a runtime-owned artifact, then compile or trigger a provider request without adding an explicit context entry. Assert the artifact payload is absent from request messages:

```rust
let payload = "artifact payload must not enter prompt by default";
// record artifact through the runtime/session helper used by existing tests
// trigger one provider request
assert!(
    request
        .messages()
        .iter()
        .all(|message| !message.content().as_text().contains(payload))
);
```

Use existing artifact helper functions in `crates/merry-runtime/src/runtime.rs` tests if integration-level access is not available from `tests/agent_loop.rs`.

- [x] **Step 5: Do not bless free-form ContextSummary as ordinary projection**

Do not add a test that treats arbitrary `record_context_summary` as ordinary dynamic context. That would bless the wrong boundary.

If an explicit projection API already exists, test that path and assert explicit projection changes dynamic context but not stable prefix:

The assertion must be:

```rust
assert_eq!(
    request_before_context.stable_prefix_hash(),
    request_after_context.stable_prefix_hash()
);
assert_ne!(
    request_before_context.dynamic_context_hash(),
    request_after_context.dynamic_context_hash()
);
```

Also assert the projected text is traceable to the explicit projection API, not to ledger reducer output.

If no explicit projection API exists yet, do not invent one in this task. Record the absence as an implementation note and rely on the negative ledger/artifact projection tests plus Task 5 append-only body and Task 6 project-rules work for this milestone.

Implementation note: no new explicit projection API was introduced in this
slice. The current manual `record_context_summary` API remains documented as an
explicit/raw context write path, while ledger/artifact negative projection tests
cover the default boundary.

- [x] **Step 6: Keep `compile_step_model_request` projection allowlisted**

In `crates/merry-runtime/src/step.rs`, preserve this shape for this slice:

```text
stable prefix:
  system: DEFAULT_RUNTIME_BASE_INSTRUCTIONS
  system: project rules, if loaded by the explicit project-rules layer

checkpoint segment:
  latest ledger-derived checkpoint, if present
  empty checkpoint renders nothing in this plan

append-only body:
  prior user/assistant messages, if Task 5 has implemented message history
  current user input
  explicit context-policy projections

uncheckpointed protocol continuity:
  recent function_call/function_call_output pairs
```

Do not add a dynamic tail marker like `Runtime: pending tool result resolved`.
Do not put ledger observations into the prompt just because they were recorded.
Do not render full artifact payloads unless an explicit tool read/project operation requests them.

- [x] **Step 7: Run tests**

Run:

```bash
cargo test -p merry-runtime agent_loop
cargo test -p merry-runtime context_projection
```

Expected: PASS. If `context_projection` is not the final filter name, run the exact tests added in this task.

Implementation note: there is no `context_projection` filter yet; the exact
added tests are
`ledger_observations_do_not_enter_prompt_context_by_default` and
`artifact_payloads_do_not_enter_prompt_context_by_default` in
`crates/merry-runtime/tests/provider_boundary.rs`.

- [x] **Step 8: Commit**

```bash
git add crates/merry-runtime/src/context.rs crates/merry-llm/src/request.rs crates/merry-runtime/src/step.rs crates/merry-runtime/src/runtime.rs crates/merry-runtime/tests/agent_loop.rs
git commit -m "test(runtime): lock context projection boundaries"
```

Only add files actually changed.

## Task 5: Add Append-Only Message Body Or Mark It Out Of Scope

**Files:**
- Modify: `crates/merry-runtime/src/session.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/src/step.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [x] **Step 1: Choose implementation for this plan**

Preferred implementation for this plan: add a minimal append-only user/assistant body because the spec acceptance says ordinary user messages remain append-only without a Task Anchor.

If this is too large for the current coding-loop slice, explicitly edit this plan before implementation and mark append-only historical body as out-of-scope for this plan. Do not silently leave it ambiguous.

- [x] **Step 2: Add session-owned message history**

Add a runtime-owned history shape in `SessionState`, not a provider-owned conversation id:

```rust
enum SessionMessage {
    User { text: String },
    Assistant { artifact_id: ArtifactId },
}
```

Use owned runtime state and artifact references. Do not store provider response ids.

- [x] **Step 3: Record user input when a loop begins**

When `run_agent_loop` begins an independent user task, append the user text exactly once to the session body. Avoid appending the generated continuation prompt text such as `DEFAULT_AGENT_LOOP_CONTINUATION_INPUT`; that is loop control text, not user conversation history.

- [x] **Step 4: Record assistant output after artifact write succeeds**

When `record_assistant_text_output(text)` succeeds, append an assistant body item referencing that assistant output artifact. The body item must not claim the artifact before it is recorded.

- [x] **Step 5: Compile append-only body into request messages**

Update `compile_step_model_request` to include previous user/assistant body messages before the current loop input where appropriate. If current code already passes the current user text separately, avoid duplicating it in the same request.

The intended request body order is:

```text
stable prefix system messages
optional checkpoint segment, if present
optional explicit context-policy projections
prior append-only user/assistant messages
current user or continuation control input
uncheckpointed function-call continuations
```

- [x] **Step 6: Add tests**

Add tests:

```rust
#[tokio::test(flavor = "current_thread")]
async fn ordinary_user_messages_remain_append_only_without_task_anchor() {
    // Run two completed loops against the same runtime.
    // Assert the second provider request includes the first user message and assistant answer
    // in dynamic messages, while stable_prefix_hash remains unchanged.
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_control_prompt_is_not_recorded_as_user_history() {
    // Run a one-tool loop.
    // Assert DEFAULT_AGENT_LOOP_CONTINUATION_INPUT is not stored as an append-only user message.
}
```

- [x] **Step 7: Run tests**

Run:

```bash
cargo test -p merry-runtime agent_loop
cargo test -p merry-runtime ordinary_user_messages_remain_append_only_without_task_anchor
cargo test -p merry-runtime continuation_control_prompt_is_not_recorded_as_user_history
```

Expected: PASS.

- [x] **Step 8: Commit**

```bash
git add crates/merry-runtime/src/session.rs crates/merry-runtime/src/runtime.rs crates/merry-runtime/src/step.rs crates/merry-runtime/tests/agent_loop.rs
git commit -m "feat(runtime): add append-only message body"
```

Only add files actually changed.

## Task 6: Add Project Rules Stable Prefix Layer

**Files:**
- Modify: `crates/merry-runtime/src/context.rs`
- Modify: `crates/merry-runtime/src/session.rs`
- Modify: `crates/merry-runtime/src/step.rs`
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/tests/debug.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Add a runtime-owned project rules projection**

Add an explicit project-rules layer for stable project instructions such as `AGENTS.md`. This is not ledger projection and not a tool-result summary.

Target shape:

```rust
pub struct ProjectRules {
    source_path: String,
    text: String,
    content_hash: String,
}
```

Keep construction validated: nonblank source path, nonblank text, no control characters except newline/tab.

- [ ] **Step 2: Add rules to the stable prefix**

Update `compile_step_model_request` so loaded project rules become a leading system message included in `stable_prefix_message_count`.

Expected prefix:

```text
system: DEFAULT_RUNTIME_BASE_INSTRUCTIONS
system: project rules from AGENTS.md, if explicitly loaded
```

Because project rules are part of the cacheable prefix, changing AGENTS.md changes `stable_prefix_hash`.

- [ ] **Step 3: Load AGENTS.md for CLI coding-loop fixtures**

For the debug coding-loop task smoke, the CLI may still require the model to inspect `AGENTS.md` through tools for realistic behavior. In addition, load the fixture `AGENTS.md` as project rules before the first provider call so stable instructions are cacheable.

Do not recursively scan parent directories in this task unless already implemented. Use the known disposable fixture root.

- [ ] **Step 4: Add stable-prefix tests**

Add tests:

```rust
#[tokio::test(flavor = "current_thread")]
async fn project_rules_enter_stable_prefix_and_affect_stable_hash() {
    // Build two otherwise identical runtimes with different project rules text.
    // Trigger one provider request each.
    // Assert stable_prefix_hash differs.
    // Assert dynamic_context_hash can remain the same for the same user input.
}

#[tokio::test(flavor = "current_thread")]
async fn ledger_and_artifact_changes_do_not_change_project_rules_stable_hash() {
    // Trigger tool/artifact/ledger changes with project rules loaded.
    // Assert stable_prefix_hash remains stable across those dynamic changes.
}
```

- [ ] **Step 5: Keep live smoke inspect requirement**

Do not remove the live smoke assertion that the model reads `AGENTS.md`. Stable prefix rules help cache and baseline behavior; explicit inspection still proves the coding loop can read exact workspace evidence.

- [ ] **Step 6: Run tests**

Run:

```bash
cargo test -p merry-runtime project_rules
cargo test -p merry-cli coding_loop_task
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/merry-runtime/src/context.rs crates/merry-runtime/src/session.rs crates/merry-runtime/src/step.rs crates/merry-runtime/tests/agent_loop.rs crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
git commit -m "feat(runtime): add project rules stable prefix"
```

Only add files actually changed.

## Task 7: Tighten Live Smoke Event Feedback

**Files:**
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/tests/debug.rs`

- [x] **Step 1: Keep runtime event printing for the debug live smoke**

Inspect the existing event writer:

```bash
rg -n "write_runtime_events|coding-loop-task-live-smoke|RuntimeEvent" crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
```

Ensure `run_debug_coding_loop_task_live_smoke` prints each `RuntimeEvent` as JSONL before or alongside the final `coding-loop-task-live-smoke: ok` line. Do not move this into production behavior; it is for debug smoke only.

- [ ] **Step 2: Add assertions for realistic task behavior**

Partially complete: current code checks required tools, `AGENTS.md`,
`src/lib.rs`, `workspace_patch`, `cargo check -p`, and `cargo test -p`.
It does not yet explicitly require reading `tests/status.rs`, so this remains
unchecked as optional tightening.

Keep or add assertions in `assert_coding_loop_task_live_smoke_tool_sequence` that prove the live model:

```text
read AGENTS.md
read tests/status.rs
read src/lib.rs
called workspace_patch
ran cargo check -p merry_coding_loop_task_status_text
ran cargo test -p merry_coding_loop_task_status_text
completed without pending tool calls
```

If the current assertion requires exact order and creates flakiness, prefer "must contain" assertions with path/tool evidence over exact sequence.

- [ ] **Step 3: Ensure live smoke failure reports are useful**

Partially complete: failed live task smoke writes a failed header, runtime
event JSONL, and process artifact previews. It does not yet include the compact
summary requested here: loop status, step count, pending-call state, missing
observations, and fixture path.

When the live smoke fails after running the loop, the error message should include:

```text
loop status
steps_run
whether pending tool calls remain
which required tool observations were missing
path to .merry/local/coding-loop-task-live-smoke
```

Do not include API keys, provider payloads, or raw large file contents.

- [x] **Step 4: Run deterministic CLI tests**

Covered by later implementation/verification commits for the current smoke
shape, including `75152c9` and `2adf538`.

Run:

```bash
cargo test -p merry-cli coding_loop_task
cargo test -p merry-cli debug_coding_loop_task_live_smoke_requires_with_sandbox_before_config_or_network
```

Expected: PASS.

- [x] **Step 5: Run optional live smoke only when credentials are configured**

The live task lane was exercised during the smoke-correction work; rerun
remains opt-in when credentials and outer bwrap are available.

Run only if local XDG config and `MERRY_OPENAI_DEBUG=1` are intentionally available:

```bash
MERRY_OPENAI_DEBUG=1 ./target/debug/merry --with-sandbox debug coding-loop-task-live-smoke --task status-text
```

Expected: the command prints JSONL runtime events and ends with:

```text
coding-loop-task-live-smoke: ok
```

- [x] **Step 6: Commit**

Relevant commits: `75152c9 chore(cli): dump live task smoke runtime events`
and `2adf538 test(cli): make live coding task smoke realistic`.

```bash
git add crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
git commit -m "test(cli): tighten live coding smoke feedback"
```

## Task 8: Rename And Document The Patch Tool Contract

**Files:**
- Modify: `crates/merry-tool-workspace/src/lib.rs`
- Modify: `crates/merry-tool-workspace/tests/runtime_integration.rs`
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/tests/debug.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [x] **Step 1: Confirm current tool name and schema**

Run:

```bash
rg -n "WORKSPACE_PATCH_TOOL|workspace_patch_file|workspace_patch\"|WorkspacePatchArgs" crates/merry-tool-workspace/src crates/merry-runtime/tests crates/merry-cli/src crates/merry-cli/tests
```

The target model-visible name is:

```text
workspace_patch
```

If `workspace_patch_file` still exists in model-visible tool names, rename it to `workspace_patch` and update tests. If it is already `workspace_patch`, do not rename.

- [x] **Step 2: Keep one patch tool, not two**

The only model-facing write tool for this slice should accept:

```json
{
  "patch": "*** Begin Workspace Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Workspace Patch"
}
```

Do not keep an old `path + old_text + new_text` tool alongside the patch tool unless a deterministic backwards-compatibility test proves the old shape is still used by public callers.

- [x] **Step 3: Update the tool description**

In `workspace_patch_spec()`, keep the description explicit:

```rust
"Apply one Merry workspace patch set to UTF-8 files under configured stable workspace roots. Use workspace-relative paths in *** Update File: ... headers. Include enough unchanged context lines in each hunk to make the preimage unique. Prefer localized hunks; do not submit whole-file content for small edits."
```

- [x] **Step 4: Run schema-related tests**

Run:

```bash
cargo test -p merry-tool-workspace workspace_patch_spec
cargo test -p merry-cli coding_loop_task_live_prompt_delegates_to_default_prompt_and_agents
```

If the first filter finds no tests, run the exact workspace patch parser tests in Task 9 after adding them.

- [x] **Step 5: Commit**

Current code has model-visible `workspace_patch`, the single `patch` string
argument, and an explicit localized-edit description. Covered by
`897184e Advance coding loop patch and context`.

```bash
git add crates/merry-tool-workspace/src/lib.rs crates/merry-tool-workspace/tests/runtime_integration.rs crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs crates/merry-runtime/tests/agent_loop.rs
git commit -m "refactor(workspace): standardize workspace patch tool"
```

Only add files actually changed.

## Task 9: Add Workspace Patch Parser Tests For Standard Patch Shape

**Files:**
- Modify: `crates/merry-tool-workspace/src/lib.rs`

- [ ] **Step 1: Add parser tests**

Equivalent executor-level coverage exists for standard envelope alias and
multi-file patch execution. Dedicated parser-only tests for duplicate update
files and explicit `@@` markers were not found and remain optional hardening.

Inside the existing `#[cfg(test)]` module in `crates/merry-tool-workspace/src/lib.rs`, add tests for:

```rust
#[test]
fn workspace_patch_parser_accepts_multiple_update_files() {
    let patch = "*** Begin Workspace Patch\n\
*** Update File: src/lib.rs\n\
@@\n\
 pub fn status() -> &'static str {\n\
-    \"todo\"\n\
+    \"done\"\n\
 }\n\
*** Update File: tests/status.rs\n\
@@\n\
-    assert_eq!(status(), \"todo\");\n\
+    assert_eq!(status(), \"done\");\n\
*** End Workspace Patch";

    let parsed = parse_workspace_patch(patch).expect("patch should parse");
    assert_eq!(parsed.files.len(), 2);
    assert_eq!(parsed.files[0].path, "src/lib.rs");
    assert_eq!(parsed.files[1].path, "tests/status.rs");
}

#[test]
fn workspace_patch_parser_rejects_duplicate_update_files() {
    let patch = "*** Begin Workspace Patch\n\
*** Update File: src/lib.rs\n\
-todo\n\
+done\n\
*** Update File: src/lib.rs\n\
-old\n\
+new\n\
*** End Workspace Patch";

    let err = parse_workspace_patch(patch).expect_err("duplicate file should fail");
    assert_eq!(
        err.message,
        "workspace patch must not update the same file more than once"
    );
    assert_eq!(err.path.as_deref(), Some("src/lib.rs"));
}

#[test]
fn workspace_patch_parser_accepts_explicit_hunk_markers_without_line_numbers() {
    let patch = "*** Begin Workspace Patch\n\
*** Update File: src/lib.rs\n\
@@\n\
 pub const STATUS: &str = \"todo\";\n\
-pub const LABEL: &str = \"todo\";\n\
+pub const LABEL: &str = \"done\";\n\
*** End Workspace Patch";

    let parsed = parse_workspace_patch(patch).expect("patch should parse");
    assert_eq!(parsed.files.len(), 1);
}
```

Adjust exact helper names if the test module already has constructors for update patches.

- [ ] **Step 2: Run parser tests**

No dedicated `workspace_patch_parser` test filter exists yet.

Run:

```bash
cargo test -p merry-tool-workspace workspace_patch_parser
```

Expected: PASS.

- [ ] **Step 3: Commit**

Parser-only coverage remains optional; do not treat this as the next blocker
unless parser regressions appear.

```bash
git add crates/merry-tool-workspace/src/lib.rs
git commit -m "test(workspace): cover workspace patch parser shape"
```

## Task 10: Improve Hunk Matching For Repeated Text

**Files:**
- Modify: `crates/merry-tool-workspace/src/lib.rs`

- [x] **Step 1: Add a failing repeated-text test**

Repeated-text behavior is covered through executor-level ambiguous preimage
tests and the CLI status fixture, which contains multiple plausible
`value: "todo"` entries.

Add a test that creates a temporary workspace file containing repeated target text and a hunk with surrounding context that should match only one location:

```rust
#[test]
fn workspace_patch_applies_repeated_removed_text_when_context_is_unique() {
    let (_temp, root) = temp_workspace();
    write_utf8(
        &root.join("src/lib.rs"),
        "pub fn first() -> &'static str {\n    \"todo\"\n}\n\npub fn second() -> &'static str {\n    \"todo\"\n}\n",
    );
    let state = patch_state_for(root);
    let args = WorkspacePatchArgs {
        patch: "*** Begin Workspace Patch\n\
*** Update File: src/lib.rs\n\
@@\n\
 pub fn second() -> &'static str {\n\
-    \"todo\"\n\
+    \"done\"\n\
 }\n\
*** End Workspace Patch"
            .to_owned(),
    };

    let outcome = workspace_patch_blocking(&state, args, || false);
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        std::fs::read_to_string(root.join("src/lib.rs")).expect("file should read"),
        "pub fn first() -> &'static str {\n    \"todo\"\n}\n\npub fn second() -> &'static str {\n    \"done\"\n}\n"
    );
}
```

Use existing helper names from the module instead of introducing duplicate helpers if available.

- [x] **Step 2: Run the test and confirm current behavior**

Run:

```bash
cargo test -p merry-tool-workspace workspace_patch_applies_repeated_removed_text_when_context_is_unique
```

Expected: PASS if current context-based `old_text` already handles this. If it fails, fix in Step 3.

- [x] **Step 3: Fix only if needed**

Current hunk matching builds preimage from context plus removed lines in
`build_patch_replacement`, so no line-number parsing is needed for this slice.

If the test fails because matching still sees ambiguity, update `build_patch_replacement`/`build_replacement` so the full hunk preimage includes context and removed lines, not only the removed line. The current implementation should already build `old_text` from context plus removed lines:

```rust
WorkspacePatchLine::Context(text) | WorkspacePatchLine::Remove(text) => Some(text)
```

Do not add line-number parsing unless a failing test proves context-only hunks are insufficient.

- [x] **Step 4: Add an ambiguity test when context is missing**

Covered by `workspace_patch_stale_and_ambiguous_preimages_fail_without_mutation`.

Add:

```rust
#[test]
fn workspace_patch_rejects_repeated_removed_text_without_unique_context() {
    let (_temp, root) = temp_workspace();
    write_utf8(&root.join("src/lib.rs"), "\"todo\"\n\"todo\"\n");
    let state = patch_state_for(root);
    let args = WorkspacePatchArgs {
        patch: "*** Begin Workspace Patch\n\
*** Update File: src/lib.rs\n\
-\"todo\"\n\
+\"done\"\n\
*** End Workspace Patch"
            .to_owned(),
    };

    let outcome = workspace_patch_blocking(&state, args, || false);
    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert!(
        outcome
            .diagnostic()
            .expect("failure should include diagnostic")
            .code()
            .contains("ambiguous")
    );
}
```

Adjust diagnostic code assertion to the current `ERROR_PREIMAGE_AMBIGUOUS` value.

- [x] **Step 5: Run tests**

Run:

```bash
cargo test -p merry-tool-workspace repeated_removed_text
cargo test -p merry-tool-workspace workspace_patch
```

Expected: PASS.

- [x] **Step 6: Commit**

Covered by `897184e Advance coding loop patch and context` and
`2adf538 test(cli): make live coding task smoke realistic`.

```bash
git add crates/merry-tool-workspace/src/lib.rs
git commit -m "test(workspace): require unique patch hunk context"
```

## Task 11: Add Multi-File Patch Execution Coverage

**Files:**
- Modify: `crates/merry-tool-workspace/src/lib.rs`
- Modify: `crates/merry-tool-workspace/tests/runtime_integration.rs`

- [x] **Step 1: Add unit test for multi-file execution**

Covered by `workspace_patch_executor_applies_multi_file_patch_and_records_each_change`.

In `crates/merry-tool-workspace/src/lib.rs`, add a test that writes two files and applies one patch with two `*** Update File:` sections. Assert both files changed and result payload lists two changes.

Use this expected patch shape:

```text
*** Begin Workspace Patch
*** Update File: src/lib.rs
@@
-pub const STATUS: &str = "todo";
+pub const STATUS: &str = "done";
*** Update File: tests/status.rs
@@
-    assert_eq!(STATUS, "todo");
+    assert_eq!(STATUS, "done");
*** End Workspace Patch
```

- [ ] **Step 2: Add integration test through runtime tool execution**

Runtime integration still covers single-file patch in the coding-loop harness.
A dedicated multi-file runtime-tool integration test was not found.

In `crates/merry-tool-workspace/tests/runtime_integration.rs`, add a fake-provider sequence:

```text
tool call workspace_patch with multi-file patch
final text
```

Assert:

```rust
assert_eq!(result.status(), &AgentLoopStatus::Completed);
assert!(src changed);
assert!(test changed);
assert_eq!(patch result status, ToolCallResultStatus::Succeeded);
```

- [x] **Step 3: Decide partial-application behavior and lock it**

Current planner parses/resolves/reads/builds all file plans before execution.
Execution-time stale/missing/ambiguous failures are covered by fail-without
mutation tests for the relevant single-file cases. A dedicated multi-file
partial-write failure test remains optional hardening.

For this MVP, use all-or-nothing planning:

```text
parse every file
resolve every path
read every preimage
build every replacement
only then write files
```

If current code writes earlier files before a later file fails, add a failing test and fix the planner/executor so no files are written until all file plans are valid.

- [x] **Step 4: Run tests**

Run:

```bash
cargo test -p merry-tool-workspace multi_file
cargo test -p merry-tool-workspace runtime_integration
```

Expected: PASS.

- [x] **Step 5: Commit**

Unit-level multi-file coverage is included in `897184e`.

```bash
git add crates/merry-tool-workspace/src/lib.rs crates/merry-tool-workspace/tests/runtime_integration.rs
git commit -m "test(workspace): cover multi-file workspace patches"
```

## Task 12: Update Live Smoke Fixture To Exercise Patch Disambiguation

**Files:**
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/tests/debug.rs`

- [x] **Step 1: Inspect current fixture**

Run:

```bash
rg -n "CodingLoopTaskSmokeTask|status-text|source_satisfies_task|tests/status.rs|AGENTS.md" crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
```

- [x] **Step 2: Make the source contain repeated ordinary text**

Update the `status-text` fixture so `src/lib.rs` contains at least two plausible `"todo"` occurrences, but only one function or entry affects `tests/status.rs`.

Example fixture source:

```rust
pub struct Entry {
    pub key: &'static str,
    pub value: &'static str,
}

pub const ENTRIES: &[Entry] = &[
    Entry { key: "status", value: "todo" },
    Entry { key: "note", value: "todo" },
];

pub fn status_text() -> &'static str {
    ENTRIES
        .iter()
        .find(|entry| entry.key == "status")
        .map(|entry| entry.value)
        .unwrap_or("missing")
}
```

The integration test should require only the `status` entry to become `"done"`.

- [x] **Step 3: Keep the prompt natural**

Do not tell the model the exact path or symbol in the live prompt. Keep:

```text
Fix this disposable Rust project so the required status-text behavior is implemented. Use the available tools to inspect, edit, and verify before reporting completion.
```

Project-specific details belong in `AGENTS.md` and tests, not the user prompt.

- [x] **Step 4: Update deterministic fake provider patch only if needed**

The deterministic fake provider can still provide the exact patch. Its patch should use context:

```text
*** Begin Workspace Patch
*** Update File: src/lib.rs
@@
 pub const ENTRIES: &[Entry] = &[
-    Entry { key: "status", value: "todo" },
+    Entry { key: "status", value: "done" },
     Entry { key: "note", value: "todo" },
 ];
*** End Workspace Patch
```

- [x] **Step 5: Run CLI deterministic tests**

Run:

```bash
cargo test -p merry-cli coding_loop_task_status_text_fixture_forces_disambiguated_localized_patch
cargo test -p merry-cli coding_loop_task_smoke_patches_fixture_and_verifies_with_fake_runner
```

Expected: PASS.

- [x] **Step 6: Commit**

Covered by `2adf538 test(cli): make live coding task smoke realistic`.

```bash
git add crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
git commit -m "test(cli): make coding task patch disambiguation realistic"
```

## Task 13: Add Context Budget Policy Types Without Compaction

**Files:**
- Modify: `crates/merry-runtime/src/context.rs`
- Modify: `crates/merry-runtime/src/lib.rs`
- Modify: `crates/merry-llm/src/capability.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs` or `crates/merry-runtime/src/context.rs`

- [ ] **Step 1: Add runtime-owned budget policy types**

In `crates/merry-runtime/src/context.rs`, add types near `ContextCompiler`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextBudgetPolicy {
    CostAware,
    Balanced,
    Capacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    effective_window_tokens: u64,
    stable_prefix_tokens: u64,
    output_reserve_tokens: u64,
    body_budget_tokens: u64,
    soft_water_tokens: u64,
    hard_water_tokens: u64,
}
```

Add accessors. Validate:

```text
effective_window_tokens > stable_prefix_tokens + output_reserve_tokens
body_budget_tokens = effective_window_tokens - stable_prefix_tokens - output_reserve_tokens
soft_water_tokens < hard_water_tokens
hard_water_tokens <= body_budget_tokens
```

- [ ] **Step 2: Add calculation function**

Add:

```rust
impl ContextBudget {
    pub fn from_window(
        resolved_context_window_tokens: u64,
        effective_context_window_percent: u8,
        stable_prefix_tokens: u64,
        output_reserve_tokens: u64,
        policy: ContextBudgetPolicy,
    ) -> Result<Self, ContextError> {
        // percent must be 1..=100
        // effective_window = window * percent / 100
        // body_budget = effective_window - stable_prefix_tokens - output_reserve
        // soft/hard ratios:
        // CostAware 60/80
        // Balanced 70/90
        // Capacity 85/95
    }
}
```

Keep this pure. Do not wire it into provider calls yet.

- [ ] **Step 3: Add tests for ratios**

Add tests:

```rust
#[test]
fn context_budget_balanced_uses_large_windows_without_step_count_compaction() {
    let budget = ContextBudget::from_window(
        1_000_000,
        95,
        120_000,
        32_000,
        ContextBudgetPolicy::Balanced,
    )
    .expect("budget should calculate");

    assert_eq!(budget.effective_window_tokens(), 950_000);
    assert_eq!(budget.stable_prefix_tokens(), 120_000);
    assert_eq!(budget.body_budget_tokens(), 798_000);
    assert_eq!(budget.soft_water_tokens(), 558_600);
    assert_eq!(budget.hard_water_tokens(), 718_200);
}

#[test]
fn context_budget_rejects_invalid_percent_or_reserve() {
    assert!(ContextBudget::from_window(1_000_000, 0, 0, 32_000, ContextBudgetPolicy::Balanced).is_err());
    assert!(ContextBudget::from_window(1_000, 95, 100, 1_000, ContextBudgetPolicy::Balanced).is_err());
    assert!(ContextBudget::from_window(1_000, 95, 950, 1, ContextBudgetPolicy::Balanced).is_err());
}
```

- [ ] **Step 4: Export only if needed**

If tests or downstream crates need these types, add to `crates/merry-runtime/src/lib.rs`:

```rust
pub use context::{ContextBudget, ContextBudgetPolicy};
```

If they are crate-internal for now, keep them private.

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test -p merry-runtime context_budget
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-runtime/src/context.rs crates/merry-runtime/src/lib.rs
git commit -m "feat(runtime): add context budget watermarks"
```

Only add `lib.rs` if changed.

## Task 14: Resolve Context Window Metadata Conservatively

**Files:**
- Modify: `crates/merry-runtime/src/context.rs`
- Modify: `crates/merry-llm/src/capability.rs`
- Modify: `crates/merry-provider-openai/src/config.rs`

- [ ] **Step 1: Use existing capabilities first**

`ModelCapabilities` already has:

```rust
max_input_tokens: Option<u64>
max_output_tokens: Option<u64>
```

Do not add API probing in this milestone. Use explicit provider capabilities when available, otherwise fallback.

- [ ] **Step 2: Add resolver**

In `crates/merry-runtime/src/context.rs`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextWindowSource {
    ExplicitConfig,
    ProviderCapabilities,
    BundledCatalog,
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedContextWindow {
    tokens: u64,
    source: ContextWindowSource,
}
```

Add a pure function:

```rust
pub fn resolve_context_window(
    explicit_override: Option<u64>,
    provider_capability: Option<u64>,
    bundled_catalog_value: Option<u64>,
    fallback: u64,
) -> Result<ResolvedContextWindow, ContextError>
```

Priority:

```text
explicit config override
provider model metadata/capabilities
bundled model catalog
conservative fallback
```

- [ ] **Step 3: Add resolver tests**

Tests:

```rust
#[test]
fn context_window_resolver_prefers_explicit_config() {
    let resolved = resolve_context_window(Some(1_000_000), Some(200_000), Some(128_000), 64_000)
        .expect("window should resolve");
    assert_eq!(resolved.tokens(), 1_000_000);
    assert_eq!(resolved.source(), ContextWindowSource::ExplicitConfig);
}

#[test]
fn context_window_resolver_falls_back_when_metadata_is_missing() {
    let resolved = resolve_context_window(None, None, None, 64_000)
        .expect("window should resolve");
    assert_eq!(resolved.tokens(), 64_000);
    assert_eq!(resolved.source(), ContextWindowSource::Fallback);
}
```

- [ ] **Step 4: Do not add new config keys yet**

Unless the user explicitly approves a config schema change, do not update `examples/config.toml`.

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test -p merry-runtime context_window_resolver
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-runtime/src/context.rs
git commit -m "feat(runtime): resolve context window budgets"
```

## Task 15: Add Checkpoint Segment And Trigger Decisions Without Checkpoint Content

**Files:**
- Modify: `crates/merry-runtime/src/context.rs`
- Modify: `crates/merry-runtime/src/step.rs`
- Modify: `crates/merry-llm/src/request.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Add an empty checkpoint segment to the compiler shape**

Add a runtime-owned checkpoint slot to the compiled context shape. This slot exists between TaskAnchor/project rules and the append-only body. It may be empty in this plan.

Target shape:

```rust
pub struct ContextCheckpointSegment {
    // Empty for this plan, or an Option<ContextCheckpoint> if the local shape
    // is clearer. Do not store full artifact payloads here.
}
```

If a concrete checkpoint body type is too early, use:

```rust
checkpoint: Option<CompiledCheckpoint>
```

and leave it `None` throughout this implementation. The important acceptance point is structural: an absent checkpoint renders no prompt text, but the compiler has an explicit place where the latest ledger-derived checkpoint will later live.

- [ ] **Step 2: Add checkpoint hash diagnostics**

If `ModelRequest` diagnostics are expanded in this milestone, add a `checkpoint_hash` or make sure `dynamic_context_hash` test coverage can distinguish:

```text
empty checkpoint segment
non-empty future checkpoint segment
append-only body after checkpoint
```

If adding a public hash is premature, keep the slot internal and add a test that an empty checkpoint does not change request messages or stable prefix hash.

- [ ] **Step 3: Add trigger enum**

This task is intentionally only a trigger skeleton. It does not implement checkpoint content generation, checkpoint prompt projection, removal of old raw function-call continuity after checkpoint, or model-assisted summary generation.

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointDecision {
    Continue,
    PlanCheckpoint,
    RequireCheckpoint,
}
```

- [ ] **Step 4: Add pure decision function**

Add:

```rust
pub fn decide_checkpoint(
    dynamic_body_tokens: u64,
    budget: ContextBudget,
) -> CheckpointDecision {
    if dynamic_body_tokens >= budget.hard_water_tokens() {
        CheckpointDecision::RequireCheckpoint
    } else if dynamic_body_tokens >= budget.soft_water_tokens() {
        CheckpointDecision::PlanCheckpoint
    } else {
        CheckpointDecision::Continue
    }
}
```

- [ ] **Step 5: Add trigger tests**

Add:

```rust
#[test]
fn checkpoint_decision_uses_watermarks_not_turn_counts() {
    let budget = ContextBudget::from_window(
        100_000,
        90,
        8_000,
        10_000,
        ContextBudgetPolicy::Balanced,
    )
    .expect("budget should calculate");

    assert_eq!(decide_checkpoint(1, budget), CheckpointDecision::Continue);
    assert_eq!(
        decide_checkpoint(budget.soft_water_tokens(), budget),
        CheckpointDecision::PlanCheckpoint
    );
    assert_eq!(
        decide_checkpoint(budget.hard_water_tokens(), budget),
        CheckpointDecision::RequireCheckpoint
    );
}
```

- [ ] **Step 6: Add empty-segment rendering tests**

Add tests:

```rust
#[test]
fn empty_checkpoint_segment_renders_no_prompt_text() {
    // Compile a request with no checkpoint.
    // Assert request messages contain no checkpoint marker text.
    // Assert stable_prefix_hash is not affected by the absent checkpoint.
}

#[test]
fn checkpoint_segment_is_separate_from_append_only_body() {
    // If the local types expose this structure, assert the compiler can
    // represent checkpoint and append-only body separately.
    // If not exposed yet, keep this as a comment-level invariant in the
    // context compiler tests added in this task.
}
```

- [ ] **Step 7: Keep content generation unwired**

Do not compact prompt history yet. This task only creates deterministic policy primitives for a later compiler integration. It must not be marked as satisfying checkpoint/compaction acceptance from the spec.

The later checkpoint-content plan must separately prove:

```text
checkpoint content is generated from runtime-owned state
checkpoint text references artifacts/evidence instead of replacing exact evidence
unresolved or uncheckpointed function-call continuity remains exact
old raw function-call continuity is removed only after checkpoint is recorded
```

- [ ] **Step 8: Run tests**

Run:

```bash
cargo test -p merry-runtime checkpoint_decision
cargo test -p merry-runtime checkpoint_segment
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/merry-runtime/src/context.rs crates/merry-runtime/src/step.rs crates/merry-llm/src/request.rs crates/merry-runtime/tests/agent_loop.rs
git commit -m "feat(runtime): reserve checkpoint segment"
```

## Task 16: Final Integration Verification

**Files:**
- No required edits unless tests expose a regression.

- [ ] **Step 1: Run formatting**

Run:

```bash
cargo fmt --all --check
```

Expected: PASS.

- [ ] **Step 2: Run focused test suites**

Run:

```bash
cargo test -p merry-runtime agent_loop
cargo test -p merry-runtime context_projection
cargo test -p merry-runtime ordinary_user_messages_remain_append_only_without_task_anchor
cargo test -p merry-runtime project_rules
cargo test -p merry-runtime context_budget
cargo test -p merry-runtime context_window_resolver
cargo test -p merry-runtime checkpoint_decision
cargo test -p merry-tool-workspace workspace_patch
cargo test -p merry-cli coding_loop_task
```

Expected: PASS.

- [ ] **Step 3: Run broad checks if time allows**

Run:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: PASS. If too slow, record that focused checks passed and broad checks remain.

- [ ] **Step 4: Run ignored bwrap smoke when available**

Run:

```bash
cargo test -p merry-cli debug_coding_loop_task_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored
```

Expected: PASS in an environment where nested `bwrap` works.

- [ ] **Step 5: Run live smoke when credentials are available**

Run:

```bash
MERRY_OPENAI_DEBUG=1 ./target/debug/merry --with-sandbox debug coding-loop-task-live-smoke --task status-text
```

Expected:

```text
JSONL RuntimeEvent records show inspect/read/patch/check/test/final
coding-loop-task-live-smoke: ok
```

- [ ] **Step 6: Update public status only with user approval**

If all relevant checks pass and the user approves a status update, update `ROADMAP.md` under `Recently Completed` and `Next Active`. Do not change roadmap priority without explicit user approval.

## Scope Guards

- Do not make `store=true` the default.
- Do not use `previous_response_id` as Merry task state.
- Do not project ledger facts by default just because they were recorded.
- Do not add a per-turn runtime status marker to the prompt.
- Do not add `/task` or TUI commands in this implementation plan.
- Do not broaden shell/process authorization in this plan.
- Do not add arbitrary absolute path editing. Future non-workspace files should enter through explicit authorization views, with file-level or directory-level grants decided by the user.
- Do not commit ignored private docs.

## Open Follow-Ups After This Plan

- Define the durable authorization-view model for temporary file and directory grants.
- Decide whether Task Anchor lives in task ledger or a separate session control plane.
- Design deterministic checkpoint content format backed by ledger facts and artifact refs.
- Decide whether model-assisted checkpoint summaries are useful after deterministic reducers exist.
- Design TUI commands for task pinning, grants, checkpoint visibility, and runtime-event inspection.
