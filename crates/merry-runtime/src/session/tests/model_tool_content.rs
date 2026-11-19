use super::*;
use crate::{FileSessionStore, ToolExecutionOutcome, session::TranscriptItemSnapshot};

const MODEL_BODY: &str = "found: exact result\n";

fn completed_result(full: &str, model: Option<&str>) -> (SessionState, ToolCallResult) {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("compact-result");
    session
        .record_test_tool_call_pending(call.clone())
        .expect("pending call");
    let outcome = ToolExecutionOutcome::succeeded_text(full);
    let outcome = match model {
        Some(body) => outcome.with_model_text(body),
        None => outcome,
    };
    let events = session
        .submit_tool_execution_outcomes(vec![(call.id().clone(), outcome)])
        .expect("resolve");
    let result = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result.clone()),
            _ => None,
        })
        .expect("resolved result");
    (session, result)
}

fn result_body(snapshot: &[TranscriptItemSnapshot]) -> &ToolOutput {
    snapshot
        .iter()
        .find_map(|item| match item {
            TranscriptItemSnapshot::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .expect("tool result in history")
}

#[tokio::test]
async fn model_body_replays_after_save_without_changing_full_output() {
    let full = "full display details ".repeat(200);
    let (mut session, result) = completed_result(&full, Some(MODEL_BODY));
    let store_dir = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(store_dir.path());
    session.save_to(&store).await.expect("save");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("load");
    for state in [&session, &loaded] {
        assert_eq!(
            result_body(&state.provider_transcript_snapshot()),
            &ToolOutput::text(MODEL_BODY)
        );
        assert_eq!(
            result_body(&state.full_transcript_snapshot()),
            &ToolOutput::text(&full)
        );
        assert_eq!(
            state.read_tool_output(result.call_id()),
            Some(ToolOutput::text(&full))
        );
    }
}

#[tokio::test]
async fn omitted_or_equal_model_body_elides_model_output_in_state_json() {
    for model in [None, Some(MODEL_BODY)] {
        let (mut session, _) = completed_result(MODEL_BODY, model);
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(dir.path());
        session.save_to(&store).await.expect("save");
        let document = crate::session::last_commit_json(
            &store.read_state_bytes(&session_id()).await.expect("read"),
        );
        let result = document["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["type"] == "tool_result")
            .expect("result");
        assert!(result.get("model_output").is_none());
        let loaded = SessionState::load_from(&store, &session_id())
            .await
            .expect("state loads");
        assert_eq!(
            result_body(&loaded.provider_transcript_snapshot()),
            &ToolOutput::text(MODEL_BODY)
        );
    }
}

#[test]
fn invalid_model_body_rejects_entire_batch_without_resolving_any_call() {
    let mut session = SessionState::new(session_id());
    let first = pending_tool_call("first");
    let second = pending_tool_call("second");
    session
        .record_test_tool_call_batch_pending(
            PendingToolCallBatch::new(
                ToolCallBatchId::new("compact-batch").expect("id"),
                vec![first.clone(), second.clone()],
            )
            .expect("batch"),
        )
        .expect("pending batch");
    let before = session.full_transcript_snapshot();
    let sequence = session.next_sequence();
    let error = session
        .submit_tool_execution_outcomes(vec![
            (
                first.id().clone(),
                ToolExecutionOutcome::succeeded_text("full first").with_model_text("first"),
            ),
            (
                second.id().clone(),
                ToolExecutionOutcome::succeeded_text("full second").with_model_text(" \n"),
            ),
        ])
        .expect_err("blank model body must fail");
    assert!(matches!(
        error,
        RuntimeError::UnsupportedToolResultContent { .. }
    ));
    assert_eq!(session.next_sequence(), sequence);
    assert_eq!(session.full_transcript_snapshot(), before);
    assert_eq!(session.pending_tool_calls(), vec![first, second]);
}

#[tokio::test]
async fn compaction_window_uses_model_body_and_keeps_full_output() {
    let full = "full original evidence\n".repeat(200);
    let (mut session, result) = completed_result(&full, Some(MODEL_BODY));
    session
        .record_test_user_message_body("continue")
        .expect("retained turn");
    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(None, None, 1).expect("policy"),
        )
        .expect("input")
        .expect("covered turn");
    let window = serde_json::to_value(input.covered_window()).expect("window serializes");
    let exchange = window[0]["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["role"] == "tool_exchange")
        .expect("exchange");
    assert_eq!(exchange["result"]["content"], MODEL_BODY);
    assert!(exchange["result"].get("artifact_id").is_none());
    session
        .install_summary_compaction(input, "Covered history was compacted.")
        .expect("install");
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(dir.path());
    session.save_to(&store).await.expect("save");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("load");
    assert_eq!(
        loaded.read_tool_output(result.call_id()),
        Some(ToolOutput::text(&full))
    );
}

#[test]
fn permission_review_keeps_full_evidence_when_history_uses_a_model_body() {
    let full = "full execution evidence ".repeat(500);
    let (compact, _) = completed_result(&full, Some(MODEL_BODY));
    let (legacy, _) = completed_result(&full, None);
    let review = compact
        .permission_review_context_snapshot()
        .expect("review");
    assert_eq!(review.len(), 1);
    assert_eq!(
        review,
        legacy
            .permission_review_context_snapshot()
            .expect("full review")
    );
    assert_eq!(
        result_body(&compact.provider_transcript_snapshot()),
        &ToolOutput::text(MODEL_BODY)
    );
}

#[test]
fn history_budget_and_compactor_budget_do_not_charge_for_display_only_bytes() {
    let build = |full: &str| {
        let (mut session, _) = completed_result(full, Some(MODEL_BODY));
        session
            .record_test_user_message_body("retain")
            .expect("tail");
        session
            .build_test_citation_compaction_input(
                CitationCompactionPolicy::new(None, None, 1).expect("policy"),
            )
            .expect("input")
            .expect("covered turn")
    };
    let small = build("small display");
    let large = build(&"display only ".repeat(10_000));
    assert_eq!(
        small.covered_payload_token_estimate().expect("estimate"),
        large.covered_payload_token_estimate().expect("estimate")
    );
    let budget = super::rolling_compaction::window_budget(800);
    for full in ["small display".to_owned(), "display only ".repeat(10_000)] {
        let (mut session, _) = completed_result(&full, Some(MODEL_BODY));
        session
            .record_test_user_message_body("retain")
            .expect("tail");
        let plan = session
            .plan_compaction_window(super::rolling_compaction::policy(5), budget)
            .expect("plan");
        assert!(
            plan.is_none(),
            "compact history should fit without collapsing or summarizing display bytes"
        );
    }
}

#[tokio::test]
async fn collapsed_compact_results_keep_notices_and_full_output_after_resume() {
    use crate::compaction::{CompactionCoverageBudget, CompactionPreparation};

    let mut session = SessionState::new(session_id());
    let full = "original retained evidence\n".repeat(100);
    let model = "model output\n".repeat(80);
    for turn in 0..5 {
        let call = pending_tool_call(&format!("archive-model-{turn}"));
        session
            .record_test_tool_call_pending(call.clone())
            .expect("pending call");
        session
            .submit_tool_execution_outcomes(vec![(
                call.id().clone(),
                ToolExecutionOutcome::succeeded_text(&full).with_model_text(&model),
            )])
            .expect("resolve");
    }
    let policy = super::rolling_compaction::policy(5);
    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy,
            policy.resolve(64_000).expect("budget"),
            super::rolling_compaction::window_budget(800),
            CompactionCoverageBudget::limited(0),
        )
        .expect("preparation")
        .expect("collapse needed");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("collapse-only expected");
    };
    let call_id = input
        .window_plan()
        .archived_tool_call_ids()
        .iter()
        .next()
        .expect("collapsed call id")
        .clone();
    session
        .install_archive_only_compaction(input)
        .expect("collapse");
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(dir.path());
    session.save_to(&store).await.expect("save");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("load");
    assert_eq!(
        loaded.read_tool_output(&call_id),
        Some(ToolOutput::text(&full))
    );
    let snapshot = loaded.provider_transcript_snapshot();
    let notice: serde_json::Value =
        serde_json::from_str(result_body(&snapshot).as_str()).expect("JSON");
    assert_eq!(notice["merry_omitted"], true);
    assert_eq!(notice["status"], "succeeded");
    assert_eq!(notice["call_id"], call_id.as_str());
    assert!(notice.get("artifact_id").is_none());
    assert!(notice.get("ref").is_none());
}

#[tokio::test]
async fn tool_result_images_persist_preserve_failures_and_drop_when_collapsed() {
    use crate::compaction::{CompactionCoverageBudget, CompactionPreparation};
    use crate::context::{ContextCompiler, SessionContextSnapshot};
    use crate::step::{StepModelRequestParts, compile_step_model_request};
    use crate::{PromptProfile, StepContext, StepInput, UserImageInput};
    use merry_core::ErrorInfo;
    use merry_llm::{GenerationConfig, ModelInputItem, ModelName};

    let png_bytes = std::sync::Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 42]);
    let snapshot = UserImageInput::png("browser-snapshot.png", png_bytes, 1, 1).expect("valid png");

    // 1. Both succeeded and failed outcomes preserve attached screenshots and persist across save/load.
    let mut session = SessionState::new(session_id());
    let ok_call = pending_tool_call("shot-ok");
    let fail_call = pending_tool_call("shot-fail");
    session
        .record_test_tool_call_batch_pending(
            PendingToolCallBatch::new(
                ToolCallBatchId::new("shot-batch").expect("id"),
                vec![ok_call.clone(), fail_call.clone()],
            )
            .expect("batch"),
        )
        .expect("pending batch");
    session
        .submit_tool_execution_outcomes(vec![
            (
                ok_call.id().clone(),
                ToolExecutionOutcome::succeeded_text("ok snapshot").with_image(snapshot.clone()),
            ),
            (
                fail_call.id().clone(),
                ToolExecutionOutcome::failed_text(
                    "click failed",
                    ErrorInfo::new("click_failed", "element covered").expect("diagnostic"),
                )
                .with_image(snapshot.clone()),
            ),
        ])
        .expect("resolve outcomes with images");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(dir.path());
    session.save_to(&store).await.expect("save");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("load");
    let transcript = loaded.provider_transcript_snapshot();
    let empty_context = ContextCompiler::new()
        .compile(&SessionContextSnapshot::new(Vec::new(), Vec::new(), None))
        .expect("empty context compiles");
    let compiled = compile_step_model_request(StepModelRequestParts {
        input: &StepInput::user_text("continue").expect("input"),
        model: &ModelName::new("test-model").expect("model"),
        skill_catalog: None,
        project_rules: None,
        task_anchor: None,
        plan_control: None,
        context: &empty_context,
        transcript: &transcript,
        tool_specs: Vec::new(),
        generation_config: GenerationConfig::default(),
        prompt_profile: &PromptProfile::default(),
        progress_commentary: false,
        memory_context: None,
    })
    .expect("compile request");

    let model_results = compiled
        .input()
        .iter()
        .filter_map(|item| match item {
            ModelInputItem::ToolResult(result) => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(model_results.len(), 2);
    assert_eq!(model_results[0].images().len(), 1);
    assert_eq!(model_results[0].images()[0].label(), "browser-snapshot.png");
    assert_eq!(model_results[1].images().len(), 1);
    assert_eq!(model_results[1].images()[0].label(), "browser-snapshot.png");
    let _ = StepContext::default();

    // 2. Collapsing older tool results drops screenshots from active provider projection.
    let mut archive_session = SessionState::new(session_id());
    for turn in 0..5 {
        let call = pending_tool_call(&format!("archive-img-{turn}"));
        archive_session
            .record_test_tool_call_pending(call.clone())
            .expect("pending call");
        archive_session
            .submit_tool_execution_outcomes(vec![(
                call.id().clone(),
                ToolExecutionOutcome::succeeded_text("dom ".repeat(200))
                    .with_image(snapshot.clone()),
            )])
            .expect("resolve");
    }
    let policy = super::rolling_compaction::policy(5);
    let preparation = archive_session
        .build_compaction_preparation_with_window_budget(
            policy,
            policy.resolve(64_000).expect("budget"),
            super::rolling_compaction::window_budget(800),
            CompactionCoverageBudget::limited(0),
        )
        .expect("preparation")
        .expect("collapse needed");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("collapse-only expected");
    };
    archive_session
        .install_archive_only_compaction(input)
        .expect("collapse");
    let active_snapshot = archive_session.provider_transcript_snapshot();
    let archived_item = active_snapshot
        .iter()
        .find_map(|item| match item {
            TranscriptItemSnapshot::ToolResult { images, .. } => Some(images),
            _ => None,
        })
        .expect("collapsed tool result present");
    assert!(
        archived_item.is_empty(),
        "collapsed tool result must drop attached images from prompt projection"
    );
    let full_snapshot = archive_session.full_transcript_snapshot();
    let full_item = full_snapshot
        .iter()
        .find_map(|item| match item {
            TranscriptItemSnapshot::ToolResult { images, .. } => Some(images),
            _ => None,
        })
        .expect("full tool result present");
    assert_eq!(full_item.len(), 1);
}

#[test]
fn permission_review_context_preserves_task_anchor_and_user_intent_past_long_tool_runs() {
    let mut session = SessionState::new(session_id());
    session.set_task_anchor(
        crate::TaskAnchor::new("Fix parser bugs and run cargo test.")
            .expect("task anchor should be valid"),
    );
    session
        .record_test_user_message_body("Please fix the bug, fetch missing crates, and run tests.")
        .expect("user message should record");

    for index in 0..16 {
        let call = pending_tool_call(&format!("long-run-tool-{index}"));
        session
            .record_test_tool_call_pending(call.clone())
            .expect("pending call should record");
        session
            .submit_tool_execution_outcomes(vec![(
                call.id().clone(),
                ToolExecutionOutcome::succeeded_text(format!("tool output {index}")),
            )])
            .expect("tool result should record");
    }

    let review = session
        .permission_review_context_snapshot()
        .expect("review snapshot");
    let rendered = format!("{review:?}");
    assert!(
        rendered.contains("task_anchor")
            && rendered.contains("Fix parser bugs and run cargo test."),
        "review context must retain task_anchor: {rendered}"
    );
    assert!(
        rendered.contains("Please fix the bug, fetch missing crates, and run tests."),
        "review context must keep user intent even after >12 tool exchanges: {rendered}"
    );
    assert!(
        rendered.contains("tool output 15"),
        "review context must keep the latest tool output: {rendered}"
    );
    assert!(
        !rendered.contains("tool output 0"),
        "older tool exchanges outside the trailing window should still stay bounded: {rendered}"
    );
}

#[test]
fn permission_review_context_retains_user_intent_after_compaction() {
    let mut session = SessionState::new(session_id());
    session
        .record_test_user_message_body("Install dependencies and verify the build.")
        .expect("user message should record");
    let first_call = pending_tool_call("pre-compaction-call");
    session
        .record_test_tool_call_pending(first_call.clone())
        .expect("pending call should record");
    session
        .submit_tool_execution_outcomes(vec![(
            first_call.id().clone(),
            ToolExecutionOutcome::succeeded_text("first tool output"),
        )])
        .expect("tool result should record");
    let second_call = pending_tool_call("post-compaction-call");
    session
        .record_test_tool_call_pending(second_call.clone())
        .expect("second pending call should record");
    session
        .submit_tool_execution_outcomes(vec![(
            second_call.id().clone(),
            ToolExecutionOutcome::succeeded_text("Could not resolve host: crates.io"),
        )])
        .expect("second tool result should record");

    let input = session
        .build_test_citation_compaction_input(
            CitationCompactionPolicy::new(None, None, 1).expect("policy"),
        )
        .expect("compaction input")
        .expect("covered turn");
    session
        .install_summary_compaction(input, "Earlier turn was compacted.")
        .expect("compaction should install");

    let review = session
        .permission_review_context_snapshot()
        .expect("review snapshot");
    let rendered = format!("{review:?}");
    assert!(
        rendered.contains("Install dependencies and verify the build."),
        "compacted user intent must still be visible to permission review: {rendered}"
    );
    assert!(
        rendered.contains("Could not resolve host: crates.io"),
        "recent uncompacted tool failure must remain visible: {rendered}"
    );
}

#[test]
fn permission_review_context_caps_succeeded_tool_output_tighter_than_failed_diagnostics() {
    let mut session = SessionState::new(session_id());

    let large_arg_call = PendingToolCall::new(
        ToolCallId::new("call-large-args").expect("valid call id"),
        ToolName::new("apply_patch").expect("valid tool name"),
        ToolCallArguments::try_from(serde_json::json!({
            "patch": "A".repeat(4000)
        }))
        .expect("valid arguments"),
    );
    session
        .record_test_tool_call_pending(large_arg_call.clone())
        .expect("pending call should record");
    session
        .submit_tool_execution_outcomes(vec![(
            large_arg_call.id().clone(),
            ToolExecutionOutcome::failed_text(
                format!("{}permission denied sentinel", "x".repeat(400)),
                merry_core::ErrorInfo::new("failed", "failed").expect("valid error"),
            ),
        )])
        .expect("failed tool outcome should record");

    let large_read_call = pending_tool_call("call-large-read");
    session
        .record_test_tool_call_pending(large_read_call.clone())
        .expect("pending read call should record");
    session
        .submit_tool_execution_outcomes(vec![(
            large_read_call.id().clone(),
            ToolExecutionOutcome::succeeded_text(format!(
                "HEAD_PREFIX {} TAIL_DROPPED_MARKER",
                "b".repeat(1000)
            )),
        )])
        .expect("succeeded tool outcome should record");

    let review = session
        .permission_review_context_snapshot()
        .expect("review snapshot");
    assert_eq!(review.len(), 2);
    let failed_entry = format!("{:?}", review[0]);
    let succeeded_entry = format!("{:?}", review[1]);

    assert!(
        failed_entry.contains("result_status:failed")
            && failed_entry.contains("permission denied sentinel"),
        "oversized tool args must not push out result_status or failure diagnostic: {failed_entry}"
    );
    assert!(
        !failed_entry.contains("artifact:"),
        "artifact id noise should be omitted from review context: {failed_entry}"
    );
    assert!(
        succeeded_entry.contains("HEAD_PREFIX") && !succeeded_entry.contains("TAIL_DROPPED_MARKER"),
        "succeeded tool output should stay tightly bounded at 256 bytes: {succeeded_entry}"
    );
}
