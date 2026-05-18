use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, PendingToolCall, RuntimeEvent,
    RuntimeEventKind, SessionId, ToolCallId, ToolCallResult, ToolCallResultStatus, ToolName,
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
use serde_json::{Map, Value, json};
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
    model_tool_call_with_args("call-1", "search_notes", Map::new())
}

fn model_tool_call_with_id(id: &str) -> ModelToolCall {
    model_tool_call_with_args(id, "search_notes", Map::new())
}

fn model_tool_call_with_args(id: &str, name: &str, arguments: Map<String, Value>) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::new(arguments),
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
    collect_step_with_context(runtime, text, StepContext::new(CancellationToken::new())).await
}

async fn collect_step_with_context(
    runtime: &Runtime,
    text: &str,
    context: StepContext,
) -> Vec<RuntimeEvent> {
    runtime
        .step(
            StepInput::user_text(text).expect("valid step input"),
            context,
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
            RuntimeEventKind::ToolCallPending { .. } => "ToolCallPending",
            RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
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

fn assert_no_tool_call_pending(events: &[RuntimeEvent]) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind, RuntimeEventKind::ToolCallPending { .. })),
        "terminal failure/cancellation must not record pending tool calls: {events:?}"
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

fn pending_tool_call(events: &[RuntimeEvent]) -> &PendingToolCall {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallPending { call } => Some(call),
            _ => None,
        })
        .expect("pending tool call should be emitted")
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
async fn runtime_step_with_provider_uses_step_generation_config() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-generation-config", provider.clone());
    let context = StepContext::new(CancellationToken::new()).with_generation_config(
        GenerationConfig::new(Some(16), false).expect("valid generation config"),
    );

    let events = collect_step_with_context(&runtime, "Limit the output.", context).await;

    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
    assert!(!requests[0].generation().allow_parallel_tool_calls());
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
async fn provider_streamed_tool_call_requested_emits_pending_without_completion() {
    let call = model_tool_call();
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested { call: call.clone() }),
        Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-streamed", provider);

    let events = collect_step(&runtime, "Request a tool.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
    assert!(failed_code(&events).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_multiple_tool_call_requests_fail_without_partial_pending() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-2"),
        }),
        Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-1"))],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-streamed-multiple", provider);

    let events = collect_step(&runtime, "Request multiple streamed tools.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("model_parallel_tool_calls_unsupported")
    );
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_tool_call_then_stop_text_fails_without_artifact_or_pending() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(completed_outputs_event(
            vec![ModelOutput::text("fallback text")],
            FinishReason::Stop,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-then-stop-text", provider);

    let events = collect_step(&runtime, "Request tool then stop with text.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_mixed_output"));
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_streamed_tool_call_then_completed_different_tool_call_fails_without_partial_pending()
 {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::Started),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call_with_id("call-1"),
        }),
        Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-2"))],
            FinishReason::ToolCalls,
        )),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-completed-different", provider);

    let events = collect_step(&runtime, "Request one tool and complete another.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("model_parallel_tool_calls_unsupported")
    );
    assert_no_tool_call_pending(&events);
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
async fn provider_completed_with_single_tool_call_emits_pending_without_completion() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-finish-tool-calls", provider);

    let events = collect_step(&runtime, "Finish with tool calls.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(pending_tool_call(&events).id().as_str(), "call-1");
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
    assert!(failed_code(&events).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_tool_call_pending_preserves_id_name_arguments_and_ledger_fact() {
    let arguments = Map::from_iter([
        ("query".to_owned(), json!("runtime tool calls")),
        ("limit".to_owned(), json!(3)),
        ("include_archived".to_owned(), json!(false)),
    ]);
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call_with_args(
            "call.provider/opaque.id:42",
            "search_notes",
            arguments.clone(),
        ))],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-payload", provider);

    let events = collect_step(&runtime, "Search notes.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let call = pending_tool_call(&events);
    assert_eq!(call.id().as_str(), "call.provider/opaque.id:42");
    assert_eq!(call.name().as_str(), "search_notes");
    assert_eq!(call.arguments().as_object(), &arguments);
    let mut pending = runtime.pending_tool_calls().await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id().as_str(), "call.provider/opaque.id:42");
    assert_eq!(pending[0].name().as_str(), "search_notes");
    assert_eq!(pending[0].arguments().as_object(), &arguments);

    pending.clear();
    let pending_after_caller_mutation = runtime.pending_tool_calls().await;
    assert_eq!(pending_after_caller_mutation, vec![call.clone()]);

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
                kind: LedgerFactKind::ToolCallPending,
            },
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_provider_tool_call_id_after_pending_fails_without_second_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-duplicate-id", provider);

    let first_events = collect_step(&runtime, "Request a tool.").await;
    let second_events = collect_step(&runtime, "Request the same tool id again.").await;

    assert_eq!(
        event_kind_names(&first_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&second_events), Some("tool_call_duplicate"));
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["call-1".to_owned()]
    );

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
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::Failed,
            },
        ]
    );

    let result_artifact = ArtifactRef::new(
        artifact_id("tool-result-after-duplicate-pending"),
        ArtifactKind::Text,
    );
    let result = ToolCallResult::succeeded(
        ToolCallId::new("call-1").expect("valid call id"),
        result_artifact.clone(),
    );
    let resolved_events = runtime
        .submit_tool_result(
            result.clone(),
            ArtifactContent::text("only accepted once\n"),
        )
        .await
        .expect("single pending call should resolve");
    let duplicate_result = ToolCallResult::succeeded(
        ToolCallId::new("call-1").expect("valid call id"),
        ArtifactRef::new(
            artifact_id("tool-result-after-duplicate-second"),
            ArtifactKind::Text,
        ),
    );
    let duplicate_err = runtime
        .submit_tool_result(
            duplicate_result,
            ArtifactContent::text("must not resolve twice\n"),
        )
        .await
        .expect_err("resolved call id should reject duplicate result");

    assert_eq!(
        event_kind_names(&resolved_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert!(matches!(
        duplicate_err,
        merry_runtime::RuntimeError::ToolCallAlreadyResolved { call_id, .. }
            if call_id == ToolCallId::new("call-1").expect("valid call id")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_records_success_artifact_resolves_pending_and_updates_ledger() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-success", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact = ArtifactRef::new(artifact_id("tool-result-success"), ArtifactKind::Text);
    let result = ToolCallResult::succeeded(call.id().clone(), result_artifact.clone());

    let events = runtime
        .submit_tool_result(result.clone(), ArtifactContent::text("exact tool output\n"))
        .await
        .expect("tool result should resolve");

    assert_eq!(
        event_kind_names(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(matches!(
        &events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == &result_artifact
    ));
    assert!(matches!(
        &events[1].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == &result
    ));
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("tool result artifact should be readable");
    assert_eq!(evidence.artifact_id, *result_artifact.id());

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
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );

    let next_events = collect_step(&runtime, "after tool result").await;
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn submit_failed_tool_result_preserves_diagnostic_and_failure_artifact_without_failed_event()
{
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-failed", provider);
    let pending_events = collect_step(&runtime, "Request a failing tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let diagnostic = merry_core::ErrorInfo::new("tool_failed", "Tool exited with status 2")
        .expect("valid diagnostic");
    let result_artifact = ArtifactRef::new(artifact_id("tool-result-failed"), ArtifactKind::Json);
    let result = ToolCallResult::failed(call.id().clone(), result_artifact.clone(), diagnostic);

    let events = runtime
        .submit_tool_result(
            result.clone(),
            ArtifactContent::json(r#"{"stderr":"permission denied"}"#),
        )
        .await
        .expect("failed tool result should resolve");

    assert_eq!(
        event_kind_names(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(matches!(
        &events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == &result_artifact
    ));
    assert!(matches!(
        &events[1].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved }
            if resolved.status() == ToolCallResultStatus::Failed
                && resolved.diagnostic().map(merry_core::ErrorInfo::code) == Some("tool_failed")
                && resolved == &result
    ));
    assert!(
        pending_events
            .iter()
            .chain(events.iter())
            .all(|event| !matches!(event.kind, RuntimeEventKind::Failed { .. })),
        "tool execution failure must be represented as ToolCallResolved, not RuntimeEventKind::Failed"
    );
    let evidence = runtime
        .evidence_ref(result_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("failure artifact should be readable");
    assert_eq!(evidence.artifact_id, *result_artifact.id());

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
                kind: LedgerFactKind::ToolCallPending,
            },
            LedgerProjection::Lifecycle {
                sequence: 3,
                order: 3,
                kind: LedgerFactKind::ArtifactRecorded,
            },
            LedgerProjection::Lifecycle {
                sequence: 4,
                order: 4,
                kind: LedgerFactKind::ToolCallResolved,
            },
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_tool_result_after_resolved_does_not_mutate_session() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-duplicate", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let first_artifact = ArtifactRef::new(artifact_id("tool-result-first"), ArtifactKind::Text);
    let first = ToolCallResult::succeeded(call.id().clone(), first_artifact.clone());
    runtime
        .submit_tool_result(first, ArtifactContent::text("first result\n"))
        .await
        .expect("first result should resolve");
    let projection_before_duplicate = runtime.ledger_projection().await;
    let duplicate_artifact =
        ArtifactRef::new(artifact_id("tool-result-duplicate"), ArtifactKind::Text);
    let duplicate = ToolCallResult::succeeded(call.id().clone(), duplicate_artifact.clone());

    let err = runtime
        .submit_tool_result(duplicate, ArtifactContent::text("duplicate result\n"))
        .await
        .expect_err("duplicate result should be rejected");
    let projection_after_duplicate = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ToolCallAlreadyResolved {
            session_id: rejected_session,
            call_id
        } if rejected_session == session_id("provider-tool-result-duplicate")
            && call_id == ToolCallId::new("call-1").expect("valid call id")
    ));
    assert_eq!(projection_before_duplicate, projection_after_duplicate);
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence_err = runtime
        .evidence_ref(duplicate_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("duplicate artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *duplicate_artifact.id()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn artifact_error_while_submitting_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-artifact-error", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let duplicate_artifact =
        ArtifactRef::new(artifact_id("tool-result-conflict"), ArtifactKind::Text);
    runtime
        .record_artifact(
            duplicate_artifact.clone(),
            ArtifactContent::text("existing artifact\n"),
        )
        .await
        .expect("conflicting artifact should record before submit");
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(call.id().clone(), duplicate_artifact.clone());

    let err = runtime
        .submit_tool_result(result, ArtifactContent::text("replacement\n"))
        .await
        .expect_err("duplicate artifact id should reject submit");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::DuplicateId { id }
        } if id == *duplicate_artifact.id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);

    let next_events = collect_step(&runtime, "after duplicate artifact submit").await;
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![4, 5]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn incompatible_tool_result_content_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-incompatible", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("tool-result-json-mismatch"), ArtifactKind::Json),
    );

    let err = runtime
        .submit_tool_result(
            result.clone(),
            ArtifactContent::text("not json content kind\n"),
        )
        .await
        .expect_err("content kind mismatch should reject submit");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::IncompatibleContent {
                id,
                artifact_kind,
                content_kind
            }
        } if id == *result.artifact().id()
            && artifact_kind == ArtifactKind::Json
            && content_kind == merry_runtime::ArtifactContentKind::Text
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("incompatible result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_rejects_unsupported_content_kind_without_mutation() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-unsupported", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("tool-result-binary"), ArtifactKind::Binary),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::binary([1, 2, 3]))
        .await
        .expect_err("binary tool result content is not accepted in MVP");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Binary
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_empty_tool_call_args_succeeds() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-empty-args", provider);

    let events = collect_step(&runtime, "Call with empty args.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert!(
        pending_tool_call(&events)
            .arguments()
            .as_object()
            .is_empty()
    );
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_multiple_tool_calls_fails_without_partial_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![
            ModelOutput::tool_call(model_tool_call_with_id("call-1")),
            ModelOutput::tool_call(model_tool_call_with_id("call-2")),
        ],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-multiple", provider);

    let events = collect_step(&runtime, "Return multiple tool calls.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(
        failed_code(&events),
        Some("model_parallel_tool_calls_unsupported")
    );
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_tool_calls_finish_but_no_tool_call_fails() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        Vec::new(),
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-missing", provider);

    let events = collect_step(&runtime, "Finish without tool call payload.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_missing"));
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_completed_with_mixed_text_and_tool_call_fails_without_partial_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![
            ModelOutput::text("partial answer"),
            ModelOutput::tool_call(model_tool_call()),
        ],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-mixed-output", provider);

    let events = collect_step(&runtime, "Mix text and tool call.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_mixed_output"));
    assert_no_tool_call_pending(&events);
    assert_no_artifact_recorded(&events);
    assert_no_completion(&events);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_tool_call_after_non_empty_text_delta_fails_without_partial_pending() {
    let provider = FakeModelProvider::new(vec![
        Ok(ModelEvent::OutputTextDelta {
            delta: "thinking aloud".to_owned(),
        }),
        Ok(ModelEvent::ToolCallRequested {
            call: model_tool_call(),
        }),
    ]);
    let runtime = runtime_with_provider("provider-tool-call-after-text-delta", provider);

    let events = collect_step(&runtime, "Emit text before tool call.").await;

    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&events), Some("model_tool_call_mixed_output"));
    assert_no_tool_call_pending(&events);
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
