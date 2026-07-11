use super::*;

#[tokio::test(flavor = "current_thread")]
async fn provider_failure_finishes_and_eof_abort_in_progress_turns() {
    for (case, finish_reason, expected_code) in [
        ("length", Some(FinishReason::Length), "model_length"),
        ("blocked", Some(FinishReason::Blocked), "model_blocked"),
        ("eof", None, "model_stream_eof"),
    ] {
        let events = finish_reason.map_or_else(Vec::new, |finish_reason| {
            vec![Ok(completed_event_with(Vec::new(), finish_reason))]
        });
        let provider =
            RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(
                events,
            )]);
        let runtime = Runtime::builder(session_id(&format!("runtime-provider-{case}-abort")))
            .model_provider(Arc::new(provider), model_name())
            .build()
            .expect("runtime should build");

        let events = collect_step(
            &runtime,
            "End this provider turn.",
            crate::StepContext::default(),
        )
        .await;

        assert_eq!(failed_code(&events), Some(expected_code), "case {case}");
        assert_eq!(
            runtime
                .inner
                .session
                .lock()
                .await
                .model_turn_status(ModelTurnId::new(1)),
            Some(ModelTurnStatus::Aborted),
            "case {case}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn one_slot_commentary_observes_atomically_committed_tool_response() {
    let call = model_tool_call("drop-after-commentary");
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![
                    ModelOutput::text("Commentary delivered before drop."),
                    ModelOutput::tool_call(call),
                ],
                FinishReason::ToolCalls,
            ),
        )])]);
    let runtime = Runtime::builder(session_id("runtime-drop-after-commentary"))
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero event buffer"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let mut stream = runtime
        .step(
            crate::StepInput::user_text("Request a tool call.").expect("valid input"),
            crate::StepContext::default(),
        )
        .expect("step should start");

    let session_started = stream.next().await.expect("session start event");
    let step_started = stream.next().await.expect("step start event");
    assert_eq!(session_started.sequence, 0);
    assert_eq!(step_started.sequence, 1);

    let session = loop {
        let session = runtime.inner.session.lock().await;
        let commentary_recorded = session
            .full_transcript_snapshot()
            .expect("transcript should be readable")
            .iter()
            .any(|item| {
                matches!(
                    item,
                    crate::session::TranscriptItemSnapshot::AssistantText { text }
                        if text == "Commentary delivered before drop."
                )
            });
        if commentary_recorded {
            break session;
        }
        drop(session);
        tokio::task::yield_now().await;
    };

    let commentary = stream.next().await.expect("commentary event");
    assert_eq!(commentary.sequence, 2);
    assert!(matches!(
        commentary.payload,
        RuntimeJournalPayload::AssistantOutputRecorded { .. }
    ));

    assert_eq!(session.pending_tool_calls().len(), 1);
    assert_eq!(session.next_sequence(), 4);
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::AwaitingToolResults)
    );
    assert_eq!(
        session
            .full_transcript_snapshot()
            .expect("transcript should remain readable")
            .len(),
        3,
        "user input, commentary, and tool call must commit before commentary is observable"
    );
    assert!(session.ledger_projection().entries().iter().any(|entry| {
        matches!(
            entry,
            LedgerProjection::Lifecycle {
                sequence: 2,
                kind: LedgerFactKind::ArtifactRecorded,
                ..
            }
        )
    }));
    assert!(session.ledger_projection().entries().iter().any(|entry| {
        matches!(
            entry,
            LedgerProjection::Lifecycle {
                sequence: 3,
                kind: LedgerFactKind::ToolCallPending,
                ..
            }
        )
    }));

    drop(stream);
    drop(session);
    tokio::task::yield_now().await;
    let session = runtime.inner.session.lock().await;
    assert_eq!(session.pending_tool_calls().len(), 1);
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::AwaitingToolResults)
    );
    assert_eq!(session.next_sequence(), 4);
}
