use crate::{
    FileSessionStore,
    compaction::{CompactionCoverageBudget, CompactionPreparation},
    session::tests::{
        ErrorInfo, PendingToolCallBatch, SessionId, SessionState, ToolCallBatchId, ToolCallResult,
        ToolOutput, TranscriptItem, pending_tool_call,
        rolling_compaction::{
            policy, record_completed_tool_turn, record_completed_user_turn, window_budget,
        },
    },
};

#[test]
fn reverse_tool_results_collapse_by_result_arrival_and_keep_pairs_valid() {
    let mut session =
        SessionState::new(SessionId::new("rolling-reverse-results").expect("valid session id"));
    record_completed_user_turn(&mut session, "old prefix");

    for turn in 3..=6 {
        record_completed_user_turn(&mut session, &format!("small retained {turn}"));
    }

    let tool_turn = session.begin_model_turn().expect("tool turn begins");
    let call_a = pending_tool_call("reverse-call-a");
    let call_b = pending_tool_call("reverse-call-b");
    session
        .record_tool_call_batch_pending(
            tool_turn,
            PendingToolCallBatch::new(
                ToolCallBatchId::new("reverse-batch").expect("valid batch id"),
                vec![call_a.clone(), call_b.clone()],
            )
            .expect("valid batch"),
        )
        .expect("calls record");
    session
        .close_model_response(tool_turn, true)
        .expect("tool response closes");
    for (call, body) in [(&call_b, "b".repeat(1_000)), (&call_a, "a".repeat(1_000))] {
        session
            .submit_tool_result(
                ToolCallResult::succeeded(call.id().clone()),
                ToolOutput::text(body),
            )
            .expect("tool result records");
    }

    let budget = window_budget(450);
    let plan = session
        .plan_compaction_window(policy(5), budget)
        .expect("plan succeeds")
        .expect("old prefix is compressible");
    assert_eq!(
        plan.archived_tool_call_ids_for_tests(),
        vec![call_b.id().clone()],
        "the first arriving result must be collapsed first"
    );

    let resolved = policy(5).resolve(64_000).expect("budget resolves");
    let input = session
        .build_citation_compaction_input_with_window_budget(
            policy(5),
            resolved,
            budget,
            CompactionCoverageBudget::unbounded(),
        )
        .expect("input builds")
        .expect("old prefix is compressible");
    let result_b_ref = session
        .transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::ToolResult { id, call_id, .. } if call_id == call_b.id() => {
                Some(format!("h{}", id.as_u64()))
            }
            _ => None,
        })
        .expect("result B ref exists");
    assert!(
        input
            .manifest()
            .refs()
            .iter()
            .any(|reference| reference.id().as_str() == result_b_ref)
    );

    session
        .install_summary_compaction(input, "The covered prefix was compacted.")
        .expect("checkpoint installs");
    let provider = session.provider_transcript_snapshot();
    let calls = provider
        .iter()
        .filter_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolCall { call } => Some(call.id()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, [call_a.id(), call_b.id()]);
    let notice = provider
        .iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, output, ..
            } if call_id == call_b.id() => Some(output.as_str()),
            _ => None,
        })
        .expect("result B notice exists");
    let notice: serde_json::Value = serde_json::from_str(notice).expect("notice is typed JSON");
    assert_eq!(notice["merry_omitted"], true);
    assert_eq!(notice["status"], "succeeded");
    assert_eq!(notice["call_id"], "reverse-call-b");
    assert!(notice.get("artifact_id").is_none());
    assert!(notice.get("ref").is_none());

    let result_a = provider
        .iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, output, ..
            } if call_id == call_a.id() => Some(output.as_str()),
            _ => None,
        })
        .expect("result A remains visible");
    assert_eq!(result_a, "a".repeat(1_000));
}

#[test]
fn collapses_tried_in_five_turn_plan_do_not_leak_into_three_turn_fallback() {
    let mut session = SessionState::new(SessionId::new("rolling-archive-reset").expect("valid id"));
    record_completed_user_turn(&mut session, "old prefix");
    for turn in 1..=2 {
        let turn_id = session.begin_model_turn().expect("large tool turn begins");
        session
            .record_user_message_body(turn_id, &"discussion".repeat(200))
            .expect("large discussion records");
        let call = pending_tool_call(&format!("dropped-call-{turn}"));
        session
            .record_tool_call_batch_pending(
                turn_id,
                PendingToolCallBatch::new(
                    ToolCallBatchId::new(&format!("dropped-batch-{turn}")).expect("valid batch id"),
                    vec![call.clone()],
                )
                .expect("valid batch"),
            )
            .expect("tool call records");
        session
            .close_model_response(turn_id, true)
            .expect("tool response closes");
        session
            .submit_tool_result(
                ToolCallResult::succeeded(call.id().clone()),
                ToolOutput::text("tool body".repeat(100)),
            )
            .expect("tool result records");
    }
    for turn in 4..=6 {
        record_completed_user_turn(&mut session, &format!("small retained {turn}"));
    }

    let plan = session
        .plan_compaction_window(policy(5), window_budget(400))
        .expect("fallback plan succeeds")
        .expect("three-turn fallback covers the large tool turns");

    assert_eq!(plan.retained_turn_ids_u64(), vec![4, 5, 6]);
    assert!(plan.archived_tool_call_ids().is_empty());
}

#[tokio::test]
async fn collapse_only_retains_full_tool_output_and_round_trips_through_store() {
    let session_id = SessionId::new("rolling-archive-manifest").expect("valid id");
    let mut session = SessionState::new(session_id.clone());
    for turn in 1..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("manifest-call-{turn}"),
            &format!("manifest-result-{turn}"),
            &format!("manifest exact body {turn} {}", "x".repeat(1_000)),
        );
    }
    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
            CompactionCoverageBudget::limited(0),
        )
        .expect("preparation builds")
        .expect("collapse-only is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("expected collapse-only preparation");
    };
    let collapsed_call_id = input
        .window_plan()
        .archived_tool_call_ids()
        .iter()
        .next()
        .expect("one collapsed call id exists")
        .clone();
    session
        .install_archive_only_compaction(input)
        .expect("collapse-only install succeeds");

    assert!(session.compacted_checkpoint.is_none());
    let stored = session
        .read_tool_output(&collapsed_call_id)
        .expect("collapsed tool output remains readable")
        .as_str()
        .to_owned();
    assert!(stored.contains("manifest exact body 1"));

    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    session.save_to(&store).await.expect("session saves");
    let loaded = SessionState::load_from(&store, &session_id)
        .await
        .expect("session loads");
    assert!(loaded.compacted_checkpoint.is_none());
    let loaded_text = loaded
        .read_tool_output(&collapsed_call_id)
        .expect("loaded collapsed tool output remains readable")
        .as_str()
        .to_owned();
    assert_eq!(loaded_text, stored);
}

#[test]
fn failed_collapsed_result_notice_names_the_call_id_without_a_ref() {
    let mut session = SessionState::new(SessionId::new("rolling-failed-notice").expect("valid id"));
    let turn_id = session.begin_model_turn().expect("failed tool turn begins");
    let failed_call = pending_tool_call("failed-notice-call");
    session
        .record_tool_call_batch_pending(
            turn_id,
            PendingToolCallBatch::new(
                ToolCallBatchId::new("failed-notice-batch").expect("valid batch id"),
                vec![failed_call.clone()],
            )
            .expect("valid batch"),
        )
        .expect("failed call records");
    session
        .close_model_response(turn_id, true)
        .expect("tool response closes");
    session
        .submit_tool_result(
            ToolCallResult::failed(
                failed_call.id().clone(),
                ErrorInfo::new("tool_failed", "expected failure").expect("valid diagnostic"),
            ),
            ToolOutput::text("f".repeat(1_000)),
        )
        .expect("failed result records");
    for turn in 2..=5 {
        record_completed_tool_turn(
            &mut session,
            &format!("failed-tail-call-{turn}"),
            &format!("failed-tail-result-{turn}"),
            &"x".repeat(1_000),
        );
    }

    let preparation = session
        .build_compaction_preparation_with_window_budget(
            policy(5),
            policy(5).resolve(64_000).expect("budget resolves"),
            window_budget(1_300),
            CompactionCoverageBudget::limited(0),
        )
        .expect("preparation builds")
        .expect("collapse-only is required");
    let CompactionPreparation::ArchiveToolResults(input) = preparation else {
        panic!("expected collapse-only preparation");
    };
    session
        .install_archive_only_compaction(input)
        .expect("collapse-only install succeeds");

    let notice = session
        .provider_transcript_snapshot()
        .into_iter()
        .find_map(|item| match item {
            crate::session::TranscriptItemSnapshot::ToolResult {
                call_id, output, ..
            } if call_id == *failed_call.id() => Some(output.as_str().to_owned()),
            _ => None,
        })
        .expect("failed notice exists");
    let notice: serde_json::Value = serde_json::from_str(&notice).expect("notice parses");
    let object = notice.as_object().expect("notice is an object");
    assert_eq!(object.len(), 3);
    assert_eq!(notice["merry_omitted"], true);
    assert_eq!(notice["status"], "failed");
    assert_eq!(notice["call_id"], "failed-notice-call");
}
