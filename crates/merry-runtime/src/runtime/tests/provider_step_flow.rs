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

    drop(session);
    tokio::task::yield_now().await;

    assert_activated_memory_projection_cleared(&runtime).await;
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
    assert_activated_memory_projection_retained(
        &runtime,
        "memory-provider-stop",
        "Activated memory must survive provider stop completion after setup.",
    )
    .await;
}
