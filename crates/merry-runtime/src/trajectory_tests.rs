use super::*;
use crate::ArtifactContent;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, PendingToolCallBatch, QueuedInputLane, QueuedInputView,
    RuntimeJournalEvent, RuntimeJournalPayload, SessionId, ToolCallArguments, ToolCallBatchId,
    ToolInputSchema, ToolName, ToolOutput, ToolSpec, TrajectoryRecordDetails, TrajectoryRecordId,
    TrajectoryRecordKind, TrajectoryRecordStatus,
};
use merry_llm::{
    GenerationConfig, ModelContent, ModelInputItem, ModelMessage, ModelMessageRole, ModelName,
    ModelRequest,
};
use schemars::Schema;
use serde_json::json;

fn tool_call() -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new("call-1").expect("valid call id"),
        ToolName::new("read_file").expect("valid tool name"),
        ToolCallArguments::try_from(json!({"path":"README.md"})).expect("valid arguments"),
    )
}

#[test]
fn tool_result_updates_the_existing_tool_record() {
    let observability = RuntimeObservability::new(
        SessionId::new("trajectory-test").expect("valid session"),
        Vec::new(),
    );
    let call = tool_call();
    observability.observe_journal_event(&RuntimeJournalEvent::new(
        SessionId::new("trajectory-test").expect("valid session"),
        1,
        RuntimeJournalPayload::ToolCallPending { call: call.clone() },
    ));
    let artifact = ArtifactRef::new(
        ArtifactId::new("artifact-1").expect("valid artifact"),
        ArtifactKind::Text,
    );
    let result = ToolCallResult::succeeded(call.id().clone(), artifact);
    let output = ToolOutput::Text {
        text: "exact tool result".to_owned(),
    };
    observability.observe_journal_event_with_contents(
        &RuntimeJournalEvent::new(
            SessionId::new("trajectory-test").expect("valid session"),
            2,
            RuntimeJournalPayload::ToolCallResolved { result },
        ),
        None,
        Some(&output),
    );

    let snapshot = observability.snapshot();
    assert_eq!(snapshot.records().len(), 1);
    assert_eq!(
        snapshot.records()[0].status(),
        TrajectoryRecordStatus::Succeeded
    );
    assert_eq!(snapshot.latest_sequence(), 2);
    let TrajectoryRecordDetails::Tool { tool } = snapshot.records()[0].details() else {
        panic!("tool record should expose tool details");
    };
    assert_eq!(
        tool.output().map(TrajectoryPayload::content),
        Some("exact tool result")
    );
}

#[test]
fn tool_records_retain_schema_arguments_and_result_payload() {
    let session_id = SessionId::new("trajectory-test").expect("valid session");
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "required": ["path"]
    }))
    .expect("valid schema");
    let spec = ToolSpec::new(
        ToolName::new("read_file").expect("valid tool name"),
        "Read a UTF-8 file from the workspace.",
        ToolInputSchema::new(schema).expect("valid input schema"),
    )
    .expect("valid tool specification");
    let observability = RuntimeObservability::new(session_id.clone(), vec![spec]);
    let call = tool_call();
    let artifact = ArtifactRef::new(
        ArtifactId::new("artifact-1").expect("valid artifact"),
        ArtifactKind::Text,
    );
    let result = ToolCallResult::succeeded(call.id().clone(), artifact);
    observability.seed_from_transcript(&[
        SessionTranscriptItem::ToolCall { call: call.clone() },
        SessionTranscriptItem::ToolResult {
            call_id: call.id().clone(),
            result,
            output: Some(ToolOutput::Text {
                text: "file contents".to_owned(),
            }),
        },
    ]);

    let snapshot = observability.snapshot();
    let details = snapshot.records()[0].details();
    let merry_core::TrajectoryRecordDetails::Tool { tool } = details else {
        panic!("tool record should expose tool details");
    };
    assert_eq!(
        tool.tool_name().map(|name| name.as_str()),
        Some("read_file")
    );
    assert_eq!(tool.arguments().as_object()["path"], json!("README.md"));
    assert_eq!(tool.arguments_json(), r#"{"path":"README.md"}"#);
    assert_eq!(
        tool.output().map(TrajectoryPayload::content),
        Some("file contents")
    );
}

#[test]
fn model_request_projects_prompt_snapshot_without_repeating_records() {
    let observability = RuntimeObservability::new(
        SessionId::new("trajectory-test").expect("valid session"),
        Vec::new(),
    );
    let request = ModelRequest::new_with_input_and_stable_prefix(
        ModelName::new("test-model").expect("valid model"),
        vec![
            ModelInputItem::Message(
                ModelMessage::new(
                    ModelMessageRole::System,
                    ModelContent::text("stable runtime instructions").expect("valid content"),
                )
                .expect("valid system message"),
            ),
            ModelInputItem::Message(
                ModelMessage::new(
                    ModelMessageRole::System,
                    ModelContent::text(
                        "<merry_compiled_context>context evidence</merry_compiled_context>",
                    )
                    .expect("valid content"),
                )
                .expect("valid context message"),
            ),
            ModelInputItem::Message(
                ModelMessage::new(
                    ModelMessageRole::User,
                    ModelContent::text("inspect the repository").expect("valid content"),
                )
                .expect("valid user message"),
            ),
        ],
        Vec::new(),
        GenerationConfig::default(),
        1,
    )
    .expect("valid model request");

    observability.observe_model_request(&request, 8);

    let snapshot = observability.snapshot();
    assert!(snapshot.records().is_empty());
    assert_eq!(snapshot.prompt().stable_blocks().len(), 1);
    assert_eq!(
        snapshot.prompt().stable_blocks()[0].content(),
        "stable runtime instructions"
    );
    assert_eq!(snapshot.prompt().dynamic_context_count(), 1);
    assert_eq!(snapshot.prompt().latest_dynamic_sequence(), Some(8));
}

#[test]
fn accepted_input_waits_for_a_real_step_sequence() {
    let observability = RuntimeObservability::new(
        SessionId::new("trajectory-test").expect("valid session"),
        Vec::new(),
    );
    observability.record_queued_input_accepted(&[QueuedInputView {
        text: "inspect the repository".to_owned(),
        lane: QueuedInputLane::Next,
        position: 0,
    }]);
    observability.observe_journal_event(&RuntimeJournalEvent::new(
        SessionId::new("trajectory-test").expect("valid session"),
        7,
        RuntimeJournalPayload::StepStarted,
    ));

    let snapshot = observability.snapshot();
    assert_eq!(snapshot.records().len(), 1);
    assert_eq!(snapshot.records()[0].start_sequence(), 7);
    assert_eq!(
        snapshot.records()[0].summary(),
        Some("inspect the repository")
    );
    assert_eq!(
        snapshot.records()[0].turn_id().map(TrajectoryTurnId::value),
        Some(1)
    );
}

#[test]
fn repeated_transcript_messages_keep_distinct_record_ids() {
    let observability = RuntimeObservability::new(
        SessionId::new("trajectory-test").expect("valid session"),
        Vec::new(),
    );
    observability.seed_from_transcript(&[
        SessionTranscriptItem::UserMessage {
            text: "same message".to_owned(),
            images: Vec::new(),
        },
        SessionTranscriptItem::UserMessage {
            text: "same message".to_owned(),
            images: Vec::new(),
        },
    ]);

    let snapshot = observability.snapshot();
    assert_eq!(snapshot.records().len(), 2);
    assert_ne!(snapshot.records()[0].id(), snapshot.records()[1].id());
    assert_eq!(
        snapshot.records()[0].turn_id().map(TrajectoryTurnId::value),
        Some(1)
    );
    assert_eq!(
        snapshot.records()[1].turn_id().map(TrajectoryTurnId::value),
        Some(2)
    );
}

#[test]
fn trajectory_retains_complete_long_messages_prompts_and_tool_output() {
    let session_id = SessionId::new("trajectory-test").expect("valid session");
    let long_text = "x".repeat(16 * 1024 + 1);
    let long_output = format!("tool-output-{long_text}");
    let observability = RuntimeObservability::new(session_id, Vec::new());
    let call = tool_call();

    observability.seed_stable_prompt(std::slice::from_ref(&long_text));
    observability.seed_from_transcript(&[
        SessionTranscriptItem::UserMessage {
            text: long_text.clone(),
            images: Vec::new(),
        },
        SessionTranscriptItem::ToolCall { call: call.clone() },
        SessionTranscriptItem::ToolResult {
            call_id: call.id().clone(),
            result: ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    ArtifactId::new("artifact-1").expect("valid artifact id"),
                    ArtifactKind::Text,
                ),
            ),
            output: Some(ToolOutput::Text {
                text: long_output.clone(),
            }),
        },
    ]);

    let snapshot = observability.snapshot();
    assert_eq!(snapshot.prompt().stable_blocks()[0].content(), long_text);
    assert!(!snapshot.prompt().stable_blocks()[0].truncated());

    let merry_core::TrajectoryRecordDetails::Message { content, truncated } =
        snapshot.records()[0].details()
    else {
        panic!("user input should retain message details");
    };
    assert_eq!(content, &long_text);
    assert!(!truncated);

    let tool_record = snapshot
        .records()
        .iter()
        .find(|record| record.tool_call_id().is_some())
        .expect("tool record is retained");
    let merry_core::TrajectoryRecordDetails::Tool { tool } = tool_record.details() else {
        panic!("tool record should expose tool details");
    };
    let output = tool.output().expect("tool output is retained");
    assert_eq!(output.content(), long_output);
    assert!(!output.truncated());
}

#[test]
fn assistant_output_events_retain_exact_message_details() {
    let session_id = SessionId::new("trajectory-test").expect("valid session");
    let observability = RuntimeObservability::new(session_id.clone(), Vec::new());
    let artifact = ArtifactRef::new(
        ArtifactId::new("assistant-output-1").expect("valid artifact id"),
        ArtifactKind::Text,
    );
    let event = RuntimeJournalEvent::new(
        session_id,
        12,
        RuntimeJournalPayload::AssistantOutputRecorded {
            artifact: artifact.clone(),
        },
    );

    observability.observe_journal_event_with_assistant_text(&event, Some("assistant answer"));

    let snapshot = observability.snapshot();
    let record = snapshot
        .records()
        .first()
        .expect("assistant record is projected");
    assert_eq!(record.summary(), Some("assistant answer"));
    assert_eq!(record.artifacts(), &[artifact]);
    assert_eq!(
        record.details(),
        &TrajectoryRecordDetails::Message {
            content: "assistant answer".to_owned(),
            truncated: false,
        }
    );
}

#[test]
fn restoring_snapshot_hydrates_missing_assistant_message_details() {
    let session_id = SessionId::new("trajectory-test").expect("valid session");
    let artifact = ArtifactRef::new(
        ArtifactId::new("assistant-output-401").expect("valid artifact id"),
        ArtifactKind::Text,
    );
    let mut session = SessionState::new(session_id.clone());
    let assistant_text = "legacy assistant response";
    session
        .record_artifact_state(artifact.clone(), ArtifactContent::text(assistant_text))
        .expect("assistant artifact is recordable");

    let mut snapshot = TrajectorySnapshot::new(session_id.clone());
    let mut record = TrajectoryRecord::new(
        TrajectoryRecordId::new("assistant-record").expect("valid record id"),
        TrajectoryLane::Model,
        TrajectoryRecordKind::AssistantMessage,
        "Assistant message".to_owned(),
        TrajectoryRecordStatus::Succeeded,
        401,
    );
    record.add_artifact(artifact);
    snapshot.upsert_record(record);

    let observability = RuntimeObservability::new(session_id, Vec::new());
    observability.restore_snapshot(snapshot, &session);

    let snapshot = observability.snapshot();
    let record = snapshot
        .records()
        .first()
        .expect("restored assistant record");
    assert_eq!(record.summary(), Some(assistant_text));
    assert_eq!(
        record.details(),
        &TrajectoryRecordDetails::Message {
            content: assistant_text.to_owned(),
            truncated: false,
        }
    );
}

#[test]
fn restoring_snapshot_replays_transcript_tail_with_durable_sequences() {
    let session_id = SessionId::new("trajectory-replay-test").expect("valid session id");
    let mut session = SessionState::new(session_id.clone());
    let old_turn = session.begin_model_turn().expect("old turn begins");
    session
        .record_user_message_body(old_turn, "old input")
        .expect("old input records");
    let old_step = session.record_step_started();
    let old_assistant = session
        .record_assistant_text_output(old_turn, "old answer".to_owned())
        .expect("old assistant records");

    let observability = RuntimeObservability::new(session_id.clone(), Vec::new());
    observability.record_queued_input_accepted(&[QueuedInputView {
        text: "old input".to_owned(),
        lane: QueuedInputLane::Next,
        position: 0,
    }]);
    observability.observe_journal_event(&old_step);
    observability.observe_journal_event_with_assistant_text(&old_assistant, Some("old answer"));
    observability.close();
    let mut persisted_snapshot = observability.snapshot();
    for (index, mut record) in persisted_snapshot
        .records()
        .to_vec()
        .into_iter()
        .enumerate()
    {
        record.set_start_sequence(u64::try_from(index + 1).expect("test sequence fits"));
        persisted_snapshot.upsert_record(record);
    }

    let new_turn = session.begin_model_turn().expect("new turn begins");
    session
        .record_user_message_body(new_turn, "new input")
        .expect("new input records");
    let new_step = session.record_step_started();
    let _retry =
        session.record_model_retry_event(RuntimeJournalPayload::ModelRetryAttemptStarted {
            attempt: 1,
            max_attempts: 2,
        });
    let call = PendingToolCall::new(
        ToolCallId::new("replay-call").expect("valid call id"),
        ToolName::new("read_file").expect("valid tool name"),
        ToolCallArguments::try_from(json!({"path": "README.md"})).expect("valid arguments"),
    );
    let second_call = PendingToolCall::new(
        ToolCallId::new("replay-call-2").expect("valid call id"),
        ToolName::new("read_file").expect("valid tool name"),
        ToolCallArguments::try_from(json!({"path": "Cargo.toml"})).expect("valid arguments"),
    );
    let batch = PendingToolCallBatch::new(
        ToolCallBatchId::new("replay-batch").expect("valid batch id"),
        vec![call.clone(), second_call.clone()],
    )
    .expect("valid tool call batch");
    let pending = session
        .record_tool_call_batch_pending(new_turn, batch)
        .expect("new tool call records");
    session
        .close_model_response(new_turn, true)
        .expect("new tool response closes");
    let first_result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(
            ArtifactId::new("tool-result-replay-1").expect("valid artifact id"),
            ArtifactKind::Json,
        ),
    );
    let first_resolved = session
        .submit_tool_result(first_result, ArtifactContent::json(r#"{"ok":true}"#))
        .expect("tool result records");
    let second_result = ToolCallResult::succeeded(
        second_call.id().clone(),
        ArtifactRef::new(
            ArtifactId::new("tool-result-replay-2").expect("valid artifact id"),
            ArtifactKind::Json,
        ),
    );
    let second_resolved = session
        .submit_tool_result(second_result, ArtifactContent::json(r#"{"ok":false}"#))
        .expect("second tool result records");
    let continuation = session.begin_model_turn().expect("continuation begins");
    let assistant = session
        .record_assistant_text_output(continuation, "new answer".to_owned())
        .expect("new assistant records");

    let session_trajectory = session
        .trajectory_items()
        .expect("session trajectory is readable");
    let ledger = session.ledger_projection();
    let resumed = RuntimeObservability::new(session_id, Vec::new());
    resumed.restore_snapshot(persisted_snapshot, &session);
    resumed.reconcile_from_session(&session_trajectory, &ledger);

    let snapshot = resumed.snapshot();
    assert!(!snapshot.is_closed());
    assert_eq!(snapshot.latest_sequence(), assistant.sequence);
    assert_eq!(snapshot.records().len(), 7);
    let old_user = snapshot
        .records()
        .iter()
        .find(|record| record.summary() == Some("old input"))
        .expect("legacy user input is normalized");
    assert_eq!(old_user.start_sequence(), old_step.sequence);
    let old_assistant_record = snapshot
        .records()
        .iter()
        .find(|record| record.summary() == Some("old answer"))
        .expect("legacy assistant message is normalized");
    assert_eq!(
        old_assistant_record.start_sequence(),
        old_assistant.sequence
    );
    let new_user = snapshot
        .records()
        .iter()
        .find(|record| {
            record.kind() == TrajectoryRecordKind::UserInput
                && record.summary() == Some("new input")
        })
        .expect("replayed user input");
    assert_eq!(new_user.start_sequence(), new_step.sequence);
    assert!(snapshot.records().iter().any(|record| {
        record.kind() == TrajectoryRecordKind::Lifecycle && record.start_sequence() == 3
    }));
    let tool = snapshot
        .records()
        .iter()
        .find(|record| record.tool_call_id().is_some_and(|id| id == call.id()))
        .expect("replayed tool record");
    assert_eq!(tool.start_sequence(), pending.sequence);
    assert_eq!(
        tool.end_sequence(),
        first_resolved.last().map(|event| event.sequence)
    );
    let TrajectoryRecordDetails::Tool { tool } = tool.details() else {
        panic!("replayed tool should retain tool details");
    };
    assert_eq!(
        tool.output().map(TrajectoryPayload::content),
        Some(r#"{"ok":true}"#)
    );
    let second_tool = snapshot
        .records()
        .iter()
        .find(|record| {
            record
                .tool_call_id()
                .is_some_and(|id| id == second_call.id())
        })
        .expect("second replayed tool record");
    assert_eq!(second_tool.start_sequence(), pending.sequence);
    assert_eq!(
        second_tool.end_sequence(),
        second_resolved.last().map(|event| event.sequence)
    );
    assert!(snapshot.records().iter().any(|record| {
        record.kind() == TrajectoryRecordKind::AssistantMessage
            && record.summary() == Some("new answer")
    }));
}

#[test]
fn replay_uses_model_turn_sequence_after_step_fails_before_turn_begins() {
    let session_id = SessionId::new("trajectory-pre-turn-failure").expect("valid session id");
    let mut session = SessionState::new(session_id.clone());

    let first_step = session.record_step_started();
    let first_turn = session
        .begin_model_turn_at_sequence(first_step.sequence)
        .expect("first turn begins");
    session
        .record_user_message_body(first_turn, "first input")
        .expect("first input records");

    let failed_step = session.record_step_started();
    let second_step = session.record_step_started();
    let second_turn = session
        .begin_model_turn_at_sequence(second_step.sequence)
        .expect("second turn begins");
    session
        .record_user_message_body(second_turn, "second input")
        .expect("second input records");

    let trajectory = session
        .trajectory_items()
        .expect("trajectory projection is readable");
    let observability = RuntimeObservability::new(session_id, Vec::new());
    observability.reconcile_from_session(&trajectory, &session.ledger_projection());

    let snapshot = observability.snapshot();
    let second = snapshot
        .records()
        .iter()
        .find(|record| record.summary() == Some("second input"))
        .expect("second input is replayed");
    assert_eq!(second.start_sequence(), second_step.sequence);
    assert_ne!(second.start_sequence(), failed_step.sequence);
}

#[test]
fn closing_the_runtime_publishes_one_terminal_trajectory_event() {
    let observability = RuntimeObservability::new(
        SessionId::new("trajectory-test").expect("valid session"),
        Vec::new(),
    );
    let (_, mut receiver) = observability.subscribe_with_snapshot();
    observability.close();
    observability.close();

    assert!(matches!(
        receiver.try_recv().expect("closed trajectory event"),
        TrajectoryEvent::SessionClosed { .. }
    ));
    assert!(receiver.try_recv().is_err());
    assert!(observability.snapshot().is_closed());
}
