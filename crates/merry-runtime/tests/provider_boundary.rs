use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, RuntimeEvent, RuntimeEventKind,
    SessionId, ToolName,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelError, ModelEvent, ModelMessageRole, ModelName,
    ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId, ProviderErrorKind, ToolArguments,
    testing::FakeModelProvider,
};
use merry_runtime::{
    ArtifactContent, ContextCompiler, ContextEvidence, ContextSummary, LedgerFactKind,
    LedgerProjection, Runtime, StepContext, StepInput,
};
use serde_json::Map;
use std::{num::NonZeroUsize, sync::Arc};
use tokio_util::sync::CancellationToken;

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

fn completed_event() -> ModelEvent {
    completed_event_with_finish(FinishReason::Stop)
}

fn completed_event_with_finish(finish_reason: FinishReason) -> ModelEvent {
    completed_outputs_event(vec![ModelOutput::text("model result")], finish_reason)
}

fn completed_text_event(text: &str) -> ModelEvent {
    completed_outputs_event(vec![ModelOutput::text(text)], FinishReason::Stop)
}

fn completed_outputs_event(outputs: Vec<ModelOutput>, finish_reason: FinishReason) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(outputs, finish_reason, None),
    }
}

fn model_tool_call() -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new("call-1").expect("valid tool call id"),
        ToolName::new("search_notes").expect("valid tool name"),
        ToolArguments::new(Map::new()),
    )
}

fn runtime_with_provider(session: &str, provider: FakeModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn runtime_with_provider_event_buffer(
    session: &str,
    provider: FakeModelProvider,
    event_buffer_size: usize,
) -> Runtime {
    Runtime::builder(session_id(session))
        .event_buffer_size(NonZeroUsize::new(event_buffer_size).expect("non-zero buffer"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

async fn collect_step(runtime: &Runtime, text: &str) -> Vec<RuntimeEvent> {
    runtime
        .step(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start")
        .collect()
        .await
}

fn event_kind_names(events: &[RuntimeEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event.kind {
            RuntimeEventKind::SessionStarted => "SessionStarted",
            RuntimeEventKind::StepStarted => "StepStarted",
            RuntimeEventKind::StepCompleted => "StepCompleted",
            RuntimeEventKind::Cancelled { .. } => "Cancelled",
            RuntimeEventKind::Failed { .. } => "Failed",
            RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
            RuntimeEventKind::EvidenceReferenced { .. } => "EvidenceReferenced",
            _ => "Unknown",
        })
        .collect()
}

fn failed_code(events: &[RuntimeEvent]) -> Option<&str> {
    events.iter().find_map(|event| match &event.kind {
        RuntimeEventKind::Failed { diagnostic } => Some(diagnostic.code()),
        _ => None,
    })
}

fn failed_sequence(events: &[RuntimeEvent]) -> u64 {
    events
        .iter()
        .find_map(|event| match event.kind {
            RuntimeEventKind::Failed { .. } => Some(event.sequence),
            _ => None,
        })
        .expect("failed event should be present")
}

fn assert_no_completion(events: &[RuntimeEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind, RuntimeEventKind::StepCompleted)),
        "terminal failure/cancellation must not be followed by StepCompleted: {events:?}"
    );
}

fn assert_no_artifact_recorded(events: &[RuntimeEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind, RuntimeEventKind::ArtifactRecorded { .. })),
        "terminal failure/cancellation must not record artifacts: {events:?}"
    );
}

fn assert_no_failed(events: &[RuntimeEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind, RuntimeEventKind::Failed { .. })),
        "terminal cancellation must not emit Failed: {events:?}"
    );
}

async fn record_valid_context(runtime: &Runtime) -> String {
    let artifact = ArtifactRef::new(
        artifact_id("provider-boundary-context-artifact"),
        ArtifactKind::Text,
    );
    runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("alpha\nbeta\ngamma\n"),
        )
        .await
        .expect("artifact should record through eventful path");
    let evidence = runtime
        .evidence_ref(
            artifact.id(),
            EvidenceLocator::line_range(2, 3).expect("valid line range"),
        )
        .await
        .expect("evidence should resolve");

    runtime
        .record_context_summary(
            ContextSummary::new(
                "provider-boundary-summary",
                "Provider boundary context is compiled.",
                vec![
                    ContextEvidence::new("selected lines", evidence)
                        .expect("valid context evidence"),
                ],
            )
            .expect("valid context summary"),
        )
        .await;

    ContextCompiler::new()
        .compile(&runtime.context_snapshot().await)
        .expect("context should compile")
        .to_snapshot()
}

fn assistant_output_artifact(events: &[RuntimeEvent]) -> &ArtifactRef {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ArtifactRecorded { artifact } => Some(artifact),
            _ => None,
        })
        .expect("assistant output artifact should be recorded")
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_compiles_user_text_request_and_records_assistant_output_artifact()
 {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::OutputTextDelta {
            delta: "ignored".to_owned(),
        }),
        Ok(completed_event()),
    ]);
    let runtime = runtime_with_provider("provider-user-text", provider.clone());

    let events = collect_step(&runtime, "Explain the runtime boundary.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    let artifact = assistant_output_artifact(&events);
    assert_eq!(artifact.id().as_str(), "assistant-output-2");
    assert_eq!(artifact.kind(), &ArtifactKind::Text);
    let evidence = runtime
        .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("artifact event should be observable only after artifact is readable");
    assert_eq!(evidence.artifact_id, *artifact.id());

    let projection = runtime.ledger_projection().await;
    assert_eq!(
        projection.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::StepCompleted,
            },
        ]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.model(), &model_name());
    assert_eq!(request.messages().len(), 1);
    assert_eq!(request.messages()[0].role(), ModelMessageRole::User);
    assert_eq!(
        request.messages()[0].content().as_text(),
        "Explain the runtime boundary."
    );
    assert!(request.tools().is_empty());
    assert_eq!(request.generation(), &GenerationConfig::default());
    assert!(!request.generation().allow_parallel_tool_calls());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_includes_compiled_context_as_system_message() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-context", provider.clone());
    let expected_snapshot = record_valid_context(&runtime).await;

    let events = collect_step(&runtime, "Use the stored context.").await;

    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.messages().len(), 2);
    assert_eq!(request.messages()[0].role(), ModelMessageRole::System);
    assert_eq!(request.messages()[0].content().as_text(), expected_snapshot);
    assert_eq!(request.messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        request.messages()[1].content().as_text(),
        "Use the stored context."
    );
    assert!(request.tools().is_empty());
    assert!(!request.generation().allow_parallel_tool_calls());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_multiline_text_artifact_supports_line_evidence() {
    let provider = FakeModelProvider::new(vec![Ok(completed_text_event(
        "first line\nsecond line\nthird line\n",
    ))]);
    let runtime = runtime_with_provider("provider-multiline-output", provider);

    let events = collect_step(&runtime, "Return multiple lines.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );
    let artifact = assistant_output_artifact(&events);
    let evidence = runtime
        .evidence_ref(
            artifact.id(),
            EvidenceLocator::line_range(2, 2).expect("valid line range"),
        )
        .await
        .expect("assistant output line evidence should resolve");
    assert_eq!(evidence.artifact_id, *artifact.id());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_success_with_single_slot_event_buffer_emits_artifact_and_completion() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider_event_buffer("provider-stop-buffer-one", provider, 1);

    let events = collect_step(&runtime, "Use a single-slot event buffer.").await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn second_provider_step_continues_sequences_and_does_not_replay_previous_assistant_output() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-second-step", provider.clone());

    let first_events = collect_step(&runtime, "First request.").await;
    let second_events = collect_step(&runtime, "Second request.").await;

    assert_eq!(
        first_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        second_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
    assert_eq!(
        event_kind_names(&second_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    assert_eq!(
        assistant_output_artifact(&first_events).id().as_str(),
        "assistant-output-2"
    );
    assert_eq!(
        assistant_output_artifact(&second_events).id().as_str(),
        "assistant-output-5"
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages().len(), 1);
    assert_eq!(requests[1].messages()[0].role(), ModelMessageRole::User);
    assert_eq!(
        requests[1].messages()[0].content().as_text(),
        "Second request."
    );
    assert!(
        requests[1]
            .messages()
            .iter()
            .all(|message| message.content().as_text() != "model result")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_step_with_provider_emits_failed_when_context_compile_fails() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-context-failure", provider.clone());
    runtime
        .record_context_summary(
            ContextSummary::new(
                "invalid-summary",
                "This summary has no evidence.",
                Vec::new(),
            )
            .expect("summary construction allows compiler validation"),
        )
        .await;

    let events = collect_step(&runtime, "Compile context.").await;

    assert_eq!(provider.recorded_requests().len(), 0);
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("context_compile"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stream_error_emits_failed_without_step_completed() {
    let provider = FakeModelProvider::new(vec![Err(merry_llm::ModelError::provider(
        ProviderErrorKind::Protocol,
        "bad\u{7}provider\nmessage",
    ))]);
    let runtime = runtime_with_provider("provider-stream-error", provider);

    let events = collect_step(&runtime, "Stream failure.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_protocol"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);

    let failed_sequence = failed_sequence(&events);
    let projection = runtime.ledger_projection().await;
    assert!(projection.entries().contains(&LedgerProjection::Lifecycle {
        sequence: failed_sequence,
        order: failed_sequence,
        kind: LedgerFactKind::Failed
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stream_eof_before_completed_emits_failed() {
    let provider = FakeModelProvider::new(vec![Ok(ModelEvent::Started)]);
    let runtime = runtime_with_provider("provider-eof", provider);

    let events = collect_step(&runtime, "EOF before completion.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_stream_eof"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_tool_call_requested_emits_unsupported_failed() {
    let call = model_tool_call();
    let provider = FakeModelProvider::new(vec![Ok(ModelEvent::ToolCallRequested { call })]);
    let runtime = runtime_with_provider("provider-tool-call", provider);

    let events = collect_step(&runtime, "Request a tool.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_requested"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_cancelled_stream_emits_cancelled_without_completion() {
    let provider = FakeModelProvider::new(vec![Err(merry_llm::ModelError::Cancelled)]);
    let runtime = runtime_with_provider("provider-cancelled", provider);

    let events = collect_step(&runtime, "Cancel in provider.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, RuntimeEventKind::Cancelled { .. }))
    );
    assert_no_artifact_recorded(&events);
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_tool_calls_finish_emits_failed_without_step_completed() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event_with_finish(
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-finish-tool-calls", provider);

    let events = collect_step(&runtime, "Finish with tool calls.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_requested"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_cancelled_finish_emits_cancelled_without_step_completed() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event_with_finish(
        FinishReason::Cancelled,
    ))]);
    let runtime = runtime_with_provider("provider-finish-cancelled", provider);

    let events = collect_step(&runtime, "Finish cancelled.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert_no_artifact_recorded(&events);
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_error_finish_emits_failed_without_step_completed() {
    let provider =
        FakeModelProvider::new(vec![Ok(completed_event_with_finish(FinishReason::Error))]);
    let runtime = runtime_with_provider("provider-finish-error", provider);

    let events = collect_step(&runtime, "Finish with error.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_finish_error"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_length_finish_emits_failed_without_step_completed() {
    let provider =
        FakeModelProvider::new(vec![Ok(completed_event_with_finish(FinishReason::Length))]);
    let runtime = runtime_with_provider("provider-finish-length", provider);

    let events = collect_step(&runtime, "Finish with length.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_length"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_cancelled_kind_error_emits_cancelled_without_failed_or_completion() {
    let provider = FakeModelProvider::new(vec![Err(ModelError::provider(
        ProviderErrorKind::Cancelled,
        "provider cancelled request",
    ))]);
    let runtime = runtime_with_provider("provider-cancelled-kind", provider);

    let events = collect_step(&runtime, "Provider cancellation kind.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Cancelled"]
    );
    assert_no_artifact_recorded(&events);
    assert_no_failed(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_with_multiple_outputs_emits_model_output_unsupported_failed() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::text("first"), ModelOutput::text("second")],
        FinishReason::Stop,
    ))]);
    let runtime = runtime_with_provider("provider-stop-multiple-outputs", provider);

    let events = collect_step(&runtime, "Return too many outputs.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_output_unsupported"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_stop_with_tool_output_emits_model_output_unsupported_failed() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::Stop,
    ))]);
    let runtime = runtime_with_provider("provider-stop-tool-output", provider);

    let events = collect_step(&runtime, "Return a tool output with stop.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_output_unsupported"));
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_absent_step_does_not_compile_context_and_preserves_skeleton_behavior() {
    let runtime = Runtime::builder(session_id("provider-absent"))
        .build()
        .expect("runtime should build");
    runtime
        .record_context_summary(
            ContextSummary::new(
                "invalid-summary-without-provider",
                "This would fail if the provider path compiled context.",
                Vec::new(),
            )
            .expect("summary construction allows compiler validation"),
        )
        .await;

    let events = collect_step(&runtime, "Run without provider.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "StepCompleted"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_no_artifact_recorded(&events);
}
