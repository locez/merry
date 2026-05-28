# Minimal Useful Coding Loop Continuity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the live coding-loop task behave like a real stateless-provider coding agent by preserving hot function-call continuity, keeping ledger/artifact state out of default prompt projection, improving patch reliability, and adding budget/checkpoint guardrails without turning provider conversation state into Merry runtime state.

**Architecture:** Runtime remains the source of truth for session state, artifacts, ledger facts, continuations, and context compilation. Provider calls stay stateless by default with `store=false`; Merry replays the recent function-call/output continuity required by the provider protocol while exact evidence remains artifact-backed. The work is split into small vertical slices so each milestone has deterministic tests before live smoke validation.

**Tech Stack:** Rust 2024, Tokio, `merry-runtime`, `merry-llm`, `merry-tool-workspace`, `merry-cli`, OpenAI-compatible Responses provider, deterministic fake providers/runners, opt-in `bwrap` and live-provider debug smokes.

---

## Design Inputs

- Roadmap focus: `ROADMAP.md` keeps P0 on the Minimal Useful Coding Loop.
- Context strategy: `specs/2026-05-28-context-assembly-and-ledger-strategy.md`.
- Current live smoke command: `MERRY_OPENAI_DEBUG=1 ./target/debug/merry --with-sandbox debug coding-loop-task-live-smoke --task status-text`.
- Current core rule: record aggressively, project conservatively, rewrite rarely.
- Non-negotiable boundary: do not use provider `previous_response_id` or `store=true` as Merry runtime state.

## File Structure

- Modify `crates/merry-runtime/src/session.rs`: rename or clarify hot continuation storage, preserve all hot resolved tool continuations across a single loop, and expose clear/reset operations for terminal/checkpoint boundaries.
- Modify `crates/merry-runtime/src/runtime.rs`: stop consuming tool continuations when the model emits the next tool call; clear hot continuations only after terminal assistant completion or explicit future checkpoint; extend provider request trace fields as needed.
- Modify `crates/merry-runtime/src/step.rs`: keep request assembly layered as stable base instructions plus optional compiled context plus current user/continuation input; avoid ledger projection in this slice.
- Modify `crates/merry-llm/src/request.rs`: expose any missing request diagnostics needed by compiler/cache tests, such as dynamic message hash versus continuation hash if required.
- Modify `crates/merry-runtime/tests/agent_loop.rs`: add deterministic multi-tool continuity tests and context/cache diagnostic tests.
- Modify `crates/merry-cli/src/main.rs`: keep debug smoke runtime-event printing for the live task smoke and tighten live-smoke assertions around realistic coding behavior.
- Modify `crates/merry-cli/tests/debug.rs`: update deterministic CLI smoke tests when live-smoke acceptance changes affect shared helper behavior.
- Modify `crates/merry-tool-workspace/src/lib.rs`: upgrade `workspace_patch` parser/planner/executor behavior for multi-file patch and disambiguated hunks.
- Modify `crates/merry-tool-workspace/tests/runtime_integration.rs`: add runtime-level patch integration coverage for repeated text, multi-file patching, and failure behavior.
- Optional modify `crates/merry-provider-openai/src/render.rs`: only if rendering tests show the provider adapter loses function-call/output order.
- Optional modify `examples/config.toml`: only if accepted config keys change. This plan should not introduce config keys until the budget policy slice.
- Do not modify ignored `docs/` or `merry-raw-docs/`.
- Do not update `ROADMAP.md` unless the user explicitly approves a roadmap/status edit.

## Milestones

1. **M1: Hot Function-Call Continuity**
   Preserve exact recent tool call/output pairs across multiple model steps under `store=false`.
2. **M2: Context Compiler Layer Diagnostics**
   Lock in stable-prefix versus dynamic-body behavior and prove ledger updates are not projected by default.
3. **M3: Live Coding Smoke Feedback Loop**
   Use the realistic `status-text` task to verify inspect/read/patch/check/test/final behavior and print enough runtime events to debug failures.
4. **M4: Diff-Style Workspace Patch Reliability**
   Make `workspace_patch` robust for localized edits, repeated text, and multi-file patches.
5. **M5: Context Budget And Checkpoint Skeleton**
   Add model-window-aware budget diagnostics and checkpoint trigger decisions without doing model-generated summaries.

Each milestone must be committed separately unless the user says otherwise.

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

- [ ] **Step 1: Add the deterministic test**

Add this test near `agent_loop_executes_one_tool_and_continues_to_final_completion`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_preserves_all_hot_tool_continuations_until_final_answer() {
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
    let runtime = runtime_with_tool("agent-loop-hot-continuity", provider.clone(), executor);

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
        "adding the second hot continuation should change only dynamic request context"
    );
    assert_eq!(
        requests[1].stable_prefix_hash(),
        requests[2].stable_prefix_hash(),
        "hot continuation growth must not move the cacheable stable prefix"
    );
}
```

- [ ] **Step 2: Run the focused test and confirm it fails**

Run:

```bash
cargo test -p merry-runtime agent_loop_preserves_all_hot_tool_continuations_until_final_answer
```

Expected before implementation: FAIL because the third provider request only includes the latest continuation or otherwise does not include both prior call/result pairs.

## Task 2: Preserve Hot Continuations Until Terminal Completion

**Files:**
- Modify: `crates/merry-runtime/src/session.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Clarify session continuation APIs**

In `crates/merry-runtime/src/session.rs`, keep the existing storage initially, but add explicit methods so call sites describe the lifecycle:

```rust
    pub(crate) fn hot_tool_continuation_snapshots(
        &self,
    ) -> Result<Vec<ResolvedToolContinuationSnapshot>, ArtifactError> {
        self.unconsumed_tool_continuation_snapshots()
    }

    pub(crate) fn clear_hot_tool_continuations(&mut self) {
        self.unconsumed_tool_continuations.clear();
    }
```

Keep `unconsumed_tool_continuation_snapshots` and `consume_tool_continuations` for this task if removing them would create broad churn. Add a short comment above the new methods explaining that "hot" continuations are protocol continuity and not ledger projection.

- [ ] **Step 2: Use hot snapshots when compiling the provider request**

In `crates/merry-runtime/src/runtime.rs`, replace:

```rust
let continuations = match session.unconsumed_tool_continuation_snapshots() {
```

with:

```rust
let continuations = match session.hot_tool_continuation_snapshots() {
```

Expected behavior is unchanged at this point.

- [ ] **Step 3: Stop clearing continuations when the model asks for another tool**

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

This is the core fix: a new pending tool call should not erase earlier resolved tool continuations while the loop is still hot.

- [ ] **Step 4: Clear hot continuations only after terminal assistant text is durably recorded**

In `send_assistant_text_output_completed_events`, replace:

```rust
session.consume_tool_continuations(sent_continuation_count);
```

with:

```rust
session.clear_hot_tool_continuations();
```

Keep the clear after `record_assistant_text_output(text)` succeeds. If recording assistant output fails, do not clear the continuity state.

- [ ] **Step 5: Remove now-unused count plumbing**

If `sent_continuation_count` is only used for assistant completion, keep it there until the code compiles, then remove it if unused. The provider request trace should still record the count that was sent:

```rust
trace_provider_request(provider.name().as_str(), &request, sent_continuation_count);
```

- [ ] **Step 6: Run focused runtime tests**

Run:

```bash
cargo test -p merry-runtime agent_loop_preserves_all_hot_tool_continuations_until_final_answer
cargo test -p merry-runtime agent_loop_executes_one_tool_and_continues_to_final_completion
cargo test -p merry-runtime unregistered_tool_resolves_failed_and_continues_once
cargo test -p merry-runtime denied_registered_tool_resolves_failed_and_agent_loop_continues_once
```

Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/merry-runtime/src/session.rs crates/merry-runtime/src/runtime.rs crates/merry-runtime/tests/agent_loop.rs
git commit -m "fix(runtime): preserve hot tool continuations"
```

## Task 3: Prove Hot Continuations Are Cleared After Final Answer

**Files:**
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Add a post-final request test**

Add this test after the previous continuity test:

```rust
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_clears_hot_tool_continuations_after_final_answer() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-first",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("first final"))],
        vec![Ok(completed_text_event("second final"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool("agent-loop-hot-continuity-clear", provider.clone(), executor);

    let first = run_default_loop(&runtime, "Search once.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);

    let second = run_default_loop(&runtime, "Answer without previous tool result.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert!(
        requests[2].continuations().is_empty(),
        "terminal assistant completion should clear hot protocol continuity for the next independent loop"
    );
}
```

- [ ] **Step 2: Run the test**

Run:

```bash
cargo test -p merry-runtime agent_loop_clears_hot_tool_continuations_after_final_answer
```

Expected: PASS.

- [ ] **Step 3: Commit if this test was not included in Task 2**

```bash
git add crates/merry-runtime/tests/agent_loop.rs
git commit -m "test(runtime): cover hot continuation cleanup"
```

Skip this commit if Task 2 already committed this test.

## Task 4: Add Context/Cache Diagnostics Without Ledger Projection

**Files:**
- Modify: `crates/merry-llm/src/request.rs`
- Modify: `crates/merry-runtime/src/step.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Check whether extra hashes are needed**

Inspect `ModelRequest`:

```bash
rg -n "stable_prefix_hash|dynamic_context_hash|stable_prefix_message_count|continuations" crates/merry-llm/src/request.rs crates/merry-runtime/src/step.rs
```

If `stable_prefix_hash` and `dynamic_context_hash` are enough for the tests, do not add new public fields. Add extra `append_body_hash` or `continuation_hash` only if a deterministic test cannot state the required behavior clearly with existing hashes.

- [ ] **Step 2: Add a test that context summaries affect dynamic context, not stable prefix**

Add a test in `crates/merry-runtime/tests/agent_loop.rs` using existing `record_context_summary` helpers only if the summary can be backed by a real artifact. If the integration test setup is too heavy, place the lower-level test in `crates/merry-runtime/src/runtime.rs` tests where artifact helpers already exist.

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

Do not assert that ledger facts appear in messages.

- [ ] **Step 3: Add a test that tool-result ledger recording alone is not rendered as a new system message**

Use a simple one-tool agent loop and inspect `provider.recorded_requests()[1].messages()`:

```rust
assert_eq!(requests[1].stable_prefix_message_count(), 1);
assert!(
    requests[1]
        .messages()
        .iter()
        .all(|message| !message.content().as_text().contains("tool_result_observation"))
);
```

The exact forbidden string can be adjusted to the current ledger debug wording. The point is to lock the principle: recording is not projection.

- [ ] **Step 4: Keep `compile_step_model_request` simple**

In `crates/merry-runtime/src/step.rs`, preserve this shape:

```rust
system: DEFAULT_RUNTIME_BASE_INSTRUCTIONS
optional system: compiled context snapshot
user: current step input
continuations: recent function_call/function_call_output pairs
stable_prefix_message_count: 1
```

Do not add a dynamic tail marker like `Runtime: pending tool result resolved`.

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test -p merry-runtime agent_loop
cargo test -p merry-runtime context
```

Expected: PASS. If `context` is not a valid filter for the intended tests, run the exact test names added in this task.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-llm/src/request.rs crates/merry-runtime/src/step.rs crates/merry-runtime/tests/agent_loop.rs crates/merry-runtime/src/runtime.rs
git commit -m "test(runtime): lock context compiler projection boundaries"
```

Only add files actually changed.

## Task 5: Tighten Live Smoke Event Feedback

**Files:**
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/tests/debug.rs`

- [ ] **Step 1: Keep runtime event printing for the debug live smoke**

Inspect the existing event writer:

```bash
rg -n "write_runtime_events|coding-loop-task-live-smoke|RuntimeEvent" crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
```

Ensure `run_debug_coding_loop_task_live_smoke` prints each `RuntimeEvent` as JSONL before or alongside the final `coding-loop-task-live-smoke: ok` line. Do not move this into production behavior; it is for debug smoke only.

- [ ] **Step 2: Add assertions for realistic task behavior**

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

When the live smoke fails after running the loop, the error message should include:

```text
loop status
steps_run
whether pending tool calls remain
which required tool observations were missing
path to .merry/local/coding-loop-task-live-smoke
```

Do not include API keys, provider payloads, or raw large file contents.

- [ ] **Step 4: Run deterministic CLI tests**

Run:

```bash
cargo test -p merry-cli coding_loop_task
cargo test -p merry-cli debug_coding_loop_task_live_smoke_requires_with_sandbox_before_config_or_network
```

Expected: PASS.

- [ ] **Step 5: Run optional live smoke only when credentials are configured**

Run only if local XDG config and `MERRY_OPENAI_DEBUG=1` are intentionally available:

```bash
MERRY_OPENAI_DEBUG=1 ./target/debug/merry --with-sandbox debug coding-loop-task-live-smoke --task status-text
```

Expected: the command prints JSONL runtime events and ends with:

```text
coding-loop-task-live-smoke: ok
```

- [ ] **Step 6: Commit**

```bash
git add crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
git commit -m "test(cli): tighten live coding smoke feedback"
```

## Task 6: Rename And Document The Patch Tool Contract

**Files:**
- Modify: `crates/merry-tool-workspace/src/lib.rs`
- Modify: `crates/merry-tool-workspace/tests/runtime_integration.rs`
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/tests/debug.rs`
- Modify: `crates/merry-runtime/tests/agent_loop.rs`

- [ ] **Step 1: Confirm current tool name and schema**

Run:

```bash
rg -n "WORKSPACE_PATCH_TOOL|workspace_patch_file|workspace_patch\"|WorkspacePatchArgs" crates/merry-tool-workspace/src crates/merry-runtime/tests crates/merry-cli/src crates/merry-cli/tests
```

The target model-visible name is:

```text
workspace_patch
```

If `workspace_patch_file` still exists in model-visible tool names, rename it to `workspace_patch` and update tests. If it is already `workspace_patch`, do not rename.

- [ ] **Step 2: Keep one patch tool, not two**

The only model-facing write tool for this slice should accept:

```json
{
  "patch": "*** Begin Workspace Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Workspace Patch"
}
```

Do not keep an old `path + old_text + new_text` tool alongside the patch tool unless a deterministic backwards-compatibility test proves the old shape is still used by public callers.

- [ ] **Step 3: Update the tool description**

In `workspace_patch_spec()`, keep the description explicit:

```rust
"Apply one Merry workspace patch set to UTF-8 files under configured stable workspace roots. Use workspace-relative paths in *** Update File: ... headers. Include enough unchanged context lines in each hunk to make the preimage unique. Prefer localized hunks; do not submit whole-file content for small edits."
```

- [ ] **Step 4: Run schema-related tests**

Run:

```bash
cargo test -p merry-tool-workspace workspace_patch_spec
cargo test -p merry-cli coding_loop_task_live_prompt_delegates_to_default_prompt_and_agents
```

If the first filter finds no tests, run the exact workspace patch parser tests in Task 7 after adding them.

- [ ] **Step 5: Commit**

```bash
git add crates/merry-tool-workspace/src/lib.rs crates/merry-tool-workspace/tests/runtime_integration.rs crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs crates/merry-runtime/tests/agent_loop.rs
git commit -m "refactor(workspace): standardize workspace patch tool"
```

Only add files actually changed.

## Task 7: Add Workspace Patch Parser Tests For Standard Patch Shape

**Files:**
- Modify: `crates/merry-tool-workspace/src/lib.rs`

- [ ] **Step 1: Add parser tests**

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

Run:

```bash
cargo test -p merry-tool-workspace workspace_patch_parser
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/merry-tool-workspace/src/lib.rs
git commit -m "test(workspace): cover workspace patch parser shape"
```

## Task 8: Improve Hunk Matching For Repeated Text

**Files:**
- Modify: `crates/merry-tool-workspace/src/lib.rs`

- [ ] **Step 1: Add a failing repeated-text test**

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

- [ ] **Step 2: Run the test and confirm current behavior**

Run:

```bash
cargo test -p merry-tool-workspace workspace_patch_applies_repeated_removed_text_when_context_is_unique
```

Expected: PASS if current context-based `old_text` already handles this. If it fails, fix in Step 3.

- [ ] **Step 3: Fix only if needed**

If the test fails because matching still sees ambiguity, update `build_patch_replacement`/`build_replacement` so the full hunk preimage includes context and removed lines, not only the removed line. The current implementation should already build `old_text` from context plus removed lines:

```rust
WorkspacePatchLine::Context(text) | WorkspacePatchLine::Remove(text) => Some(text)
```

Do not add line-number parsing unless a failing test proves context-only hunks are insufficient.

- [ ] **Step 4: Add an ambiguity test when context is missing**

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

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test -p merry-tool-workspace repeated_removed_text
cargo test -p merry-tool-workspace workspace_patch
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-tool-workspace/src/lib.rs
git commit -m "test(workspace): require unique patch hunk context"
```

## Task 9: Add Multi-File Patch Execution Coverage

**Files:**
- Modify: `crates/merry-tool-workspace/src/lib.rs`
- Modify: `crates/merry-tool-workspace/tests/runtime_integration.rs`

- [ ] **Step 1: Add unit test for multi-file execution**

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

- [ ] **Step 3: Decide partial-application behavior and lock it**

For this MVP, use all-or-nothing planning:

```text
parse every file
resolve every path
read every preimage
build every replacement
only then write files
```

If current code writes earlier files before a later file fails, add a failing test and fix the planner/executor so no files are written until all file plans are valid.

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p merry-tool-workspace multi_file
cargo test -p merry-tool-workspace runtime_integration
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/merry-tool-workspace/src/lib.rs crates/merry-tool-workspace/tests/runtime_integration.rs
git commit -m "test(workspace): cover multi-file workspace patches"
```

## Task 10: Update Live Smoke Fixture To Exercise Patch Disambiguation

**Files:**
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/tests/debug.rs`

- [ ] **Step 1: Inspect current fixture**

Run:

```bash
rg -n "CodingLoopTaskSmokeTask|status-text|source_satisfies_task|tests/status.rs|AGENTS.md" crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
```

- [ ] **Step 2: Make the source contain repeated ordinary text**

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

- [ ] **Step 3: Keep the prompt natural**

Do not tell the model the exact path or symbol in the live prompt. Keep:

```text
Fix this disposable Rust project so the required status-text behavior is implemented. Use the available tools to inspect, edit, and verify before reporting completion.
```

Project-specific details belong in `AGENTS.md` and tests, not the user prompt.

- [ ] **Step 4: Update deterministic fake provider patch only if needed**

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

- [ ] **Step 5: Run CLI deterministic tests**

Run:

```bash
cargo test -p merry-cli coding_loop_task_status_text_fixture_forces_disambiguated_localized_patch
cargo test -p merry-cli coding_loop_task_smoke_patches_fixture_and_verifies_with_fake_runner
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
git commit -m "test(cli): make coding task patch disambiguation realistic"
```

## Task 11: Add Context Budget Policy Types Without Compaction

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
    output_reserve_tokens: u64,
    soft_water_tokens: u64,
    hard_water_tokens: u64,
}
```

Add accessors. Validate:

```text
effective_window_tokens > output_reserve_tokens
soft_water_tokens < hard_water_tokens
hard_water_tokens <= effective_window_tokens - output_reserve_tokens
```

- [ ] **Step 2: Add calculation function**

Add:

```rust
impl ContextBudget {
    pub fn from_window(
        resolved_context_window_tokens: u64,
        effective_context_window_percent: u8,
        output_reserve_tokens: u64,
        policy: ContextBudgetPolicy,
    ) -> Result<Self, ContextError> {
        // percent must be 1..=100
        // effective_window = window * percent / 100
        // body_budget = effective_window - output_reserve
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
        32_000,
        ContextBudgetPolicy::Balanced,
    )
    .expect("budget should calculate");

    assert_eq!(budget.effective_window_tokens(), 950_000);
    assert_eq!(budget.body_budget_tokens(), 918_000);
    assert_eq!(budget.soft_water_tokens(), 642_600);
    assert_eq!(budget.hard_water_tokens(), 826_200);
}

#[test]
fn context_budget_rejects_invalid_percent_or_reserve() {
    assert!(ContextBudget::from_window(1_000_000, 0, 32_000, ContextBudgetPolicy::Balanced).is_err());
    assert!(ContextBudget::from_window(1_000, 95, 1_000, ContextBudgetPolicy::Balanced).is_err());
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

## Task 12: Resolve Context Window Metadata Conservatively

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

## Task 13: Add Checkpoint Trigger Decisions Without Checkpoint Content

**Files:**
- Modify: `crates/merry-runtime/src/context.rs`

- [ ] **Step 1: Add trigger enum**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointDecision {
    Continue,
    PlanCheckpoint,
    RequireCheckpoint,
}
```

- [ ] **Step 2: Add pure decision function**

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

- [ ] **Step 3: Add tests**

Add:

```rust
#[test]
fn checkpoint_decision_uses_watermarks_not_turn_counts() {
    let budget = ContextBudget::from_window(
        100_000,
        90,
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

- [ ] **Step 4: Keep it unwired**

Do not compact prompt history yet. This task only creates deterministic policy primitives for a later compiler integration.

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test -p merry-runtime checkpoint_decision
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/merry-runtime/src/context.rs
git commit -m "feat(runtime): add checkpoint watermark decisions"
```

## Task 14: Final Integration Verification

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
