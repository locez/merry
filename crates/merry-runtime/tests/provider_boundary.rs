use futures_util::{StreamExt, stream};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, EvidenceLocator, PendingToolCall,
    ProviderName, RuntimeEvent, RuntimeEventKind, SessionId, ToolCallId, ToolCallResult,
    ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent, ModelEventStream,
    ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
    ModelResponse, ModelStreamContext, ModelToolCall, ModelToolCallId, ProviderErrorKind,
    ToolArguments, testing::FakeModelProvider,
};
use merry_runtime::{
    ArtifactContent, ArtifactContentKind, ArtifactError, ContextCompiler, ContextEvidence,
    ContextSummary, LedgerFactKind, LedgerProjection, RegisteredTool, Runtime, StepContext,
    StepInput, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::{Map, Value, json};
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};
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

fn runtime_with_scripted_provider(session: &str, provider: ScriptedModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

#[derive(Debug, Clone)]
struct ScriptedModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<Vec<ScriptedProviderStep>>>,
    recorded_requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[derive(Debug)]
enum ScriptedProviderStep {
    Stream(Vec<Result<ModelEvent, ModelError>>),
    SetupError(ModelError),
}

impl ScriptedModelProvider {
    fn new(scripts: Vec<Vec<Result<ModelEvent, ModelError>>>) -> Self {
        Self::new_steps(
            scripts
                .into_iter()
                .map(ScriptedProviderStep::Stream)
                .collect(),
        )
    }

    fn new_steps(steps: Vec<ScriptedProviderStep>) -> Self {
        Self {
            name: ProviderName::new("scripted-model-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.recorded_requests
            .lock()
            .expect("recorded requests mutex should not be poisoned")
            .clone()
    }
}

impl ModelProvider for ScriptedModelProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            self.recorded_requests
                .lock()
                .expect("recorded requests mutex should not be poisoned")
                .push(request);

            let step = self
                .steps
                .lock()
                .expect("steps mutex should not be poisoned")
                .pop()
                .unwrap_or_else(|| ScriptedProviderStep::Stream(Vec::new()));

            match step {
                ScriptedProviderStep::Stream(script) => {
                    let event_stream: ModelEventStream = Box::pin(stream::iter(script));
                    Ok(event_stream)
                }
                ScriptedProviderStep::SetupError(error) => Err(error),
            }
        })
    }
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

fn failed_tool_result(
    call_id: ToolCallId,
    artifact: ArtifactRef,
    code: &str,
    message: &str,
) -> ToolCallResult {
    ToolCallResult::failed(
        call_id,
        artifact,
        merry_core::ErrorInfo::new(code, message).expect("valid diagnostic"),
    )
}

fn test_tool_spec(name: &str) -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" }
        },
        "required": ["query"]
    }))
    .expect("test schema should be a JSON schema");

    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Search test notes",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

#[derive(Clone)]
struct ScriptedToolExecutor {
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    response: ToolExecutorResponse,
}

#[derive(Clone)]
enum ToolExecutorResponse {
    Outcome(ToolExecutionOutcome),
    Error(Arc<dyn Fn() -> ToolExecutionError + Send + Sync>),
}

impl ScriptedToolExecutor {
    fn succeeding_text(text: &str) -> Self {
        Self::new(ToolExecutorResponse::Outcome(
            ToolExecutionOutcome::succeeded_text(text),
        ))
    }

    fn failing_json(code: &str, content: &str) -> Self {
        Self::new(ToolExecutorResponse::Outcome(
            ToolExecutionOutcome::failed_json(
                content,
                ErrorInfo::new(code, "tool domain failure").expect("valid diagnostic"),
            ),
        ))
    }

    fn infrastructure_error(message: &str) -> Self {
        let message = message.to_owned();
        Self::new(ToolExecutorResponse::Error(Arc::new(move || {
            ToolExecutionError::infrastructure(message.clone())
        })))
    }

    fn new(response: ToolExecutorResponse) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response,
        }
    }

    fn calls(&self) -> Vec<PendingToolCall> {
        self.calls
            .lock()
            .expect("tool calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for ScriptedToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("tool calls mutex should not be poisoned")
                .push(call);

            match &self.response {
                ToolExecutorResponse::Outcome(outcome) => Ok(outcome.clone()),
                ToolExecutorResponse::Error(error) => Err(error()),
            }
        })
    }
}

#[derive(Clone)]
struct ReentrantMutationExecutor {
    runtime: Arc<Mutex<Option<Runtime>>>,
    observations: Arc<Mutex<Vec<ReentrantMutationObservation>>>,
}

impl ReentrantMutationExecutor {
    fn new() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(None)),
            observations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn set_runtime(&self, runtime: Runtime) {
        *self
            .runtime
            .lock()
            .expect("reentrant runtime mutex should not be poisoned") = Some(runtime);
    }

    fn observations(&self) -> Vec<ReentrantMutationObservation> {
        self.observations
            .lock()
            .expect("reentrant observations mutex should not be poisoned")
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReentrantMutationObservation {
    RecordStepAlreadyActive,
    SubmitStepAlreadyActive,
    Other(String),
}

impl ToolExecutor for ReentrantMutationExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let runtime = self
                .runtime
                .lock()
                .expect("reentrant runtime mutex should not be poisoned")
                .clone()
                .expect("runtime should be installed before executor runs");
            let artifact =
                ArtifactRef::new(artifact_id("executor-inner-artifact"), ArtifactKind::Text);
            let record_observation = match runtime
                .record_artifact(
                    artifact.clone(),
                    ArtifactContent::text("inner artifact must not record\n"),
                )
                .await
            {
                Ok(_) => ReentrantMutationObservation::Other(
                    "record_artifact unexpectedly succeeded".to_owned(),
                ),
                Err(merry_runtime::RuntimeError::StepAlreadyActive { .. }) => {
                    ReentrantMutationObservation::RecordStepAlreadyActive
                }
                Err(error) => ReentrantMutationObservation::Other(error.to_string()),
            };
            self.observations
                .lock()
                .expect("reentrant observations mutex should not be poisoned")
                .push(record_observation);

            let result = ToolCallResult::succeeded(call.id().clone(), artifact);
            let submit_observation = match runtime
                .submit_tool_result(
                    result,
                    ArtifactContent::text("inner result must not resolve\n"),
                )
                .await
            {
                Ok(_) => ReentrantMutationObservation::Other(
                    "submit_tool_result unexpectedly succeeded".to_owned(),
                ),
                Err(merry_runtime::RuntimeError::StepAlreadyActive { .. }) => {
                    ReentrantMutationObservation::SubmitStepAlreadyActive
                }
                Err(error) => ReentrantMutationObservation::Other(error.to_string()),
            };
            self.observations
                .lock()
                .expect("reentrant observations mutex should not be poisoned")
                .push(submit_observation);

            Ok(ToolExecutionOutcome::succeeded_text(
                "outer executor result\n",
            ))
        })
    }
}

fn runtime_with_registered_tool(
    session: &str,
    provider: ScriptedModelProvider,
    executor: ScriptedToolExecutor,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(RegisteredTool::new(
            test_tool_spec("search_notes"),
            Arc::new(executor),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn resolved_tool_result(events: &[RuntimeEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("resolved tool call should be emitted")
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
async fn reserved_assistant_output_external_recording_does_not_block_runtime_owned_output() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let runtime = runtime_with_provider("provider-reserved-assistant-output", provider);
    let artifact = ArtifactRef::new(artifact_id("assistant-output-3"), ArtifactKind::Text);
    let before = runtime.ledger_projection().await;

    let err = runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("external shadow output\n"),
        )
        .await
        .expect_err("external recording should not use runtime-owned assistant output ids");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ReservedArtifactId { artifact_id } if artifact_id == *artifact.id()
    ));
    assert_eq!(before, after);
    let evidence_err = runtime
        .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("reserved artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *artifact.id()
    ));

    let events = collect_step(&runtime, "after reserved artifact").await;
    assert_eq!(
        event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted"
        ]
    );
    let generated = assistant_output_artifact(&events);
    assert_eq!(generated.id().as_str(), "assistant-output-2");
    let evidence = runtime
        .evidence_ref(generated.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("runtime-owned assistant output should be readable");
    assert_eq!(evidence.artifact_id, *generated.id());
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
async fn registered_tool_specs_are_compiled_into_provider_request() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let tool = test_tool_spec("search_notes");
    let runtime = Runtime::builder(session_id("provider-registered-tool-spec"))
        .register_tool(RegisteredTool::new(
            tool.clone(),
            Arc::new(ScriptedToolExecutor::succeeding_text("unused")),
        ))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let events = collect_step(&runtime, "Use registered tools.").await;

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
    assert_eq!(requests[0].tools(), &[tool]);
    assert!(requests[0].continuations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn execute_registered_tool_success_records_artifact_resolves_and_compiles_continuation() {
    let call = model_tool_call_with_args(
        "call-success",
        "search_notes",
        Map::from_iter([("query".to_owned(), Value::String("alpha".to_owned()))]),
    );
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call.clone())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used tool result"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-success",
        provider.clone(),
        executor.clone(),
    );

    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();
    let reserved_artifact = ArtifactRef::new(artifact_id("tool-result-4"), ArtifactKind::Text);
    let before_reserved = runtime.ledger_projection().await;
    let reserved_err = runtime
        .record_artifact(
            reserved_artifact.clone(),
            ArtifactContent::text("external shadow result\n"),
        )
        .await
        .expect_err("external recording should not use runtime-owned tool result ids");
    let after_reserved = runtime.ledger_projection().await;

    assert!(matches!(
        reserved_err,
        merry_runtime::RuntimeError::ReservedArtifactId { artifact_id }
            if artifact_id == *reserved_artifact.id()
    ));
    assert_eq!(before_reserved, after_reserved);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending.clone()]);
    let evidence_err = runtime
        .evidence_ref(reserved_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("reserved tool result id must not be externally recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == *reserved_artifact.id()
    ));

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool execution should resolve");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &execution_events[1].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Text);
    assert_eq!(result.call_id(), pending.id());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("executor result artifact should be readable after ArtifactRecorded");
    assert_eq!(evidence.artifact_id, *result.artifact().id());
    assert_eq!(executor.calls(), vec![pending.clone()]);
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

    let continuation_events = collect_step(&runtime, "Continue with result.").await;
    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    assert_eq!(
        continuation_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools(), &[test_tool_spec("search_notes")]);
    assert!(requests[0].continuations().is_empty());
    assert_eq!(requests[1].tools(), &[test_tool_spec("search_notes")]);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("tool result continuation should be compiled");
    assert_eq!(continuation.call().id().as_str(), "call-success");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("search result\n")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_domain_failure_resolves_failed_without_runtime_failed() {
    let call = model_tool_call_with_id("call-domain-failure");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::failing_json("tool_lookup_failed", r#"{"ok":false}"#);
    let runtime = runtime_with_registered_tool("provider-execute-tool-failure", provider, executor);
    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("domain failure should resolve the pending call");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(
        execution_events
            .iter()
            .all(|event| !matches!(event.kind, RuntimeEventKind::Failed { .. })),
        "tool domain failure must not emit RuntimeEventKind::Failed: {execution_events:?}"
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &execution_events[1].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(result.call_id(), pending.id());
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should have diagnostic")
            .code(),
        "tool_lookup_failed"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("domain failure artifact should be readable after ArtifactRecorded");
    assert_eq!(evidence.artifact_id, *result.artifact().id());
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
async fn executor_infrastructure_error_keeps_pending_without_artifact_or_result() {
    let call = model_tool_call_with_id("call-infra-failure");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::infrastructure_error("temporary executor outage");
    let runtime =
        runtime_with_registered_tool("provider-execute-tool-infra-error", provider, executor);
    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;
    assert_eq!(
        before.entries(),
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

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("infrastructure failure should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ToolExecutionFailed { call_id, .. }
            if call_id == *pending.id()
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("infrastructure failure must not record runtime-owned tool result artifact");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_blank_text_outcome_keeps_pending_without_artifact_or_result() {
    let call = model_tool_call_with_id("call-blank-text-outcome");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text(" \n\t ");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-blank-text-outcome",
        provider,
        executor,
    );
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("blank executor text should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: ArtifactContentKind::Text
        } if artifact_id.as_str() == "tool-result-3"
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("blank executor outcome must not record an artifact");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_blank_json_outcome_keeps_pending_without_artifact_or_result() {
    let call = model_tool_call_with_id("call-blank-json-outcome");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::new(ToolExecutorResponse::Outcome(
        ToolExecutionOutcome::succeeded_json(" \n\t "),
    ));
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-blank-json-outcome",
        provider,
        executor,
    );
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("blank executor JSON should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: ArtifactContentKind::Json
        } if artifact_id.as_str() == "tool-result-3"
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("blank executor outcome must not record an artifact");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn executor_reentrant_runtime_mutations_are_rejected_while_outer_execution_resolves() {
    let call = model_tool_call_with_id("call-reentrant-executor");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ReentrantMutationExecutor::new();
    let runtime = Runtime::builder(session_id("provider-execute-tool-reentrant"))
        .register_tool(RegisteredTool::new(
            test_tool_spec("search_notes"),
            Arc::new(executor.clone()),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    executor.set_runtime(runtime.clone());
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("outer registered executor result should resolve");

    assert_eq!(
        executor.observations(),
        [
            ReentrantMutationObservation::RecordStepAlreadyActive,
            ReentrantMutationObservation::SubmitStepAlreadyActive,
        ]
    );
    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.call_id(), pending.id());
    assert!(runtime.pending_tool_calls().await.is_empty());
    let outer_evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("outer executor result artifact should be readable");
    assert_eq!(outer_evidence.artifact_id, *result.artifact().id());
    let inner_evidence_err = runtime
        .evidence_ref(
            &artifact_id("executor-inner-artifact"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("reentrant executor artifact must not be recorded");
    assert!(matches!(
        inner_evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("executor-inner-artifact")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn unregistered_pending_tool_name_resolves_failed_with_tool_not_registered() {
    let call = model_tool_call_with_args("call-unregistered", "missing_tool", Map::new());
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after missing tool"))],
    ]);
    let runtime = Runtime::builder(session_id("provider-execute-tool-unregistered"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    let pending_events = collect_step(&runtime, "Call missing tool.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("unregistered tool should synthesize failed result");

    assert_eq!(
        event_kind_names(&execution_events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    let result = resolved_tool_result(&execution_events);
    assert!(matches!(
        &execution_events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &execution_events[1].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("failed result should have diagnostic")
            .code(),
        "tool_not_registered"
    );
    assert_eq!(result.artifact().id().as_str(), "tool-result-3");
    assert_eq!(result.artifact().kind(), &ArtifactKind::Json);
    assert_eq!(result.call_id(), pending.id());
    assert!(failed_code(&execution_events).is_none());
    assert!(runtime.pending_tool_calls().await.is_empty());
    let evidence = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect("unregistered tool failure artifact should be readable after ArtifactRecorded");
    assert_eq!(evidence.artifact_id, *result.artifact().id());
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

    let continuation_events = collect_step(&runtime, "Continue after missing tool.").await;
    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    assert_eq!(
        continuation_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7]
    );
    assert_eq!(provider.recorded_requests()[1].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_with_bad_or_missing_args_still_reaches_executor() {
    let call = model_tool_call_with_args("call-bad-args", "search_notes", Map::new());
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text("accepted bad args\n");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-schema-pass",
        provider,
        executor.clone(),
    );
    let pending_events = collect_step(&runtime, "Search with missing args.").await;
    let pending = pending_tool_call(&pending_events).clone();

    runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("runtime should not schema-validate tool arguments");

    let calls = executor.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id().as_str(), "call-bad-args");
    assert!(calls[0].arguments().as_object().is_empty());
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
async fn unresolved_pending_tool_call_blocks_next_provider_step_without_calling_provider() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("must not be requested"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-pending-blocks-step", provider.clone());

    let first_events = collect_step(&runtime, "Request a tool.").await;
    let second_events = collect_step(&runtime, "Try to continue without tool result.").await;

    assert_eq!(
        event_kind_names(&first_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(
        failed_code(&second_events),
        Some("tool_call_result_required")
    );
    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["call-1".to_owned()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_provider_tool_call_id_after_pending_fails_without_second_pending() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-call-duplicate-id", provider.clone());

    let first_events = collect_step(&runtime, "Request a tool.").await;
    let second_events = collect_step(&runtime, "Request the same tool id again.").await;

    assert_eq!(
        event_kind_names(&first_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
    assert_eq!(
        failed_code(&second_events),
        Some("tool_call_result_required")
    );
    assert_eq!(provider.recorded_requests().len(), 1);
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
        artifact_id("manual-result-after-duplicate-pending"),
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
            artifact_id("manual-result-after-duplicate-second"),
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
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after tool result"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-tool-result-success", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact =
        ArtifactRef::new(artifact_id("manual-result-success"), ArtifactKind::Text);
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
        event_kind_names(&next_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    assert_eq!(provider.recorded_requests().len(), 2);
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![5, 6, 7]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn submit_tool_result_rejects_reserved_artifact_ids_without_mutation() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-reserved-submit", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    for reserved_id in ["tool-result-4", "assistant-output-4"] {
        let artifact = ArtifactRef::new(artifact_id(reserved_id), ArtifactKind::Text);
        let result = ToolCallResult::succeeded(call.id().clone(), artifact.clone());
        let err = runtime
            .submit_tool_result(result, ArtifactContent::text("external shadow result\n"))
            .await
            .expect_err("external submit should not use runtime-owned artifact ids");
        let after = runtime.ledger_projection().await;

        assert!(matches!(
            err,
            merry_runtime::RuntimeError::ReservedArtifactId { artifact_id }
                if artifact_id == *artifact.id()
        ));
        assert_eq!(before, after);
        assert_eq!(runtime.pending_tool_calls().await, vec![call.clone()]);
        let evidence_err = runtime
            .evidence_ref(artifact.id(), EvidenceLocator::whole_artifact())
            .await
            .expect_err("reserved submitted result artifact must not be recorded");
        assert!(matches!(
            evidence_err,
            merry_runtime::RuntimeError::Artifact {
                source: merry_runtime::ArtifactError::MissingArtifact { id }
            } if id == *artifact.id()
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn submitted_tool_result_is_compiled_as_provider_neutral_continuation() {
    let arguments = Map::from_iter([
        ("query".to_owned(), json!("runtime continuation")),
        ("limit".to_owned(), json!(2)),
    ]);
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_args(
                "call.provider/opaque.id:42",
                "search_notes",
                arguments.clone(),
            ))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("continued after search"))],
    ]);
    let runtime = runtime_with_scripted_provider("provider-tool-continuation", provider.clone());
    let pending_events = collect_step(&runtime, "Request a search.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-continuation-text"),
        ArtifactKind::Text,
    );
    let result = ToolCallResult::succeeded(call.id().clone(), result_artifact.clone());
    runtime
        .submit_tool_result(result, ArtifactContent::text("exact search result\n"))
        .await
        .expect("tool result should resolve");

    let events = collect_step(&runtime, "Use the tool result.").await;

    assert_eq!(
        event_kind_names(&events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("continuation should be compiled");
    assert_eq!(
        continuation.call().id().as_str(),
        "call.provider/opaque.id:42"
    );
    assert_eq!(continuation.call().name().as_str(), "search_notes");
    assert_eq!(continuation.call().arguments().as_object(), &arguments);
    assert_eq!(
        continuation.result().call_id().as_str(),
        "call.provider/opaque.id:42"
    );
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("exact search result\n")
    );
    assert!(continuation.result().diagnostic().is_none());

    let value = serde_json::to_value(&requests[1]).expect("request should serialize");
    assert!(value.get("session_id").is_none());
    assert!(value.get("ledger_id").is_none());
    assert!(value.get("artifact_id").is_none());
    assert!(value.get("previous_response_id").is_none());
    assert!(value.get("store").is_none());
    assert!(value.get("tool_call_id").is_none());
    assert!(
        value["continuations"][0]["call"]
            .get("tool_call_id")
            .is_none()
    );
    assert!(
        value["continuations"][0]["result"]
            .get("tool_call_id")
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn successful_provider_step_consumes_sent_tool_continuation_without_replay() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("used tool result"))],
        vec![Ok(completed_text_event("fresh request"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-consume", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-consumed-on-success"),
        ArtifactKind::Text,
    );
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(call.id().clone(), result_artifact),
            ArtifactContent::text("result to consume\n"),
        )
        .await
        .expect("tool result should resolve");

    let continuation_events = collect_step(&runtime, "Use tool result.").await;
    let next_events = collect_step(&runtime, "Do not replay it.").await;

    assert_eq!(
        event_kind_names(&continuation_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    assert_eq!(
        event_kind_names(&next_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert!(requests[2].continuations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn sent_tool_continuation_is_consumed_when_provider_records_new_pending_call() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-new"))],
            FinishReason::ToolCalls,
        ))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-new-pending", provider.clone());
    let pending_events = collect_step(&runtime, "Request first tool.").await;
    let old_call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                old_call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-before-new-pending"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("old result\n"),
        )
        .await
        .expect("old tool result should resolve");

    let new_pending_events = collect_step(&runtime, "Use old result and request another.").await;
    let blocked_events = collect_step(&runtime, "Do not replay old result.").await;

    assert_eq!(
        event_kind_names(&new_pending_events),
        ["StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_tool_call(&new_pending_events).id().as_str(),
        "call-new"
    );
    assert_eq!(event_kind_names(&blocked_events), ["StepStarted", "Failed"]);
    assert_eq!(
        failed_code(&blocked_events),
        Some("tool_call_result_required")
    );
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["call-new".to_owned()]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-old"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_new_tool_call_id_does_not_consume_sent_tool_continuation() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call_with_id("call-old"))],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("retry consumes old result"))],
    ]);
    let runtime = runtime_with_scripted_provider(
        "provider-tool-continuation-duplicate-new-id",
        provider.clone(),
    );
    let pending_events = collect_step(&runtime, "Request first tool.").await;
    let old_call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                old_call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-before-duplicate-new-id"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("old result\n"),
        )
        .await
        .expect("old tool result should resolve");

    let duplicate_events = collect_step(&runtime, "Provider repeats resolved id.").await;
    let retry_events = collect_step(&runtime, "Retry after duplicate id.").await;

    assert_eq!(
        event_kind_names(&duplicate_events),
        ["StepStarted", "Failed"]
    );
    assert_eq!(failed_code(&duplicate_events), Some("tool_call_duplicate"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-old"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        "call-old"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_error_does_not_consume_tool_continuation_and_retry_replays_it() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Err(ModelError::provider(
            ProviderErrorKind::Protocol,
            "transient continuation failure",
        ))],
        vec![Ok(completed_text_event("retry succeeded"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-error", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-error"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry me\n"),
        )
        .await
        .expect("tool result should resolve");

    let error_events = collect_step(&runtime, "Use result, provider fails.").await;
    let retry_events = collect_step(&runtime, "Retry with same result.").await;

    assert_eq!(event_kind_names(&error_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&error_events), Some("model_protocol"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_setup_error_does_not_consume_tool_continuation_and_retry_replays_it() {
    let provider = ScriptedModelProvider::new_steps(vec![
        ScriptedProviderStep::Stream(vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))]),
        ScriptedProviderStep::SetupError(ModelError::provider(
            ProviderErrorKind::Unavailable,
            "setup unavailable",
        )),
        ScriptedProviderStep::Stream(vec![Ok(completed_text_event("retry after setup"))]),
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-setup-error", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-setup-error"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry after setup\n"),
        )
        .await
        .expect("tool result should resolve");

    let error_events = collect_step(&runtime, "Setup fails.").await;
    let retry_events = collect_step(&runtime, "Retry setup.").await;

    assert_eq!(event_kind_names(&error_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&error_events), Some("model_unavailable"));
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn provider_cancel_does_not_consume_tool_continuation_and_retry_replays_it() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Err(ModelError::Cancelled)],
        vec![Ok(completed_text_event("retry after cancel"))],
    ]);
    let runtime =
        runtime_with_scripted_provider("provider-tool-continuation-cancel", provider.clone());
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(
                call.id().clone(),
                ArtifactRef::new(
                    artifact_id("manual-result-retry-after-cancel"),
                    ArtifactKind::Text,
                ),
            ),
            ArtifactContent::text("retry after cancel\n"),
        )
        .await
        .expect("tool result should resolve");

    let cancel_events = collect_step(&runtime, "Provider cancels.").await;
    let retry_events = collect_step(&runtime, "Retry after cancel.").await;

    assert_eq!(
        event_kind_names(&cancel_events),
        ["StepStarted", "Cancelled"]
    );
    assert_no_failed(&cancel_events);
    assert_eq!(
        event_kind_names(&retry_events),
        ["StepStarted", "ArtifactRecorded", "StepCompleted"]
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(requests[2].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn failed_tool_result_status_diagnostic_and_content_are_compiled_without_runtime_failed_event()
 {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_outputs_event(
            vec![ModelOutput::tool_call(model_tool_call())],
            FinishReason::ToolCalls,
        ))],
        vec![Ok(completed_text_event("handled failed tool result"))],
    ]);
    let runtime = runtime_with_scripted_provider(
        "provider-tool-continuation-failed-result",
        provider.clone(),
    );
    let pending_events = collect_step(&runtime, "Request a failing tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let result_artifact = ArtifactRef::new(
        artifact_id("manual-result-continuation-failed-json"),
        ArtifactKind::Json,
    );
    let result = failed_tool_result(
        call.id().clone(),
        result_artifact.clone(),
        "tool_failed",
        "Tool exited with status 2",
    );
    let resolved_events = runtime
        .submit_tool_result(
            result,
            ArtifactContent::json(r#"{"stderr":"permission denied"}"#),
        )
        .await
        .expect("failed tool result should resolve");
    let continuation_events = collect_step(&runtime, "Continue after failed tool.").await;

    assert!(
        pending_events
            .iter()
            .chain(resolved_events.iter())
            .chain(continuation_events.iter())
            .all(|event| !matches!(event.kind, RuntimeEventKind::Failed { .. })),
        "tool execution failure should be model-visible data, not runtime failure"
    );
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let continuation = requests[1]
        .continuations()
        .first()
        .expect("failed result continuation should be compiled");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .map(merry_core::ErrorInfo::code),
        Some("tool_failed")
    );
    assert_eq!(
        continuation.result().content().as_json(),
        Some(r#"{"stderr":"permission denied"}"#)
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
    let result_artifact = ArtifactRef::new(artifact_id("manual-result-failed"), ArtifactKind::Json);
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
    let first_artifact = ArtifactRef::new(artifact_id("manual-result-first"), ArtifactKind::Text);
    let first = ToolCallResult::succeeded(call.id().clone(), first_artifact.clone());
    runtime
        .submit_tool_result(first, ArtifactContent::text("first result\n"))
        .await
        .expect("first result should resolve");
    let projection_before_duplicate = runtime.ledger_projection().await;
    let duplicate_artifact =
        ArtifactRef::new(artifact_id("manual-result-duplicate"), ArtifactKind::Text);
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
        ArtifactRef::new(artifact_id("manual-result-conflict"), ArtifactKind::Text);
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
    assert_eq!(event_kind_names(&next_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&next_events), Some("tool_call_result_required"));
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
        ArtifactRef::new(
            artifact_id("manual-result-json-mismatch"),
            ArtifactKind::Json,
        ),
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
async fn blank_text_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-blank-text", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("manual-result-blank-text"), ArtifactKind::Text),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::text(" \n\t "))
        .await
        .expect_err("blank text tool result should be rejected before resolution");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Text
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("blank result artifact must not be recorded");
    assert!(matches!(
        evidence_err,
        merry_runtime::RuntimeError::Artifact {
            source: merry_runtime::ArtifactError::MissingArtifact { id }
        } if id == *result.artifact().id()
    ));

    let next_events = collect_step(&runtime, "after blank text submit").await;
    assert_eq!(event_kind_names(&next_events), ["StepStarted", "Failed"]);
    assert_eq!(failed_code(&next_events), Some("tool_call_result_required"));
    assert_eq!(
        next_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn blank_json_tool_result_keeps_call_pending_and_sequence_stable() {
    let provider = FakeModelProvider::new(vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(model_tool_call())],
        FinishReason::ToolCalls,
    ))]);
    let runtime = runtime_with_provider("provider-tool-result-blank-json", provider);
    let pending_events = collect_step(&runtime, "Request a tool.").await;
    let call = pending_tool_call(&pending_events).clone();
    let projection_before_error = runtime.ledger_projection().await;
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(artifact_id("manual-result-blank-json"), ArtifactKind::Json),
    );

    let err = runtime
        .submit_tool_result(result.clone(), ArtifactContent::json(" \n\t "))
        .await
        .expect_err("blank json tool result should be rejected before resolution");
    let projection_after_error = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            artifact_id,
            content_kind: merry_runtime::ArtifactContentKind::Json
        } if artifact_id == *result.artifact().id()
    ));
    assert_eq!(projection_before_error, projection_after_error);
    assert_eq!(runtime.pending_tool_calls().await, vec![call]);
    let evidence_err = runtime
        .evidence_ref(result.artifact().id(), EvidenceLocator::whole_artifact())
        .await
        .expect_err("blank result artifact must not be recorded");
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
        ArtifactRef::new(artifact_id("manual-result-binary"), ArtifactKind::Binary),
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
