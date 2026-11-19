use crate::{SessionTranscriptItem, session::SessionState, trajectory::RuntimeObservability};
use merry_core::{
    PendingToolCall, PendingToolCallBatch, QueuedInputLane, QueuedInputView, RuntimeJournalEvent,
    RuntimeJournalPayload, SessionId, ToolCallArguments, ToolCallBatchId, ToolCallId,
    ToolCallResult, ToolInputSchema, ToolName, ToolOutput, ToolSpec, TrajectoryEvent,
    TrajectoryPayload, TrajectoryRecordDetails, TrajectoryRecordKind, TrajectoryRecordStatus,
    TrajectoryTurnId,
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
    let result = ToolCallResult::succeeded(call.id().clone());
    let output = ToolOutput::text("exact tool result").bounded_preview(usize::MAX);
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
    let result = ToolCallResult::succeeded(call.id().clone());
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
            result: ToolCallResult::succeeded(call.id().clone()),
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
    let output = tool.output().expect("tool output preview is retained");
    assert!(output.truncated());
    assert!(long_output.starts_with(output.content()));
    assert!(output.content().len() <= crate::trajectory::TRAJECTORY_TOOL_PREVIEW_BYTES);
}

#[test]
fn assistant_output_events_retain_exact_message_details() {
    let session_id = SessionId::new("trajectory-test").expect("valid session");
    let observability = RuntimeObservability::new(session_id.clone(), Vec::new());
    let event = RuntimeJournalEvent::new(
        session_id,
        12,
        RuntimeJournalPayload::AssistantOutputRecorded {
            text: "assistant answer".to_owned(),
        },
    );

    observability.observe_journal_event_with_assistant_text(&event, Some("assistant answer"));

    let snapshot = observability.snapshot();
    let record = snapshot
        .records()
        .first()
        .expect("assistant record is projected");
    assert_eq!(record.summary(), Some("assistant answer"));
    assert_eq!(
        record.details(),
        &TrajectoryRecordDetails::Message {
            content: "assistant answer".to_owned(),
            truncated: false,
        }
    );
}

#[test]
fn compaction_failure_closes_the_active_compaction_record_with_diagnostic() {
    let session_id = SessionId::new("trajectory-compaction-failure").expect("valid session");
    let observability = RuntimeObservability::new(session_id.clone(), Vec::new());
    observability.observe_journal_event(&RuntimeJournalEvent::new(
        session_id.clone(),
        10,
        RuntimeJournalPayload::CompactionStarted,
    ));
    observability.observe_journal_event(&RuntimeJournalEvent::new(
        session_id,
        11,
        RuntimeJournalPayload::Failed {
            diagnostic: merry_core::ErrorInfo::new(
                "auto_compaction",
                "OpenAI Responses request returned HTTP 400 (code: invalid_json_schema)",
            )
            .expect("valid diagnostic"),
        },
    ));

    let snapshot = observability.snapshot();
    let compaction = snapshot
        .records()
        .iter()
        .find(|record| record.kind() == TrajectoryRecordKind::Compaction)
        .expect("compaction record is retained");
    assert_eq!(compaction.status(), TrajectoryRecordStatus::Failed);
    assert_eq!(compaction.end_sequence(), Some(11));
    assert_eq!(
        compaction.diagnostic().map(merry_core::ErrorInfo::message),
        Some("OpenAI Responses request returned HTTP 400 (code: invalid_json_schema)")
    );
    assert!(snapshot.records().iter().any(|record| {
        record.label() == "Run failed" && record.status() == TrajectoryRecordStatus::Failed
    }));
}

#[test]
fn compaction_completion_closes_the_active_compaction_record() {
    let session_id = SessionId::new("trajectory-compaction-completion").expect("valid session");
    let observability = RuntimeObservability::new(session_id.clone(), Vec::new());
    observability.observe_journal_event(&RuntimeJournalEvent::new(
        session_id.clone(),
        20,
        RuntimeJournalPayload::CompactionStarted,
    ));
    observability.observe_journal_event(&RuntimeJournalEvent::new(
        session_id,
        21,
        RuntimeJournalPayload::CompactionCompleted {
            checkpoint_id: "checkpoint-21".to_owned(),
            covered_history_item_count: 6,
        },
    ));

    let snapshot = observability.snapshot();
    let compactions = snapshot
        .records()
        .iter()
        .filter(|record| record.kind() == TrajectoryRecordKind::Compaction)
        .collect::<Vec<_>>();
    assert_eq!(compactions.len(), 1);
    assert_eq!(compactions[0].status(), TrajectoryRecordStatus::Completed);
    assert_eq!(compactions[0].end_sequence(), Some(21));
    assert_eq!(
        compactions[0].summary(),
        Some("Context checkpoint installed: checkpoint-21 (6 history items)")
    );
}

#[test]
fn seeding_from_session_uses_model_view_preview_instead_of_raw_full_json() {
    let session_id = SessionId::new("trajectory-seed-model-view").expect("valid session");
    let mut session = SessionState::new(session_id.clone());
    let turn = session.begin_model_turn().expect("turn begins");
    let call = tool_call();
    let batch = PendingToolCallBatch::new(
        ToolCallBatchId::new("seed-batch").expect("valid batch id"),
        vec![call.clone()],
    )
    .expect("valid batch");
    session
        .record_tool_call_batch_pending(turn, batch)
        .expect("tool call records");
    session
        .close_model_response(turn, true)
        .expect("model response closes");
    session
        .submit_tool_result(
            ToolCallResult::succeeded(call.id().clone()),
            crate::tool::ToolResultContent::from(ToolOutput::json(
                r#"{"kind":"process_action","stdout":{"text":"raw json body"}}"#,
            ))
            .with_model(ToolOutput::text("exit 0\nstdout:\ncompact model view")),
        )
        .expect("tool result records");

    let observability = RuntimeObservability::new(session_id, Vec::new());
    observability.seed_from_session(&session);

    let snapshot = observability.snapshot();
    let record = snapshot.records().first().expect("seeded tool record");
    let TrajectoryRecordDetails::Tool { tool } = record.details() else {
        panic!("seeded record should have tool details");
    };
    assert_eq!(
        tool.output().map(TrajectoryPayload::content),
        Some("exit 0\nstdout:\ncompact model view")
    );
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
