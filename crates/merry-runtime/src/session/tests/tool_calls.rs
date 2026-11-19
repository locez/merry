use super::*;

#[test]
fn invalid_result_diagnostics_are_rejected_before_publishing_state() {
    use merry_core::{CoreError, ToolCallResultStatus};

    for (status, diagnostic) in [
        (ToolCallResultStatus::Failed, None),
        (
            ToolCallResultStatus::Succeeded,
            Some(
                ErrorInfo::new("unexpected", "success cannot have a diagnostic")
                    .expect("diagnostic"),
            ),
        ),
    ] {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("invalid-result");
        session
            .record_test_tool_call_pending(call.clone())
            .expect("pending call");
        let history = session.full_transcript_snapshot();
        let sequence = session.next_sequence();
        let content = ToolOutput::text("must not be published");
        let error = session
            .submit_tool_execution_outcome(call.id(), status, content, diagnostic, None)
            .expect_err("inconsistent result must fail");
        assert!(matches!(
            error,
            RuntimeError::Core {
                source: CoreError::InvalidToolCallResult { .. }
            }
        ));
        assert_eq!(session.next_sequence(), sequence);
        assert_eq!(session.full_transcript_snapshot(), history);
        assert_eq!(session.pending_tool_calls(), vec![call]);
    }
}

#[test]
fn tool_result_inherits_originating_model_turn() {
    let mut session = SessionState::new(session_id());
    let turn_id = session.begin_model_turn().expect("model turn should begin");
    let call = pending_tool_call("turn-call");
    session
        .record_tool_call_pending(turn_id, call.clone())
        .expect("tool call should record");
    session
        .close_model_response(turn_id, true)
        .expect("tool-call response should close");

    let result = ToolCallResult::succeeded(call.id().clone());
    session
        .submit_tool_result(result, ToolOutput::text("result"))
        .expect("tool result should record");

    let result_turn = session
        .transcript
        .items()
        .iter()
        .find_map(|item| match item {
            TranscriptItem::ToolResult {
                call_id,
                model_turn_id,
                ..
            } if call_id == call.id() => Some(*model_turn_id),
            _ => None,
        })
        .expect("tool result should remain in transcript");
    assert_eq!(result_turn, turn_id);
    assert_eq!(
        session.model_turn_status(turn_id),
        Some(ModelTurnStatus::Completed)
    );
}

#[test]
fn out_of_order_batch_results_complete_originating_turn_only_after_last_result() {
    let mut session = SessionState::new(session_id());
    let turn_id = session.begin_model_turn().expect("model turn should begin");
    let call_a = pending_tool_call("turn-batch-a");
    let call_b = pending_tool_call("turn-batch-b");
    let batch = PendingToolCallBatch::new(
        ToolCallBatchId::new("turn-batch").expect("valid batch id"),
        vec![call_a.clone(), call_b.clone()],
    )
    .expect("valid batch");
    session
        .record_tool_call_batch_pending(turn_id, batch)
        .expect("batch should record");
    session
        .close_model_response(turn_id, true)
        .expect("tool-call response should close");

    session
        .submit_tool_result(
            ToolCallResult::succeeded(call_b.id().clone()),
            ToolOutput::text("b"),
        )
        .expect("second result should record first");
    assert_eq!(
        session.model_turn_status(turn_id),
        Some(ModelTurnStatus::AwaitingToolResults)
    );

    session
        .submit_tool_result(
            ToolCallResult::succeeded(call_a.id().clone()),
            ToolOutput::text("a"),
        )
        .expect("first result should record last");

    assert_eq!(
        session.model_turn_status(turn_id),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(
        session
            .transcript
            .items()
            .iter()
            .filter_map(|item| match item {
                TranscriptItem::ToolResult { model_turn_id, .. } => Some(*model_turn_id),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![turn_id, turn_id]
    );
}

#[test]
fn pending_tool_call_batch_records_atomically_in_model_order() {
    let mut session = SessionState::new(session_id());
    let call_a = pending_tool_call("batch-call-a");
    let call_b = pending_tool_call("batch-call-b");
    let batch = PendingToolCallBatch::new(
        ToolCallBatchId::new("tool-batch-0").expect("valid batch id"),
        vec![call_a.clone(), call_b.clone()],
    )
    .expect("valid batch");

    let event = session
        .record_test_tool_call_batch_pending(batch.clone())
        .expect("batch should record");

    assert!(matches!(
        &event.payload,
        RuntimeJournalPayload::ToolCallBatchPending { batch: recorded } if recorded == &batch
    ));
    assert_eq!(session.pending_tool_calls(), vec![call_a, call_b]);
    assert_eq!(
        session.transcript_items_for_tests(),
        [
            "tool_call:batch-call-a".to_owned(),
            "tool_call:batch-call-b".to_owned(),
        ]
    );
}

#[test]
fn pending_tool_call_batch_conflict_leaves_session_unchanged() {
    let mut session = SessionState::new(session_id());
    let existing = pending_tool_call("batch-conflict");
    session
        .record_test_tool_call_pending(existing.clone())
        .expect("existing call should record");
    let pending_before = session.pending_tool_calls();
    let transcript_before = session.transcript_items_for_tests();
    let sequence_before = session.next_sequence();

    let batch = PendingToolCallBatch::new(
        ToolCallBatchId::new("tool-batch-conflict").expect("valid batch id"),
        vec![pending_tool_call("batch-new"), existing],
    )
    .expect("batch shape should be valid");
    let error = session
        .record_test_tool_call_batch_pending(batch)
        .expect_err("conflicting batch should reject");

    assert_eq!(error.code(), "tool_call_duplicate");
    assert_eq!(session.pending_tool_calls(), pending_before);
    assert_eq!(session.transcript_items_for_tests(), transcript_before);
    assert_eq!(session.next_sequence(), sequence_before);
}

#[test]
fn submit_tool_result_starts_session_before_resolution_when_needed() {
    let mut session = SessionState::new(session_id());
    let pending = pending_tool_call("pre-start-call");
    let turn_id = session.begin_model_turn().expect("model turn should begin");
    session
        .transcript
        .push_tool_call(turn_id, pending.clone())
        .expect("fixture call should enter transcript");
    session
        .close_model_response(turn_id, true)
        .expect("fixture response should close");
    session.pending_tool_calls.push(pending);
    let result = ToolCallResult::succeeded(tool_call_id("pre-start-call"));

    let events = session
        .submit_tool_result(result.clone(), ToolOutput::json(r#"{"ok":true}"#))
        .expect("tool result should submit");

    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(matches!(
        events[0].payload,
        RuntimeJournalPayload::SessionStarted
    ));
    assert!(matches!(
        &events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == &result
    ));
}

#[test]
fn duplicate_tool_call_pending_is_rejected_without_second_pending_or_sequence() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("duplicate-call");
    let first = session
        .record_test_tool_call_pending(call.clone())
        .expect("first pending call should record");

    let err = session
        .record_test_tool_call_pending(call.clone())
        .expect_err("duplicate pending call id should be rejected");

    assert_eq!(err.code(), "tool_call_duplicate");
    assert_eq!(session.pending_tool_calls(), vec![call]);
    assert_eq!(first.sequence, 0);
    assert_eq!(session.next_sequence(), 1);
}
