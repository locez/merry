use crate::{
    runtime::{
        DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
        tests::support::{
            common::{
                collect_step, completed_event_with, event_kind_names, failed_code, model_tool_call,
                pending_tool_call,
            },
            memory::{
                ScriptedMemoryActivationResponse, ScriptedMemoryActivationSource, activated_memory,
                assert_activated_memory_projection_cleared,
                assert_activated_memory_projection_retained, record_memory_artifact,
                runtime_with_provider_and_single_memory,
            },
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            runtime_factories::runtime_with_provider_and_memory_source,
        },
    },
    session::{ModelTurnId, ModelTurnStatus},
};
use merry_llm::{FinishReason, ModelError, ModelOutput, ProviderErrorKind};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

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
