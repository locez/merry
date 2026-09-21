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
    for event in &events {
        if let RuntimeJournalPayload::ArtifactRecorded { artifact } = &event.payload {
            session
                .read_artifact_content(artifact.id())
                .expect("recorded artifact is readable");
            assert!(event.sequence < events.last().expect("resolved event").sequence);
        }
    }
    (session, result)
}

fn result_body(snapshot: &[TranscriptItemSnapshot]) -> &ArtifactContent {
    snapshot
        .iter()
        .find_map(|item| match item {
            TranscriptItemSnapshot::ToolResult { content, .. } => Some(content),
            _ => None,
        })
        .expect("tool result in history")
}

#[tokio::test]
async fn model_body_replays_after_save_without_changing_full_evidence() {
    let full = "full display details ".repeat(200);
    let (session, result) = completed_result(&full, Some(MODEL_BODY));
    let store_dir = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(store_dir.path());
    session.save_to(&store).await.expect("save");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("load");
    for state in [&session, &loaded] {
        assert_eq!(
            result_body(
                &state
                    .provider_transcript_snapshot()
                    .expect("provider history")
            ),
            &ArtifactContent::text(MODEL_BODY)
        );
        assert_eq!(
            result_body(&state.full_transcript_snapshot().expect("full history")),
            &ArtifactContent::text(&full)
        );
        assert_eq!(
            state
                .read_artifact_content(result.artifact().id())
                .expect("full evidence"),
            ArtifactContent::text(&full)
        );
    }
}

#[tokio::test]
async fn omitted_model_body_preserves_legacy_history_and_equal_body_reuses_artifact() {
    for model in [None, Some(MODEL_BODY)] {
        let (session, _) = completed_result(MODEL_BODY, model);
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(dir.path());
        session.save_to(&store).await.expect("save");
        let document: serde_json::Value =
            serde_json::from_slice(&store.read_state_bytes(&session_id()).await.expect("read"))
                .expect("JSON");
        let result = document["transcript"]["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["type"] == "tool_result")
            .expect("result");
        assert!(result.get("model_artifact_id").is_none());
        let loaded = SessionState::load_from(&store, &session_id())
            .await
            .expect("legacy-shaped state loads");
        assert_eq!(
            result_body(&loaded.provider_transcript_snapshot().expect("history")),
            &ArtifactContent::text(MODEL_BODY)
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
    let before = session.full_transcript_snapshot().expect("history");
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
    assert_eq!(session.full_transcript_snapshot().expect("history"), before);
    assert_eq!(session.pending_tool_calls(), vec![first, second]);
}

#[tokio::test]
async fn missing_model_artifact_is_rejected_instead_of_silently_using_full_content() {
    let (session, _) = completed_result("full", Some(MODEL_BODY));
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(dir.path());
    session.save_to(&store).await.expect("save");
    let mut document: serde_json::Value =
        serde_json::from_slice(&store.read_state_bytes(&session_id()).await.expect("read"))
            .expect("JSON");
    let result = document["transcript"]["items"]
        .as_array_mut()
        .expect("items")
        .iter_mut()
        .find(|item| item["type"] == "tool_result")
        .expect("result");
    result["model_artifact_id"] = json!("missing-model-body");
    store
        .write_state_bytes(&session_id(), &serde_json::to_vec(&document).expect("JSON"))
        .await
        .expect("write");
    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("missing model evidence");
    assert!(
        error
            .to_string()
            .contains("model tool result artifact is missing")
    );
}

#[tokio::test]
async fn compaction_uses_model_body_but_checkpoint_reads_full_artifact() {
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
    let payload: serde_json::Value =
        serde_json::from_str(&input.to_model_payload_json().expect("payload")).expect("JSON");
    let exchange = payload["window"][0]["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["role"] == "tool_exchange")
        .expect("exchange");
    assert_eq!(exchange["result"]["content"], MODEL_BODY);
    assert_eq!(
        exchange["result"]["artifact_id"],
        result.artifact().id().as_str()
    );
    let reference = input
        .manifest()
        .refs()
        .iter()
        .find(|reference| reference.evidence().artifact_id == *result.artifact().id())
        .expect("full evidence ref")
        .id()
        .clone();
    let candidate = super::rolling_compaction::checkpoint_candidate(reference.as_str());
    session
        .install_citation_compaction_candidate(input, &candidate)
        .expect("install");
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(dir.path());
    session.save_to(&store).await.expect("save");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("load");
    assert_eq!(
        loaded
            .read_checkpoint_ref_page(&reference, 0, 8192)
            .expect("exact page")
            .content(),
        full
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
        result_body(
            &compact
                .provider_transcript_snapshot()
                .expect("model history")
        ),
        &ArtifactContent::text(MODEL_BODY)
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
            "compact history should fit without archiving or summarizing display bytes"
        );
    }
}

#[tokio::test]
async fn archived_compact_results_keep_notices_and_full_evidence_after_resume() {
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
        .expect("archive needed");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("archive-only expected");
    };
    let reference = input.archived_refs().first().expect("archive ref").clone();
    session
        .install_archive_only_compaction(input)
        .expect("archive");
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(dir.path());
    session.save_to(&store).await.expect("save");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("load");
    let page = loaded
        .read_checkpoint_ref_page(reference.id(), 0, 4096)
        .expect("full evidence");
    assert_eq!(page.content(), full);
    let snapshot = loaded
        .provider_transcript_snapshot()
        .expect("model history");
    let notice: serde_json::Value =
        serde_json::from_str(result_body(&snapshot).as_text().expect("notice JSON")).expect("JSON");
    assert_eq!(notice["merry_archived"], true);
    assert_eq!(
        notice["artifact_id"],
        reference.evidence().artifact_id.as_str()
    );
    assert_eq!(notice["ref"], reference.id().as_str());
}
