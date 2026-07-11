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
fn assistant_output_artifact_id_uses_artifact_event_sequence() {
    let mut session = SessionState::new(session_id());
    let _started = session
        .record_session_started_if_needed()
        .expect("start should emit");
    let _step_started = session.record_step_started();

    let artifact = session
        .record_test_assistant_text_output("hello".to_owned())
        .expect("assistant output should record");
    let completed = session.record_step_completed();

    match artifact.payload {
        RuntimeJournalPayload::AssistantOutputRecorded { artifact } => {
            assert_eq!(artifact.id().as_str(), "assistant-output-2");
            assert_eq!(artifact.kind(), &ArtifactKind::Text);
        }
        other => panic!("expected assistant output event, got {other:?}"),
    }
    assert_eq!(artifact.sequence, 2);
    assert_eq!(completed.sequence, 3);
}

#[test]
fn generic_artifact_recording_still_emits_artifact_recorded() {
    let mut session = SessionState::new(session_id());
    let artifact = ArtifactRef::new(artifact_id("generic-artifact"), ArtifactKind::Text);

    let events = session
        .record_artifact_events(artifact.clone(), ArtifactContent::text("generic content"))
        .expect("generic artifact should record");

    assert!(matches!(
        events.last().map(|event| &event.payload),
        Some(RuntimeJournalPayload::ArtifactRecorded { artifact: recorded }) if recorded == &artifact
    ));
}

#[test]
fn assistant_output_content_is_stored_before_event_is_observable() {
    let mut session = SessionState::new(session_id());

    let event = session
        .record_test_assistant_text_output("stored assistant".to_owned())
        .expect("assistant output should record");

    let RuntimeJournalPayload::AssistantOutputRecorded { artifact } = &event.payload else {
        panic!("expected assistant output event, got {:?}", event.payload);
    };
    assert_eq!(
        session
            .read_artifact_content(artifact.id())
            .expect("assistant content should be readable before event leaves session"),
        ArtifactContent::text("stored assistant")
    );
}

#[test]
fn assistant_transcript_refers_to_recorded_output_artifact() {
    let mut session = SessionState::new(session_id());

    let event = session
        .record_test_assistant_text_output("transcript assistant".to_owned())
        .expect("assistant output should record");

    let RuntimeJournalPayload::AssistantOutputRecorded { artifact } = &event.payload else {
        panic!("expected assistant output event, got {:?}", event.payload);
    };
    assert_eq!(
        session.assistant_transcript_artifact_ids_for_tests(),
        vec![artifact.id().clone()]
    );
}

#[test]
fn submit_tool_result_stores_exact_content_before_resolved_event() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("call-1");
    session
        .record_test_tool_call_pending(call.clone())
        .expect("pending call should record");
    let artifact = ArtifactRef::new(artifact_id("tool-result-exact"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), artifact.clone());

    let events = session
        .submit_tool_result(result.clone(), ArtifactContent::text("exact result\n"))
        .expect("tool result should submit");

    assert_eq!(
        session
            .read_artifact_content(artifact.id())
            .expect("recorded content should be readable"),
        ArtifactContent::text("exact result\n")
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
    let artifact = ArtifactRef::new(artifact_id("tool-result-history"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), artifact);
    session
        .submit_tool_result(result, ArtifactContent::text("tool result"))
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
