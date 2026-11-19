use super::*;

#[test]
fn session_start_is_recorded_once_before_step_lifecycle() {
    let mut session = SessionState::new(session_id());

    let first = session
        .record_session_started_if_needed()
        .expect("first start should emit");
    let second = session.record_session_started_if_needed();
    let started = session.record_step_started();
    let completed = session.record_step_completed();

    assert!(matches!(
        first.payload,
        RuntimeJournalPayload::SessionStarted
    ));
    assert!(second.is_none());
    assert_eq!(started.sequence, 1);
    assert_eq!(completed.sequence, 2);
}

#[test]
fn assistant_output_records_text_with_monotonic_sequence() {
    let mut session = SessionState::new(session_id());
    let _started = session
        .record_session_started_if_needed()
        .expect("start should emit");
    let _step_started = session.record_step_started();

    let output = session
        .record_test_assistant_text_output("hello".to_owned())
        .expect("assistant output should record");
    let completed = session.record_step_completed();

    match output.payload {
        RuntimeJournalPayload::AssistantOutputRecorded { text } => {
            assert_eq!(text, "hello");
        }
        other => panic!("expected assistant output event, got {other:?}"),
    }
    assert_eq!(output.sequence, 2);
    assert_eq!(completed.sequence, 3);
}

#[test]
fn assistant_output_is_stored_in_transcript_before_event_is_observable() {
    let mut session = SessionState::new(session_id());

    let event = session
        .record_test_assistant_text_output("stored assistant".to_owned())
        .expect("assistant output should record");

    let RuntimeJournalPayload::AssistantOutputRecorded { text } = &event.payload else {
        panic!("expected assistant output event, got {:?}", event.payload);
    };
    assert_eq!(text, "stored assistant");
    assert_eq!(
        session.transcript_items_for_tests(),
        vec!["assistant:stored assistant"]
    );
}

#[test]
fn submit_tool_result_stores_exact_content_before_resolved_event() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("call-1");
    session
        .record_test_tool_call_pending(call.clone())
        .expect("pending call should record");
    let result = ToolCallResult::succeeded(call.id().clone());

    let events = session
        .submit_tool_result(result.clone(), ToolOutput::text("exact result\n"))
        .expect("tool result should submit");

    assert_eq!(
        session.read_tool_output(call.id()),
        Some(ToolOutput::text("exact result\n"))
    );
    assert!(matches!(
        events.last().map(|event| &event.payload),
        Some(RuntimeJournalPayload::ToolCallResolved { result: resolved }) if resolved == &result
    ));
}

#[test]
fn session_transcript_records_messages_and_tool_exchange_in_order() {
    let mut session =
        SessionState::new(SessionId::new("transcript-order").expect("valid session id"));
    session
        .record_test_user_message_body("first user")
        .expect("user records");
    session
        .record_test_assistant_text_output("first assistant".to_owned())
        .expect("assistant output records");

    let call = pending_tool_call("call-history");
    session
        .record_test_tool_call_pending(call.clone())
        .expect("tool call pending records");
    let result = ToolCallResult::succeeded(call.id().clone());
    session
        .submit_tool_result(result, ToolOutput::text("tool result"))
        .expect("tool result records");

    assert_eq!(
        session.transcript_items_for_tests(),
        vec![
            "user:first user",
            "assistant:first assistant",
            "tool_call:call-history",
            "tool_result:call-history:tool result",
        ]
    );
}
