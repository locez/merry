use super::*;

#[tokio::test(flavor = "current_thread")]
async fn provider_stream_context_uses_runtime_session_as_prompt_cache_key() {
    let provider = RecordingModelProvider::new();
    let runtime = Runtime::builder(session_id("runtime-cache-key"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Use the runtime session as the cache key.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    let contexts = provider.recorded_contexts();
    assert_eq!(contexts.len(), 1);
    assert_eq!(
        contexts[0]
            .prompt_cache_key()
            .expect("prompt cache key should be set")
            .as_str(),
        "runtime-cache-key"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn user_text_burst_records_every_item_in_one_model_turn() {
    let runtime = Runtime::builder(session_id("runtime-user-burst-one-turn"))
        .model_provider(Arc::new(RecordingModelProvider::new()), model_name())
        .build()
        .expect("runtime should build");
    let stream = runtime
        .step(
            crate::StepInput::user_texts(["first exact user item", "second exact user item"])
                .expect("valid user burst"),
            crate::StepContext::default(),
        )
        .expect("step should start");
    let events = stream.collect::<Vec<_>>().await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .transcript_model_turn_ids_for_tests(),
        [
            ModelTurnId::new(1),
            ModelTurnId::new(1),
            ModelTurnId::new(1),
        ],
        "both user source items and the response belong to one durable model turn"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_retry_events_are_emitted_for_failure_before_output() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Err(ModelError::provider(
                ProviderErrorKind::Unavailable,
                "stream interrupted",
            )),
        ]),
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "successful attempt".to_owned(),
            }),
            Ok(completed_event_with(
                vec![ModelOutput::text("successful attempt")],
                FinishReason::Stop,
            )),
        ]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-model-retry-events"))
        .model_retry_policy(
            ModelRetryPolicy::new(
                true,
                3,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(100),
                false,
            )
            .expect("valid retry policy"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Retry provider stream.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(provider.recorded_requests().len(), 2);
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ModelRetryAttemptStarted",
            "ModelRetryScheduled",
            "ModelRetryAttemptStarted",
            "AssistantOutputDelta",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    let artifact_id = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::AssistantOutputRecorded { artifact } => {
                Some(artifact.id().clone())
            }
            _ => None,
        })
        .expect("assistant output artifact should be recorded");
    let content = runtime
        .read_artifact_content(&artifact_id)
        .await
        .expect("artifact should be readable");
    assert_eq!(content.as_text(), Some("successful attempt"));
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(session.model_turn_status(ModelTurnId::new(2)), None);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_setup_retry_reuses_one_model_turn() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::SetupError(ModelError::provider(
            ProviderErrorKind::Unavailable,
            "retry setup",
        )),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text("setup retry succeeded")],
            FinishReason::Stop,
        ))]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-setup-retry-turn"))
        .model_retry_policy(
            ModelRetryPolicy::new(
                true,
                2,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(100),
                false,
            )
            .expect("valid retry policy"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Retry setup with one turn.",
        crate::StepContext::default(),
    )
    .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted))
    );
    assert_eq!(provider.recorded_requests().len(), 2);
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(session.model_turn_status(ModelTurnId::new(2)), None);
}

#[tokio::test(flavor = "current_thread")]
async fn model_stream_failure_after_output_is_not_retried_or_recorded_as_complete() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "visible partial output".to_owned(),
            }),
            Err(ModelError::provider(
                ProviderErrorKind::Unavailable,
                "stream interrupted",
            )),
        ]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text("must not be replayed")],
            FinishReason::Stop,
        ))]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-model-no-retry-after-output"))
        .model_retry_policy(
            ModelRetryPolicy::new(
                true,
                3,
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(100),
                false,
            )
            .expect("valid retry policy"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(
        &runtime,
        "Do not replay visible output.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ModelRetryAttemptStarted",
            "AssistantOutputDelta",
            "Failed",
        ]
    );
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        RuntimeJournalPayload::AssistantOutputDelta { delta }
            if delta == "visible partial output"
    )));
    assert!(!events.iter().any(|event| matches!(
        event.payload,
        RuntimeJournalPayload::AssistantOutputRecorded { .. }
            | RuntimeJournalPayload::StepCompleted
    )));
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Aborted)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_after_activation_before_provider_request_clears_projection() {
    let memory = activated_memory(
        "memory-cancelled-after-activation",
        "Activated memory must not survive cancellation before provider setup.",
        "memory-cancelled-after-activation-artifact",
    );
    let token = CancellationToken::new();
    let source = ScriptedMemoryActivationSource::with_script(vec![
        ScriptedMemoryActivationResponse::CancelThenMemories {
            token: token.clone(),
            memories: vec![memory],
        },
    ]);
    let provider = RecordingModelProvider::new();
    let runtime = runtime_with_provider_and_memory_source(
        "runtime-memory-activation-cancel-clears",
        provider.clone(),
        source.clone(),
    );
    record_memory_artifact(
        &runtime,
        "memory-cancelled-after-activation-artifact",
        "exact evidence for activation cancellation",
    );

    let events = collect_step(&runtime, "Topic request.", crate::StepContext::new(token)).await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 0);
    assert_eq!(
        crate::ContextCompiler::new()
            .compile(&runtime.context_snapshot().await)
            .expect("context compiles after cancellation cleanup")
            .to_snapshot(),
        ""
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_step_during_provider_setup_clears_activated_memory_projection() {
    let (provider_started_tx, provider_started_rx) = oneshot::channel();
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::PendingSetup(
            provider_started_tx,
        )]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-setup-drop-clears",
        provider.clone(),
        "memory-provider-setup-drop",
        "Activated memory must not survive dropped setup before stream commit.",
        "memory-provider-setup-drop-artifact",
    );

    let stream = runtime
        .step(
            crate::StepInput::user_text("Topic request.").expect("valid step input"),
            crate::StepContext::new(CancellationToken::new()),
        )
        .expect("step should start");
    provider_started_rx
        .await
        .expect("provider setup future should start");

    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-setup-drop",
        "Activated memory must not survive dropped setup before stream commit.",
    )
    .await;

    drop(stream);
    tokio::task::yield_now().await;

    assert_activated_memory_projection_cleared(&runtime).await;
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Aborted),
        "dropping the producer must not leave its allocated turn in progress"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_step_during_provider_setup_with_held_session_lock_defers_projection_cleanup() {
    let (provider_started_tx, provider_started_rx) = oneshot::channel();
    let (provider_dropped_tx, provider_dropped_rx) = oneshot::channel();
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::PendingSetupWithDrop {
            started: provider_started_tx,
            dropped: provider_dropped_tx,
        },
    ]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-setup-drop-spawned-cleanup",
        provider.clone(),
        "memory-provider-setup-drop-spawned",
        "Activated memory is cleared by spawned cleanup when drop cannot lock session.",
        "memory-provider-setup-drop-spawned-artifact",
    );

    let stream = runtime
        .step(
            crate::StepInput::user_text("Topic request.").expect("valid step input"),
            crate::StepContext::new(CancellationToken::new()),
        )
        .expect("step should start");
    provider_started_rx
        .await
        .expect("provider setup future should start");

    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-setup-drop-spawned",
        "Activated memory is cleared by spawned cleanup when drop cannot lock session.",
    )
    .await;

    let session = runtime.inner.session.lock().await;
    drop(stream);
    provider_dropped_rx
        .await
        .expect("provider setup future should be aborted");
    tokio::task::yield_now().await;

    let snapshot = crate::ContextCompiler::new()
        .compile(&session.context_snapshot())
        .expect("context compiles while cleanup waits for session lock")
        .to_snapshot();
    assert!(
        snapshot.contains("memory:memory-provider-setup-drop-spawned"),
        "projection should remain while spawned cleanup is waiting for session lock; snapshot:\n{snapshot}"
    );
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::InProgress),
        "turn cleanup must wait without blocking while the session lock is held"
    );

    drop(session);
    for _ in 0..32 {
        if runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1))
            == Some(ModelTurnStatus::Aborted)
        {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert_activated_memory_projection_cleared(&runtime).await;
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Aborted)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_setup_error_before_stream_clears_activated_memory_projection() {
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::SetupError(
            ModelError::provider(ProviderErrorKind::Unavailable, "provider setup failed"),
        )]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-setup-error-clears",
        provider.clone(),
        "memory-provider-setup-error",
        "Activated memory must not survive provider setup failure.",
        "memory-provider-setup-error-artifact",
    );

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_unavailable"));
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Aborted)
    );
    assert_activated_memory_projection_cleared(&runtime).await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stream_error_after_stream_start_retains_activated_memory_projection() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Err(ModelError::provider(
            ProviderErrorKind::Unavailable,
            "provider stream failed",
        ))]),
    ]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-stream-error-retains",
        provider.clone(),
        "memory-provider-stream-error",
        "Activated memory must survive provider stream failure after setup.",
        "memory-provider-stream-error-artifact",
    );

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_unavailable"));
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-stream-error",
        "Activated memory must survive provider stream failure after setup.",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stream_cancelled_error_retains_activated_memory_projection() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Err(ModelError::Cancelled)]),
    ]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-stream-cancelled-error-retains",
        provider.clone(),
        "memory-provider-stream-cancelled-error",
        "Activated memory must survive stream cancellation after setup.",
        "memory-provider-stream-cancelled-error-artifact",
    );

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Aborted)
    );
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-stream-cancelled-error",
        "Activated memory must survive stream cancellation after setup.",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_cancelled_finish_retains_activated_memory_projection() {
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(Vec::new(), FinishReason::Cancelled),
        )])]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-cancelled-finish-retains",
        provider.clone(),
        "memory-provider-cancelled-finish",
        "Activated memory must survive cancelled finish after setup.",
        "memory-provider-cancelled-finish-artifact",
    );

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-cancelled-finish",
        "Activated memory must survive cancelled finish after setup.",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_error_finish_retains_activated_memory_projection() {
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(Vec::new(), FinishReason::Error),
        )])]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-finish-error-retains",
        provider.clone(),
        "memory-provider-finish-error",
        "Activated memory must survive provider error finish after setup.",
        "memory-provider-finish-error-artifact",
    );

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_finish_error"));
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-finish-error",
        "Activated memory must survive provider error finish after setup.",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_tool_call_pending_retains_activated_memory_projection_and_pending_gate_does_not_clear_it()
 {
    let call = model_tool_call("call-tool-pending");
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(vec![ModelOutput::tool_call(call)], FinishReason::ToolCalls),
        )])]);
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-tool-call-retains",
        provider.clone(),
        "memory-provider-tool-call",
        "Activated memory must survive a pending tool call and pending gate.",
        "memory-provider-tool-call-artifact",
    );

    let first_events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&first_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        runtime.pending_tool_calls().await,
        vec![pending_tool_call("call-tool-pending")]
    );
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-tool-call",
        "Activated memory must survive a pending tool call and pending gate.",
    )
    .await;

    let second_events = collect_step(
        &runtime,
        "Second topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(
        failed_code(&second_events),
        Some(DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED)
    );
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-tool-call",
        "Activated memory must survive a pending tool call and pending gate.",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_completion_retains_activated_memory_projection() {
    let provider = RecordingModelProvider::new();
    let (runtime, source) = runtime_with_provider_and_single_memory(
        "runtime-memory-provider-stop-retains",
        provider.clone(),
        "memory-provider-stop",
        "Activated memory must survive provider stop completion after setup.",
        "memory-provider-stop-artifact",
    );

    let events = collect_step(
        &runtime,
        "Topic request.",
        crate::StepContext::new(CancellationToken::new()),
    )
    .await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted"
        ]
    );
    assert_eq!(source.call_count(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Completed)
    );
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-stop",
        "Activated memory must survive provider stop completion after setup.",
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn provider_continuation_without_user_input_starts_a_new_model_turn() {
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text("first response")],
            FinishReason::Stop,
        ))]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::text("continuation response")],
            FinishReason::Stop,
        ))]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-no-input-new-turn"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "Start the loop.", crate::StepContext::default()).await;
    runtime
        .step(
            crate::StepInput::no_new_user_input(),
            crate::StepContext::default(),
        )
        .expect("continuation step should start")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(provider.recorded_requests().len(), 2);
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::Completed)
    );
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(2)),
        Some(ModelTurnStatus::Completed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_tool_call_response_aborts_turn_before_failed_event() {
    let duplicate = model_tool_call("duplicate-turn-call");
    let provider = RecordingModelProvider::with_script(vec![
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![ModelOutput::tool_call(duplicate.clone())],
            FinishReason::ToolCalls,
        ))]),
        ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
            vec![
                ModelOutput::text("commentary that must not partially commit"),
                ModelOutput::tool_call(duplicate),
            ],
            FinishReason::ToolCalls,
        ))]),
    ]);
    let runtime = Runtime::builder(session_id("runtime-duplicate-call-aborts-turn"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    collect_step(
        &runtime,
        "Request the first call.",
        crate::StepContext::default(),
    )
    .await;
    let pending = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .next()
        .expect("first call should be pending");
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                pending.id().clone(),
                ArtifactRef::new(artifact_id("duplicate-turn-result"), ArtifactKind::Text),
            ),
            ArtifactContent::text("resolved"),
        )
        .await
        .expect("first call should resolve");
    let transcript_before = runtime
        .inner
        .session
        .lock()
        .await
        .transcript_snapshot()
        .expect("transcript should be readable");

    let events = collect_step(
        &runtime,
        "Repeat the same call id.",
        crate::StepContext::default(),
    )
    .await;

    assert_eq!(failed_code(&events), Some("tool_call_duplicate"));
    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "Failed"],
        "a rejected compound response must not hide a committed commentary event"
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence + 1),
        "rejected response events must not contain an unobservable sequence gap"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, RuntimeJournalPayload::Cancelled { .. }))
    );
    let session = runtime.inner.session.lock().await;
    assert_eq!(
        session.model_turn_status(ModelTurnId::new(2)),
        Some(ModelTurnStatus::Aborted)
    );
    let transcript_after = session
        .transcript_snapshot()
        .expect("transcript should remain readable");
    assert_eq!(
        &transcript_after[..transcript_before.len()],
        transcript_before
    );
    assert!(matches!(
        transcript_after.last(),
        Some(crate::session::TranscriptItemSnapshot::UserMessage { text, .. })
            if text == "Repeat the same call id."
    ));
    assert_eq!(
        transcript_after.len(),
        transcript_before.len() + 1,
        "only the new user source may survive rejected tool-call admission"
    );
    assert_eq!(
        session.next_sequence(),
        events.last().expect("failed event should exist").sequence + 1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_after_tool_response_reduction_preserves_awaiting_turn() {
    let call = model_tool_call("reduced-before-cancel");
    let provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(
                vec![
                    ModelOutput::text("Tool commentary."),
                    ModelOutput::tool_call(call),
                ],
                FinishReason::ToolCalls,
            ),
        )])]);
    let runtime = Runtime::builder(session_id("runtime-reduced-tool-response-cancel"))
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero event buffer"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let token = CancellationToken::new();
    let mut stream = runtime
        .step(
            crate::StepInput::user_text("Request a tool call.").expect("valid input"),
            crate::StepContext::new(token.clone()),
        )
        .expect("step should start");

    assert!(matches!(
        stream.next().await.expect("session start event").payload,
        RuntimeJournalPayload::SessionStarted
    ));
    assert!(matches!(
        stream.next().await.expect("step start event").payload,
        RuntimeJournalPayload::StepStarted
    ));
    while runtime.pending_tool_calls().await.is_empty() {
        tokio::task::yield_now().await;
    }
    token.cancel();
    let remaining = stream.collect::<Vec<_>>().await;

    assert_eq!(
        event_kind_names(&remaining),
        ["AssistantOutputRecorded", "ToolCallPending", "Cancelled"],
        "committed response events must drain before cancellation becomes observable"
    );
    assert!(
        remaining
            .windows(2)
            .all(|pair| pair[1].sequence == pair[0].sequence + 1),
        "committed response and cancellation events must have contiguous sequences"
    );
    assert_eq!(runtime.pending_tool_calls().await.len(), 1);
    assert_eq!(
        runtime
            .inner
            .session
            .lock()
            .await
            .model_turn_status(ModelTurnId::new(1)),
        Some(ModelTurnStatus::AwaitingToolResults)
    );
}
