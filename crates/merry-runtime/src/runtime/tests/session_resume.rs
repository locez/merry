use super::*;
use crate::SessionTranscriptItem;
use crate::{
    ContextEvidence, ContextSummary, FINAL_OUTPUT_TOOL_NAME, FileSessionStore, ProjectRules,
    StepInput, TaskAnchor,
};
use merry_core::ToolCallResult;

#[tokio::test(flavor = "current_thread")]
async fn builder_resumes_session_from_store_and_reinjects_construction_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-resume");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");

    runtime.save_session().await.expect("session saves");

    let resumed = Runtime::builder(session_id.clone())
        .project_rules(ProjectRules::new("AGENTS.md", "Runtime rules").expect("project rules"))
        .task_anchor(TaskAnchor::new("continue from restored state").expect("task anchor"))
        .resume_from_store(store)
        .await
        .expect("runtime resumes");

    assert_eq!(resumed.session_id(), &session_id);
    assert!(resumed.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_runtime_reads_runtime_generated_checkpoint_ref_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let id = session_id("runtime-resume-checkpoint-ref");
    let mut session = SessionState::new(id.clone());
    session
        .record_test_user_message_body("exact persisted user source")
        .expect("covered user history records");
    session
        .record_test_user_message_body("retained user history")
        .expect("retained user history records");
    let input = session
        .build_citation_compaction_input(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
        )
        .expect("compaction input builds")
        .expect("covered user history is compressible");
    session
        .install_citation_compaction_candidate(
            input,
            r#"{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [],
              "corrected_misunderstandings": [],
              "durable_conclusions": [{
                "id": "c1",
                "text": "Keep the exact persisted user source.",
                "refs": ["h0"]
              }],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
            }"#,
        )
        .expect("checkpoint installs");
    session.save_to(&store).await.expect("session saves");

    let resumed = Runtime::builder(id)
        .resume_from_store(store)
        .await
        .expect("runtime resumes with checkpoint evidence");
    let page = resumed
        .read_checkpoint_ref_page(&CheckpointRefId::new("h0").expect("valid ref id"), 0, 4096)
        .await
        .expect("runtime-generated checkpoint ref reads after resume");

    assert_eq!(page.artifact_id().as_str(), "user-message-0");
    assert_eq!(page.content(), "exact persisted user source");
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_resume_uses_default_store_constructor_shape() {
    let session_id = session_id("runtime-resume-default-shape");
    let _resume_fn = Runtime::resume;
    let _builder = Runtime::builder(session_id);
}

#[tokio::test(flavor = "current_thread")]
async fn save_session_rejects_while_step_is_active() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-save-active");
    let (started_tx, started_rx) = oneshot::channel();
    let (dropped_tx, dropped_rx) = oneshot::channel();
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::PendingSetupWithDrop {
            started: started_tx,
            dropped: dropped_tx,
        },
    ]);
    let runtime = Runtime::builder(session_id)
        .session_store(store)
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds");
    let stream = runtime
        .step(
            StepInput::user_text("hold").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts");
    started_rx.await.expect("provider setup starts");

    let error = runtime
        .save_session()
        .await
        .expect_err("active step save is rejected");
    assert!(matches!(error, RuntimeError::StepAlreadyActive { .. }));

    drop(stream);
    dropped_rx.await.expect("provider setup future drops");
}

#[tokio::test(flavor = "current_thread")]
async fn complete_boundary_savepoint_text_output_step_completed_persists_resume_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-savepoint-text");
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(vec![ModelOutput::text("persist me")], FinishReason::Stop),
        )])]);
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds");

    let events = runtime
        .step(
            StepInput::user_text("write text").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts")
        .collect::<Vec<_>>()
        .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    let resumed = Runtime::builder(session_id.clone())
        .resume_from_store(store)
        .await
        .expect("runtime resumes after text step");
    let artifact_id = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { artifact, .. } => Some(artifact.id()),
            _ => None,
        })
        .expect("assistant output artifact is emitted");
    assert_eq!(
        resumed
            .read_artifact_content(artifact_id)
            .await
            .expect("assistant output artifact resumes")
            .as_text(),
        Some("persist me")
    );
    assert_eq!(
        resumed
            .session_transcript()
            .await
            .expect("transcript resumes"),
        vec![
            SessionTranscriptItem::UserMessage {
                text: "write text".to_owned()
            },
            SessionTranscriptItem::AssistantText {
                text: "persist me".to_owned()
            }
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_text_stream_while_savepoint_is_blocked_keeps_terminal_batch_committed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text("atomic terminal response")],
                FinishReason::Stop,
            ),
        )])]);
    let runtime = Runtime::builder(session_id("runtime-savepoint-text-drop"))
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero event buffer"))
        .session_store(store)
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds");
    let mut stream = runtime
        .step(
            StepInput::user_text("write terminal text").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts");

    assert_eq!(
        stream.next().await.expect("session start event").sequence,
        0
    );
    assert_eq!(stream.next().await.expect("step start event").sequence, 1);
    let session = loop {
        let session = runtime.inner.session.lock().await;
        let assistant_recorded = session
            .full_transcript_snapshot()
            .expect("transcript remains readable")
            .iter()
            .any(|item| {
                matches!(
                    item,
                    crate::session::TranscriptItemSnapshot::AssistantText { text }
                        if text == "atomic terminal response"
                )
            });
        if assistant_recorded {
            break session;
        }
        drop(session);
        tokio::task::yield_now().await;
    };

    let assistant = stream.next().await.expect("assistant output event");
    assert_eq!(assistant.sequence, 2);
    assert!(matches!(
        assistant.payload,
        RuntimeJournalPayload::AssistantOutputRecorded { .. }
    ));
    assert_eq!(
        session.next_sequence(),
        4,
        "StepCompleted must commit before the terminal batch becomes observable"
    );
    assert!(session.ledger_projection().entries().iter().any(|entry| {
        matches!(
            entry,
            LedgerProjection::Lifecycle {
                sequence: 3,
                kind: LedgerFactKind::StepCompleted,
                ..
            }
        )
    }));

    drop(stream);
    assert_eq!(session.next_sequence(), 4);
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Completed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn load_session_from_store_does_not_enable_automatic_savepoints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-load-without-savepoint");
    let runtime = Runtime::builder(session_id.clone())
        .build()
        .expect("runtime builds");
    runtime
        .save_session_to(store.clone())
        .await
        .expect("initial session saves");
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::text("not auto saved")],
                FinishReason::Stop,
            ),
        )])]);
    let runtime = Runtime::builder(session_id.clone())
        .model_provider(Arc::new(provider), model_name())
        .load_session_from_store(store.clone())
        .await
        .expect("session loads")
        .build()
        .expect("runtime builds from loaded session");

    let events = runtime
        .step(
            StepInput::user_text("write text").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts")
        .collect::<Vec<_>>()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    let artifact_id = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { artifact, .. } => {
                Some(artifact.id().clone())
            }
            _ => None,
        })
        .expect("assistant output artifact is emitted");

    let resumed = Runtime::builder(session_id)
        .resume_from_store(store)
        .await
        .expect("runtime resumes original saved state");

    assert!(
        resumed.read_artifact_content(&artifact_id).await.is_err(),
        "loaded sessions should not write automatic savepoints"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn complete_boundary_savepoint_tool_call_pending_does_not_overwrite_last_complete_savepoint()
{
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-savepoint-pending");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");
    runtime.save_session().await.expect("initial savepoint");

    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![ModelOutput::tool_call(model_tool_call(
                    "pending-call",
                    "lookup",
                    json!({"query":"pending"}),
                ))],
                FinishReason::ToolCalls,
            ),
        )])]);
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds with provider");

    let events = runtime
        .step(
            StepInput::user_text("call tool").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts")
        .collect::<Vec<_>>()
        .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::ToolCallPending { .. }))
    );
    let resumed = Runtime::builder(session_id.clone())
        .resume_from_store(store)
        .await
        .expect("runtime resumes from previous complete savepoint");
    assert!(resumed.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn complete_boundary_savepoint_submit_tool_result_persists_complete_exchange_before_returning_events()
 {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-savepoint-tool-result");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");
    let pending = pending_tool_call("manual-call");
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending call records");
    }
    let artifact = ArtifactRef::new(
        ArtifactId::new("manual-result-artifact").expect("valid artifact id"),
        ArtifactKind::Json,
    );
    let result = ToolCallResult::succeeded(pending.id().clone(), artifact);

    let events = runtime
        .submit_tool_result(result, ArtifactContent::json(r#"{"ok":true}"#))
        .await
        .expect("tool result submits");

    assert!(events.iter().any(|event| matches!(
        event.payload,
        RuntimeJournalPayload::ToolCallResolved { .. }
    )));
    let resumed = Runtime::builder(session_id.clone())
        .resume_from_store(store)
        .await
        .expect("runtime resumes after tool result");
    assert!(resumed.pending_tool_calls().await.is_empty());
    assert_eq!(
        resumed
            .read_artifact_content(&ArtifactId::new("manual-result-artifact").expect("valid id"))
            .await
            .expect("manual result artifact resumes")
            .as_text(),
        Some(r#"{"ok":true}"#)
    );
    assert_eq!(
        resumed
            .session_transcript()
            .await
            .expect("transcript resumes"),
        vec![
            SessionTranscriptItem::ToolCall { call: pending },
            SessionTranscriptItem::ToolResult {
                call_id: ToolCallId::new("manual-call").expect("valid call id"),
                result: ToolCallResult::succeeded(
                    ToolCallId::new("manual-call").expect("valid call id"),
                    ArtifactRef::new(
                        ArtifactId::new("manual-result-artifact").expect("valid artifact id"),
                        ArtifactKind::Json,
                    ),
                ),
                output: Some(merry_core::ToolOutput::Json {
                    json: r#"{"ok":true}"#.to_owned()
                }),
            }
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn automatic_savepoint_failure_does_not_fail_submitted_tool_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    let broken_store_root = temp.path().join("sessions-file");
    std::fs::write(&broken_store_root, b"not a directory").expect("broken store marker writes");
    let store = FileSessionStore::new(&broken_store_root);
    let session_id = session_id("runtime-savepoint-tool-result-store-fails");
    let runtime = Runtime::builder(session_id)
        .session_store(store)
        .build()
        .expect("runtime builds");
    let pending = pending_tool_call("manual-call-save-fails");
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending call records");
    }
    let artifact = ArtifactRef::new(
        ArtifactId::new("manual-result-store-fails").expect("valid artifact id"),
        ArtifactKind::Json,
    );
    let result = ToolCallResult::succeeded(pending.id().clone(), artifact.clone());

    let events = runtime
        .submit_tool_result(result, ArtifactContent::json(r#"{"ok":true}"#))
        .await
        .expect("tool result succeeds even when automatic savepoint fails");

    assert!(events.iter().any(|event| matches!(
        event.payload,
        RuntimeJournalPayload::ToolCallResolved { .. }
    )));
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(
        runtime
            .read_artifact_content(artifact.id())
            .await
            .expect("committed result artifact remains readable")
            .as_text(),
        Some(r#"{"ok":true}"#)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn automatic_savepoint_failure_does_not_fail_text_step_completion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let broken_store_root = temp.path().join("sessions-file");
    std::fs::write(&broken_store_root, b"not a directory").expect("broken store marker writes");
    let store = FileSessionStore::new(&broken_store_root);
    let session_id = session_id("runtime-savepoint-text-store-fails");
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(vec![ModelOutput::text("persist maybe")], FinishReason::Stop),
        )])]);
    let runtime = Runtime::builder(session_id)
        .session_store(store)
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds");

    let events = runtime
        .step(
            StepInput::user_text("write text").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts")
        .collect::<Vec<_>>()
        .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::Failed { .. }))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn record_context_summary_rejects_missing_evidence_before_savepoint() {
    let runtime = Runtime::builder(session_id("runtime-context-missing-evidence"))
        .build()
        .expect("runtime builds");
    let missing_ref = EvidenceRef::new(
        ArtifactId::new("missing-context-evidence").expect("valid artifact id"),
        EvidenceLocator::whole_artifact(),
    );
    let summary = ContextSummary::new(
        "missing-evidence-summary",
        "This summary points at missing exact evidence.",
        vec![
            ContextEvidence::new("missing exact evidence", missing_ref)
                .expect("context evidence builds"),
        ],
    )
    .expect("context summary builds");

    let error = runtime
        .record_context_summary(summary)
        .await
        .expect_err("missing evidence is rejected at record time");

    assert!(error.to_string().contains("unreadable evidence"));
}

#[tokio::test(flavor = "current_thread")]
async fn complete_boundary_savepoint_final_output_tool_call_persists_complete_boundary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-savepoint-final-output");
    let pending = PendingToolCall::new(
        ToolCallId::new("final-output-call").expect("valid call id"),
        ToolName::new(FINAL_OUTPUT_TOOL_NAME).expect("valid tool name"),
        ToolCallArguments::try_from(json!({
            "answer": "done"
        }))
        .expect("valid tool call arguments"),
    );
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending final output records");
    }

    let (_output, events) = runtime
        .record_final_output_tool_call(pending)
        .await
        .expect("final output records");

    assert!(events.iter().any(|event| matches!(
        event.payload,
        RuntimeJournalPayload::FinalOutputRecorded { .. }
    )));
    let resumed = Runtime::builder(session_id)
        .resume_from_store(store)
        .await
        .expect("runtime resumes after final output");
    assert!(resumed.pending_tool_calls().await.is_empty());
}

fn model_tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::try_from(arguments).expect("valid tool arguments"),
    )
}
