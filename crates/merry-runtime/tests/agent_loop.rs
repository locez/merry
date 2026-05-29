use futures_util::stream;
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, PendingToolCall, ProviderName,
    RuntimeEvent, RuntimeEventKind, SessionId, ToolCallId, ToolCallResult, ToolCallResultStatus,
    ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelMessageRole,
    ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_runtime::{
    ActionExecutionEvidence, ActionProposal, ActionProposalEvidence, AgentLoopBlockedReason,
    AgentLoopConfig, AgentLoopConfigError, AgentLoopStatus, ArtifactError, ContextSummary,
    DEFAULT_AGENT_LOOP_CONTINUATION_INPUT, ProcessActionIntent, ProcessEnvPolicy,
    ProcessExitStatus, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerFuture, ProcessRunnerOutput, ProjectRules, Runtime, RuntimeError, StepContext,
    StepInput, TaskAnchor, ToolActionKind, ToolActionPreflight, ToolActionProposalFuture,
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
    process_command_tool,
};
use schemars::Schema;
use serde_json::{Map, Value, json};
use std::{
    future::Future,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_util::sync::CancellationToken;

fn trace_output_buffer() -> &'static Arc<Mutex<Vec<u8>>> {
    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("buffer mutex should not be poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    static TRACE_OUTPUT: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    TRACE_OUTPUT.get_or_init(|| {
        use tracing_subscriber::{fmt, prelude::*};

        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer_bytes = Arc::clone(&bytes);
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .json()
                .with_writer(move || Buffer(Arc::clone(&writer_bytes))),
        );
        tracing::subscriber::set_global_default(subscriber)
            .expect("test tracing subscriber should install once");
        bytes
    })
}

async fn capture_traces_for<F, R>(trace_marker: &str, future: F) -> (R, String)
where
    F: Future<Output = R>,
{
    let bytes = Arc::clone(trace_output_buffer());
    let start = bytes
        .lock()
        .expect("buffer mutex should not be poisoned")
        .len();
    let result = future.await;
    let text = {
        let guard = bytes.lock().expect("buffer mutex should not be poisoned");
        String::from_utf8(guard[start..].to_vec()).expect("trace output should be UTF-8")
    };
    let text = text
        .lines()
        .filter(|line| line.contains(trace_marker))
        .collect::<Vec<_>>()
        .join("\n");
    (result, text)
}

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

fn tool_call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("valid tool call id")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

fn tool_spec(name: &str) -> ToolSpec {
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

fn model_tool_call(id: &str, name: &str) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::new(Map::<String, Value>::new()),
    )
}

fn model_tool_call_with_arguments(id: &str, name: &str, arguments: Value) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::try_from(arguments).expect("valid model tool arguments"),
    )
}

fn completed_text_event(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

fn completed_tool_call_event(call: ModelToolCall) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    }
}

#[derive(Debug, Clone)]
struct ScriptedModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<Vec<ScriptedModelStep>>>,
    recorded_requests: Arc<Mutex<Vec<ModelRequest>>>,
}

type ScriptedModelStep = Vec<Result<ModelEvent, ModelError>>;

impl ScriptedModelProvider {
    fn new(steps: Vec<ScriptedModelStep>) -> Self {
        Self {
            name: ProviderName::new("agent-loop-scripted-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
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

            let script = self
                .steps
                .lock()
                .expect("steps mutex should not be poisoned")
                .pop()
                .unwrap_or_default();
            let event_stream: ModelEventStream = Box::pin(stream::iter(script));
            Ok(event_stream)
        })
    }
}

#[derive(Clone)]
struct BlockingModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    started_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    release_rx: Arc<AsyncMutex<Option<oneshot::Receiver<()>>>>,
}

impl BlockingModelProvider {
    fn new(started_tx: oneshot::Sender<()>, release_rx: oneshot::Receiver<()>) -> Self {
        Self {
            name: ProviderName::new("agent-loop-blocking-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            started_tx: Arc::new(Mutex::new(Some(started_tx))),
            release_rx: Arc::new(AsyncMutex::new(Some(release_rx))),
        }
    }
}

impl ModelProvider for BlockingModelProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            if let Some(started_tx) = self
                .started_tx
                .lock()
                .expect("started signal mutex should not be poisoned")
                .take()
            {
                let _ = started_tx.send(());
            }

            let release_rx = self
                .release_rx
                .lock()
                .await
                .take()
                .expect("blocking provider should only be used for one step");
            release_rx
                .await
                .expect("test should release the blocking provider");

            let event_stream: ModelEventStream =
                Box::pin(stream::iter([Ok(completed_text_event("released"))]));
            Ok(event_stream)
        })
    }
}

#[derive(Clone)]
struct ScriptedToolExecutor {
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    response: ToolExecutorResponse,
}

#[derive(Clone)]
enum ToolExecutorResponse {
    Outcome(ToolExecutionOutcome),
    InfrastructureError(String),
}

impl ScriptedToolExecutor {
    fn succeeding_text(text: &str) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response: ToolExecutorResponse::Outcome(ToolExecutionOutcome::succeeded_text(text)),
        }
    }

    fn infrastructure_error(message: &str) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response: ToolExecutorResponse::InfrastructureError(message.to_owned()),
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
                ToolExecutorResponse::InfrastructureError(message) => {
                    Err(ToolExecutionError::infrastructure(message.clone()))
                }
            }
        })
    }
}

#[derive(Clone)]
struct ProposingPatchToolExecutor {
    proposed_calls: Arc<Mutex<Vec<PendingToolCall>>>,
    executed_calls: Arc<Mutex<Vec<PendingToolCall>>>,
}

impl ProposingPatchToolExecutor {
    fn new() -> Self {
        Self {
            proposed_calls: Arc::new(Mutex::new(Vec::new())),
            executed_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn proposed_calls(&self) -> Vec<PendingToolCall> {
        self.proposed_calls
            .lock()
            .expect("proposed calls mutex should not be poisoned")
            .clone()
    }

    fn executed_calls(&self) -> Vec<PendingToolCall> {
        self.executed_calls
            .lock()
            .expect("executed calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for ProposingPatchToolExecutor {
    fn propose<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async move {
            self.proposed_calls
                .lock()
                .expect("proposed calls mutex should not be poisoned")
                .push(call.clone());

            let patch = WorkspacePatchProposal::new(
                "notes/proposed.txt",
                5,
                7,
                20,
                22,
                "fnv1a64:0000000000000010",
                "fnv1a64:0000000000000011",
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            let proposal = ActionProposal::new(
                &call,
                ToolActionKind::WorkspaceWrite,
                "workspace patch",
                "notes/proposed.txt",
                "Replace one matched preimage in notes/proposed.txt",
                ActionProposalEvidence::WorkspacePatch(patch),
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            Ok(ToolActionPreflight::Proposal(proposal))
        })
    }

    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.executed_calls
                .lock()
                .expect("executed calls mutex should not be poisoned")
                .push(call);
            let evidence = WorkspacePatchExecutionEvidence::new(
                "notes/proposed.txt",
                5,
                7,
                20,
                22,
                "fnv1a64:0000000000000010",
                "fnv1a64:0000000000000011",
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            Ok(ToolExecutionOutcome::succeeded_text("patch applied\n")
                .with_execution_evidence(ActionExecutionEvidence::WorkspacePatch(evidence)))
        })
    }
}

#[derive(Clone)]
struct BlockingToolExecutor {
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    started_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    release_rx: Arc<AsyncMutex<Option<oneshot::Receiver<()>>>>,
}

impl BlockingToolExecutor {
    fn new(started_tx: oneshot::Sender<()>, release_rx: oneshot::Receiver<()>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            started_tx: Arc::new(Mutex::new(Some(started_tx))),
            release_rx: Arc::new(AsyncMutex::new(Some(release_rx))),
        }
    }

    fn calls(&self) -> Vec<PendingToolCall> {
        self.calls
            .lock()
            .expect("tool calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for BlockingToolExecutor {
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

            if let Some(started_tx) = self
                .started_tx
                .lock()
                .expect("started signal mutex should not be poisoned")
                .take()
            {
                let _ = started_tx.send(());
            }

            let release_rx = self
                .release_rx
                .lock()
                .await
                .take()
                .expect("blocking executor should only be used once");
            release_rx
                .await
                .expect("test should release the blocking executor");

            Ok(ToolExecutionOutcome::succeeded_text("search result\n"))
        })
    }
}

#[derive(Clone)]
struct RecordingProcessRunner {
    observed_intents: Arc<Mutex<Vec<ProcessActionIntent>>>,
    stdout_text: String,
}

impl RecordingProcessRunner {
    fn succeeding(stdout_text: &str) -> Self {
        Self {
            observed_intents: Arc::new(Mutex::new(Vec::new())),
            stdout_text: stdout_text.to_owned(),
        }
    }

    fn observed_intents(&self) -> Vec<ProcessActionIntent> {
        self.observed_intents
            .lock()
            .expect("process intents mutex should not be poisoned")
            .clone()
    }
}

impl ProcessRunner for RecordingProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }

            self.observed_intents
                .lock()
                .expect("process intents mutex should not be poisoned")
                .push(intent.clone());

            ProcessRunnerOutput::new(
                &intent,
                ProcessExitStatus::Exited(0),
                self.stdout_text.clone(),
                false,
                "",
                false,
            )
            .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
        })
    }
}

fn runtime_with_provider(session: &str, provider: ScriptedModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn runtime_with_tool(
    session: &str,
    provider: ScriptedModelProvider,
    executor: impl ToolExecutor + 'static,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(executor),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn runtime_with_tool_action(
    session: &str,
    provider: ScriptedModelProvider,
    executor: impl ToolExecutor + 'static,
    action_kind: ToolActionKind,
) -> Runtime {
    Runtime::builder(session_id(session))
        .register_tool(merry_runtime::RegisteredTool::new(
            tool_spec("search_notes"),
            Arc::new(executor),
            action_kind,
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
}

fn assert_sanitized_policy_denial_json(value: &Value, tool_name: &str) {
    assert_eq!(
        value,
        &json!({
            "ok": false,
            "tool": tool_name,
            "error": {
                "code": "action_policy_denied",
                "message": "tool action was blocked by runtime policy"
            }
        })
    );
    assert!(value.get("call_id").is_none());
    assert!(value.get("action_kind").is_none());
    assert!(value.get("policy").is_none());
    assert!(value.get("reason").is_none());
}

async fn run_default_loop(runtime: &Runtime, text: &str) -> merry_runtime::AgentLoopResult {
    runtime
        .run_agent_loop(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .await
        .expect("agent loop should run")
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

fn continuation_input_for(original_task: &str) -> String {
    format!("{DEFAULT_AGENT_LOOP_CONTINUATION_INPUT}\n\nOriginal task:\n{original_task}")
}

fn assert_continuation_request_body(request: &ModelRequest, original_task: &str) {
    let dynamic = request.dynamic_messages();
    assert_eq!(
        dynamic
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        [ModelMessageRole::User, ModelMessageRole::User]
    );
    assert_eq!(dynamic[0].content().as_text(), original_task);
    assert_eq!(
        dynamic[1].content().as_text(),
        continuation_input_for(original_task)
    );
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
async fn agent_loop_executes_one_tool_and_continues_to_final_completion() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-success",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool("agent-loop-happy", provider.clone(), executor.clone());

    let result = run_default_loop(&runtime, "Search notes.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.steps_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted",
        ]
    );
    assert_eq!(
        result
            .events()
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5, 6, 7]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(executor.calls().len(), 1);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::System);
    assert!(
        requests[0].messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a pragmatic coding agent.")
    );
    assert_eq!(requests[0].messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        requests[0].messages()[1].content().as_text(),
        "Search notes."
    );
    assert!(requests[0].continuations().is_empty());
    assert_continuation_request_body(&requests[1], "Search notes.");
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-success"
    );
    assert_eq!(
        requests[1].continuations()[0].result().status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_preserves_uncheckpointed_tool_continuations_until_compaction() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-first",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-second",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final after two tools"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-uncheckpointed-continuity",
        provider.clone(),
        executor,
    );

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search twice, then answer.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(4).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.steps_run(), 3);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].continuations().is_empty());

    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-first"
    );

    assert_eq!(requests[2].continuations().len(), 2);
    assert_eq!(
        requests[2]
            .continuations()
            .iter()
            .map(|continuation| continuation.call().id().as_str())
            .collect::<Vec<_>>(),
        ["call-first", "call-second"]
    );
    assert!(
        requests[1].dynamic_context_hash() != requests[2].dynamic_context_hash(),
        "adding the second uncheckpointed continuation should change only dynamic request context"
    );
    assert_eq!(
        requests[1].stable_prefix_hash(),
        requests[2].stable_prefix_hash(),
        "uncheckpointed continuation growth must not move the cacheable stable prefix"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_keeps_uncheckpointed_continuations_after_final_answer() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-first",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("first final"))],
        vec![Ok(completed_text_event("second final"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-uncheckpointed-continuity-final",
        provider.clone(),
        executor,
    );

    let first = run_default_loop(&runtime, "Search once.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);

    let second = run_default_loop(&runtime, "Answer without compaction.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[2].continuations().len(),
        1,
        "terminal assistant completion is not compaction; old continuations remain uncheckpointed"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        "call-first"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_user_and_assistant_messages_remain_append_only_without_task_anchor() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("first final answer"))],
        vec![Ok(completed_text_event("second final answer"))],
    ]);
    let runtime = Runtime::builder(session_id("agent-loop-append-only-body"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, "First user task.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "Second user task.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].stable_prefix_message_count(), 1);
    assert_eq!(requests[1].stable_prefix_message_count(), 1);
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash(),
        "append-only body growth must not move the stable prefix"
    );
    assert_ne!(
        requests[0].dynamic_context_hash(),
        requests[1].dynamic_context_hash(),
        "append-only body growth should change only dynamic request context"
    );

    let dynamic = requests[1].dynamic_messages();
    assert_eq!(
        dynamic
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        [
            ModelMessageRole::User,
            ModelMessageRole::Assistant,
            ModelMessageRole::User
        ]
    );
    assert_eq!(dynamic[0].content().as_text(), "First user task.");
    assert_eq!(dynamic[1].content().as_text(), "first final answer");
    assert_eq!(dynamic[2].content().as_text(), "Second user task.");
}

#[tokio::test(flavor = "current_thread")]
async fn task_anchor_is_dynamic_control_segment_before_append_only_body() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("first final answer"))],
        vec![Ok(completed_text_event("second final answer"))],
    ]);
    let runtime = Runtime::builder(session_id("agent-loop-task-anchor"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .task_anchor(TaskAnchor::new("Fix the status text fixture.").expect("valid task anchor"))
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, "Start work.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "Continue.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].stable_prefix_message_count(),
        1,
        "task anchor is dynamic control context, not stable prefix"
    );
    assert_eq!(
        requests[0].stable_prefix_hash(),
        requests[1].stable_prefix_hash(),
        "append-only body growth must not move the stable prefix when task anchor is set"
    );

    let dynamic = requests[1].dynamic_messages();
    assert_eq!(
        dynamic
            .iter()
            .map(|message| message.role())
            .collect::<Vec<_>>(),
        [
            ModelMessageRole::System,
            ModelMessageRole::User,
            ModelMessageRole::Assistant,
            ModelMessageRole::User
        ]
    );
    assert_eq!(
        dynamic[0].content().as_text(),
        "task-anchor:\nFix the status text fixture."
    );
    assert_eq!(dynamic[1].content().as_text(), "Start work.");
    assert_eq!(dynamic[2].content().as_text(), "first final answer");
    assert_eq!(dynamic[3].content().as_text(), "Continue.");
}

#[tokio::test(flavor = "current_thread")]
async fn task_anchor_does_not_join_project_rules_stable_prefix() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("final answer"))]]);
    let runtime = Runtime::builder(session_id("agent-loop-task-anchor-project-rules"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .project_rules(ProjectRules::new("AGENTS.md", "Use project rules.").expect("valid rules"))
        .task_anchor(TaskAnchor::new("Keep this task pinned.").expect("valid task anchor"))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Work on the pinned task.").await;
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.stable_prefix_message_count(),
        2,
        "only base instructions and project rules belong to the stable prefix"
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("project-rules-source:AGENTS.md")
    );

    let dynamic = request.dynamic_messages();
    assert_eq!(dynamic[0].role(), ModelMessageRole::System);
    assert_eq!(
        dynamic[0].content().as_text(),
        "task-anchor:\nKeep this task pinned."
    );
    assert_eq!(dynamic[1].role(), ModelMessageRole::User);
    assert_eq!(dynamic[1].content().as_text(), "Work on the pinned task.");
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_control_prompt_is_not_recorded_as_user_history() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-success",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("first final answer"))],
        vec![Ok(completed_text_event("second final answer"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-control-prompt-not-history",
        provider.clone(),
        executor,
    );

    let first = run_default_loop(&runtime, "Search once.").await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "Second user task.").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 3);
    let final_request_text = requests[2]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(
        !final_request_text.contains(DEFAULT_AGENT_LOOP_CONTINUATION_INPUT),
        "agent-loop continuation control prompt must not be recorded as user history"
    );
    assert!(final_request_text.contains("Search once."));
    assert!(final_request_text.contains("first final answer"));
    assert!(final_request_text.contains("Second user task."));
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_executes_opt_in_workspace_patch_and_continues_to_final_completion() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-patch-success",
            "workspace_patch",
        )))],
        vec![Ok(completed_text_event("final after patch"))],
    ]);
    let executor = ProposingPatchToolExecutor::new();
    let runtime = Runtime::builder(session_id("agent-loop-opt-in-workspace-patch"))
        .register_tool(
            merry_runtime::RegisteredTool::new(
                tool_spec("workspace_patch"),
                Arc::new(executor.clone()),
                ToolActionKind::WorkspaceWrite,
            )
            .with_action_proposal(),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_low_risk_workspace_patches()
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Patch note.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.steps_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(executor.proposed_calls().len(), 1);
    assert_eq!(executor.executed_calls().len(), 1);

    let resolved = result
        .events()
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("patch tool call should resolve");
    assert_eq!(resolved.status(), ToolCallResultStatus::Succeeded);
    assert!(resolved.diagnostic().is_none());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_continuation_request_body(&requests[1], "Patch note.");
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation_result = requests[1].continuations()[0].result();
    assert_eq!(
        continuation_result.status(),
        ToolCallResultStatus::Succeeded
    );
    assert!(continuation_result.diagnostic().is_none());
    assert_eq!(
        continuation_result.content().as_text(),
        Some("patch applied\n")
    );
    assert!(
        !continuation_result
            .content()
            .as_str()
            .contains("action_policy_denied"),
        "successful opt-in patch continuation must not carry policy denial content"
    );
    for forbidden in [
        "proposal",
        "audit",
        "evidence",
        "fingerprint",
        "fnv1a64",
        "preimage_bytes",
        "replacement_bytes",
        "file_fingerprint_before",
        "file_fingerprint_after",
        "file_bytes_before",
        "file_bytes_after",
    ] {
        assert!(
            !continuation_result.content().as_str().contains(forbidden),
            "successful opt-in patch continuation leaked {forbidden}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_process_command_tool_executes_and_continues() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-rustc-version",
                "run_process",
                json!({ "argv": ["rustc", "--version"] }),
            ),
        ))],
        vec![Ok(completed_text_event("final after process"))],
    ]);
    let runner = RecordingProcessRunner::succeeding("rustc 1.85.0\n");
    let runtime = Runtime::builder(session_id("agent-loop-process-command-tool"))
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a local process from argv through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_low_risk_process_actions(Arc::new(runner.clone()))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Check rustc version.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.steps_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let observed_intents = runner.observed_intents();
    assert_eq!(observed_intents.len(), 1);
    let intent = &observed_intents[0];
    assert_eq!(intent.argv(), ["rustc", "--version"]);
    assert_eq!(intent.cwd(), None);
    assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);
    assert!(intent.stdin_text().is_none());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .tools()
            .iter()
            .any(|tool| tool.name().as_str() == "run_process"),
        "first model request should expose the registered process command tool"
    );
    assert!(requests[0].continuations().is_empty());
    assert_continuation_request_body(&requests[1], "Check rustc version.");
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-rustc-version");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert!(continuation.result().diagnostic().is_none());
    let content = continuation
        .result()
        .content()
        .as_json()
        .expect("process result should be JSON");
    let value: Value = serde_json::from_str(content).expect("process result JSON should parse");
    assert_eq!(value["ok"], true);
    assert_eq!(value["kind"], "process_action");
    assert_eq!(value["status"], json!({ "kind": "exited", "code": 0 }));
    assert_eq!(value["intent"]["argv"], json!(["rustc", "--version"]));
    assert_eq!(value["intent"]["cwd"], Value::Null);
    assert_eq!(value["stdout"]["text"], "rustc 1.85.0\n");
    assert_eq!(value["stderr"]["text"], "");
    for forbidden in ["proposal", "audit", "evidence"] {
        assert!(
            !content.contains(forbidden),
            "process continuation leaked internal {forbidden}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_process_command_invalid_arguments_resolve_failed_and_continue() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-bad-process-argv",
                "run_process",
                json!({ "argv": "cargo test -p merry-runtime" }),
            ),
        ))],
        vec![Ok(completed_text_event("final after bad process argv"))],
    ]);
    let runner = RecordingProcessRunner::succeeding("must not run\n");
    let runtime = Runtime::builder(session_id("agent-loop-process-command-invalid-args"))
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a local process from argv through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_low_risk_process_actions(Arc::new(runner.clone()))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Run process with malformed argv.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(runner.observed_intents(), Vec::<ProcessActionIntent>::new());
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-bad-process-argv");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("invalid argv should include diagnostic")
            .code(),
        "process_command_invalid_arguments"
    );
    let content = continuation
        .result()
        .content()
        .as_json()
        .expect("invalid argv result should be JSON");
    let value: Value = serde_json::from_str(content).expect("invalid argv JSON should parse");
    assert_eq!(
        value["error"]["message"],
        "argv must be an array of strings"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_traces_loop_steps_tool_process_and_terminal_status() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-rustc-version",
                "run_process",
                json!({ "argv": ["rustc", "--version"] }),
            ),
        ))],
        vec![Ok(completed_text_event("final after process"))],
    ]);
    let runner = RecordingProcessRunner::succeeding("rustc 1.85.0\n");
    let runtime = Runtime::builder(session_id("agent-loop-tracing"))
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a local process from argv through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .model_provider(Arc::new(provider), model_name())
        .allow_low_risk_process_actions(Arc::new(runner))
        .build()
        .expect("runtime should build");

    let (result, logs) = capture_traces_for(
        "agent-loop-tracing",
        runtime.run_agent_loop(
            StepInput::user_text("Check rustc version.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(8).expect("valid config"),
        ),
    )
    .await;

    let result = result.expect("agent loop should run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(logs.contains("\"event\":\"runtime.loop.start\""));
    assert!(logs.contains("\"event\":\"runtime.step.start\""));
    assert!(logs.contains("\"event\":\"runtime.provider.request\""));
    assert!(logs.contains("\"event\":\"runtime.tool.pending\""));
    assert!(logs.contains("\"event\":\"runtime.tool.execute.start\""));
    assert!(logs.contains("\"event\":\"runtime.process.execute.start\""));
    assert!(logs.contains("\"event\":\"runtime.process.execute.finish\""));
    assert!(logs.contains("\"event\":\"runtime.artifact.record\""));
    assert!(logs.contains("\"event\":\"runtime.tool.execute.finish\""));
    assert!(logs.contains("\"event\":\"runtime.loop.finish\""));
    assert!(logs.contains("\"status\":\"completed\""));
    assert!(logs.contains("\"tool_name\":\"run_process\""));
    assert!(logs.contains("\"tool_call_id\":\"call-rustc-version\""));
    assert!(logs.contains("\"permission_profile_id\":\"process.read_only.v1\""));
    assert!(logs.contains("\"argv\":\"[\\\"rustc\\\", \\\"--version\\\"]\""));
    assert!(logs.contains("\"stdout_bytes\":13"));
    assert!(logs.contains("\"stderr_bytes\":0"));
    assert!(!logs.contains("rustc 1.85.0"));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_trace_includes_checkpoint_budget_diagnostics_without_prompt_projection() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("final answer"))]]);
    let runtime = Runtime::builder(session_id("agent-loop-context-budget-trace"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .task_anchor(TaskAnchor::new("Keep budget diagnostics separate.").expect("valid anchor"))
        .build()
        .expect("runtime should build");

    let (result, logs) = capture_traces_for(
        "agent-loop-context-budget-trace",
        runtime.run_agent_loop(
            StepInput::user_text("Use a short request.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(2).expect("valid config"),
        ),
    )
    .await;

    let result = result.expect("agent loop should run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(logs.contains("\"event\":\"runtime.provider.request\""));
    assert!(logs.contains("\"context_window_source\":\"fallback\""));
    assert!(logs.contains("\"context_budget_policy\":\"balanced\""));
    assert!(logs.contains("\"checkpoint_decision\":\"continue\""));
    assert!(logs.contains("\"dynamic_body_estimated_tokens\":"));
    assert!(logs.contains("\"soft_water_tokens\":"));
    assert!(logs.contains("\"hard_water_tokens\":"));

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains("checkpoint_decision")),
        "checkpoint diagnostics must not be projected into prompt messages"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_still_runs_when_context_budget_diagnostic_is_unavailable() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("final answer"))]])
        .with_capabilities(
            ModelCapabilities::new(true, true, false, true, Some(100), Some(100))
                .expect("valid capabilities"),
        );
    let runtime = Runtime::builder(session_id("agent-loop-context-budget-unavailable"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let (result, logs) = capture_traces_for(
        "agent-loop-context-budget-unavailable",
        runtime.run_agent_loop(
            StepInput::user_text("Use a short request.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(2).expect("valid config"),
        ),
    )
    .await;

    let result = result.expect("agent loop should run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(logs.contains("\"event\":\"runtime.provider.request.context_budget_unavailable\""));
    assert!(logs.contains("\"event\":\"runtime.provider.request\""));

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].messages().iter().all(|message| {
            !message
                .content()
                .as_text()
                .contains("context_budget_unavailable")
        }),
        "budget diagnostics must remain trace-only"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_process_command_tool_executes_rg_files_and_continues() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-rg-files",
                "run_process",
                json!({ "argv": ["rg", "--files"] }),
            ),
        ))],
        vec![Ok(completed_text_event("final after rg files"))],
    ]);
    let runner =
        RecordingProcessRunner::succeeding("Cargo.toml\ncrates/merry-runtime/src/lib.rs\n");
    let runtime = Runtime::builder(session_id("agent-loop-process-command-tool-rg-files"))
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a local process from argv through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_low_risk_process_actions(Arc::new(runner.clone()))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "List tracked source files.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.steps_run(), 2);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let observed_intents = runner.observed_intents();
    assert_eq!(observed_intents.len(), 1);
    let intent = &observed_intents[0];
    assert_eq!(intent.argv(), ["rg", "--files"]);
    assert_eq!(intent.cwd(), None);
    assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);
    assert!(intent.stdin_text().is_none());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].continuations().is_empty());
    assert_continuation_request_body(&requests[1], "List tracked source files.");
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-rg-files");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert!(continuation.result().diagnostic().is_none());
    let content = continuation
        .result()
        .content()
        .as_json()
        .expect("process result should be JSON");
    let value: Value = serde_json::from_str(content).expect("process result JSON should parse");
    assert_eq!(value["ok"], true);
    assert_eq!(value["kind"], "process_action");
    assert_eq!(value["intent"]["argv"], json!(["rg", "--files"]));
    assert_eq!(
        value["stdout"]["text"],
        "Cargo.toml\ncrates/merry-runtime/src/lib.rs\n"
    );
    assert_eq!(value["stderr"]["text"], "");
    for forbidden in ["proposal", "audit", "evidence"] {
        assert!(
            !content.contains(forbidden),
            "process continuation leaked internal {forbidden}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_rejects_concurrent_pending_consumption_during_tool_execution() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-inter-operation",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let executor = BlockingToolExecutor::new(started_tx, release_rx);
    let runtime = runtime_with_tool(
        "agent-loop-inter-operation-admission",
        provider,
        executor.clone(),
    );
    let loop_runtime = runtime.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Search notes.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .await
            .expect("agent loop should complete")
    });

    started_rx
        .await
        .expect("executor should signal after tool execution starts");

    assert_eq!(
        runtime.pending_tool_calls().await,
        vec![PendingToolCall::new(
            merry_core::ToolCallId::new("call-inter-operation").expect("valid tool call id"),
            ToolName::new("search_notes").expect("valid tool name"),
            merry_core::ToolCallArguments::new(Map::new()),
        )]
    );
    assert_eq!(executor.calls().len(), 1);

    let shadow_result = ToolCallResult::succeeded(
        tool_call_id("call-inter-operation"),
        ArtifactRef::new(
            artifact_id("agent-loop-inter-operation-shadow-result"),
            ArtifactKind::Text,
        ),
    );
    let err = runtime
        .submit_tool_result(
            shadow_result,
            merry_runtime::ArtifactContent::text("should not consume pending\n"),
        )
        .await
        .expect_err("loop-level guard should reject concurrent pending consumption");
    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id("agent-loop-inter-operation-admission")
    ));

    release_tx
        .send(())
        .expect("blocking executor release receiver should still be active");
    let result = loop_handle
        .await
        .expect("agent loop task should not panic after rejected mutation");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(executor.calls().len(), 1);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_running_step_rejects_concurrent_context_summary_write() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingModelProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("agent-loop-context-provider-admission"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let loop_runtime = runtime.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Block inside provider.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .await
            .expect("agent loop should complete after provider release")
    });

    started_rx
        .await
        .expect("provider should signal after the loop step starts");

    let err = runtime
        .record_context_summary(
            ContextSummary::new(
                "blocked-summary",
                "Raw context write should wait.",
                Vec::new(),
            )
            .expect("summary construction allows compiler validation"),
        )
        .await
        .expect_err("active provider step should reject direct context summary writes");
    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id("agent-loop-context-provider-admission")
    ));

    release_tx
        .send(())
        .expect("blocking provider release receiver should still be active");
    let result = loop_handle
        .await
        .expect("agent loop task should not panic after release");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    runtime
        .record_context_summary(
            ContextSummary::new(
                "post-run-summary",
                "Raw context write may resume.",
                Vec::new(),
            )
            .expect("summary construction allows compiler validation"),
        )
        .await
        .expect("runtime should accept context summary after the loop completes");
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_tool_execution_rejects_concurrent_context_entry_write() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-context-entry-blocked",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let executor = BlockingToolExecutor::new(started_tx, release_rx);
    let runtime = runtime_with_tool("agent-loop-context-tool-admission", provider, executor);
    let loop_runtime = runtime.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Search notes.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .await
            .expect("agent loop should complete after tool release")
    });

    started_rx
        .await
        .expect("executor should signal after tool execution starts");

    let err = runtime
        .record_context_entry(merry_runtime::ContextEntry::summary(
            ContextSummary::new("blocked-entry", "Entry write should wait.", Vec::new())
                .expect("summary construction allows compiler validation"),
        ))
        .await
        .expect_err("active tool execution should reject direct context entry writes");
    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id("agent-loop-context-tool-admission")
    ));

    release_tx
        .send(())
        .expect("blocking executor release receiver should still be active");
    let result = loop_handle
        .await
        .expect("agent loop task should not panic after release");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn max_steps_blocks_before_infinite_tool_loop_and_leaves_pending() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-loop", "search_notes"),
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text("should not run\n");
    let runtime = runtime_with_tool("agent-loop-max-steps", provider, executor);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search forever.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(1).expect("valid non-zero budget"),
        )
        .await
        .expect("agent loop should return blocked status");

    assert_eq!(
        result.status(),
        &AgentLoopStatus::Blocked {
            reason: AgentLoopBlockedReason::MaxStepsReached { max_steps: 1 },
        }
    );
    assert_eq!(result.steps_run(), 1);
    assert_eq!(
        event_kind_names(result.events()),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        runtime.pending_tool_calls().await,
        vec![pending_tool_call(result.events()).clone()]
    );
}

#[test]
fn agent_loop_config_rejects_zero_max_steps() {
    let err = AgentLoopConfig::new(0).expect_err("zero budget should be rejected");

    assert_eq!(err, AgentLoopConfigError::MaxStepsMustBeNonZero);
}

#[tokio::test(flavor = "current_thread")]
async fn unregistered_tool_resolves_failed_and_continues_once() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-missing-tool",
            "missing_tool",
        )))],
        vec![Ok(completed_text_event("final after missing tool"))],
    ]);
    let runtime = runtime_with_provider("agent-loop-unregistered", provider.clone());

    let result = run_default_loop(&runtime, "Call missing tool.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted",
        ]
    );
    let resolved = result
        .events()
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool call should resolve");
    assert_eq!(resolved.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        resolved
            .diagnostic()
            .expect("unregistered tool result should have diagnostic")
            .code(),
        "tool_not_registered"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(provider.recorded_requests()[1].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn denied_registered_tool_resolves_failed_and_agent_loop_continues_once() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-policy-denied",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final after policy denial"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("executor must not run\n");
    let runtime = runtime_with_tool_action(
        "agent-loop-policy-denied",
        provider.clone(),
        executor.clone(),
        ToolActionKind::WorkspaceWrite,
    );

    let (result, logs) = capture_traces_for(
        "agent-loop-policy-denied",
        runtime.run_agent_loop(
            StepInput::user_text("Call denied tool.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        ),
    )
    .await;
    let result = result.expect("agent loop should complete after denied tool");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ArtifactRecorded",
            "StepCompleted",
        ]
    );
    assert_eq!(executor.calls().len(), 0);
    let resolved = result
        .events()
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool call should resolve");
    assert_eq!(resolved.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        resolved
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        "action_policy_denied"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].result().status(),
        ToolCallResultStatus::Failed
    );
    let content = requests[1].continuations()[0]
        .result()
        .content()
        .as_json()
        .expect("policy denial continuation should carry JSON content");
    let value: Value = serde_json::from_str(content).expect("denial JSON should parse");
    assert_sanitized_policy_denial_json(&value, "search_notes");
    assert_eq!(
        logs.matches("\"event\":\"runtime.tool.execute.finish\"")
            .count(),
        1
    );
    assert!(logs.contains("\"status\":\"denied\""));
    assert!(logs.contains("\"diagnostic_code\":\"action_policy_denied\""));
    assert!(!logs.contains("\"status\":\"failed\""));
}

#[tokio::test(flavor = "current_thread")]
async fn executor_infrastructure_error_preserves_events_and_pending_call() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-infra-error", "search_notes"),
    ))]]);
    let executor = ScriptedToolExecutor::infrastructure_error("temporary executor outage");
    let runtime = runtime_with_tool("agent-loop-infra-error", provider, executor);

    let (result, logs) = capture_traces_for(
        "agent-loop-infra-error",
        runtime.run_agent_loop(
            StepInput::user_text("Search notes.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        ),
    )
    .await;
    let err = result.expect_err("infrastructure error should stop the loop as a method error");

    assert_eq!(
        event_kind_names(err.events()),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = pending_tool_call(err.events()).clone();
    assert!(matches!(
        err.runtime_error(),
        RuntimeError::ToolExecutionFailed { call_id, .. } if call_id == pending.id()
    ));
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert!(logs.contains("\"event\":\"runtime.loop.finish\""));
    assert!(logs.contains("\"status\":\"error\""));
    assert!(logs.contains("\"diagnostic_code\":\"tool_execution_failed\""));
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_tool_execution_cancellation_returns_cancelled_and_keeps_pending() {
    let (started_tx, started_rx) = oneshot::channel();
    let (_release_tx, release_rx) = oneshot::channel();
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-loop-cancelled-tool",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("should not continue"))],
    ]);
    let executor = BlockingToolExecutor::new(started_tx, release_rx);
    let runtime = runtime_with_tool(
        "agent-loop-tool-cancellation",
        provider.clone(),
        executor.clone(),
    );
    let token = CancellationToken::new();
    let loop_runtime = runtime.clone();
    let loop_token = token.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Search notes.").expect("valid step input"),
                StepContext::new(loop_token),
                AgentLoopConfig::default(),
            )
            .await
            .expect("tool cancellation should return a loop status, not a method error")
    });

    started_rx
        .await
        .expect("executor should signal after tool execution starts");
    token.cancel();

    let result = loop_handle
        .await
        .expect("agent loop task should not panic after cancellation");

    assert!(matches!(
        result.status(),
        AgentLoopStatus::Cancelled {
            diagnostic
        } if diagnostic.code() == "tool_execution_cancelled"
            && diagnostic.message().contains("call-loop-cancelled-tool")
    ));
    assert_eq!(result.steps_run(), 1);
    assert_eq!(
        event_kind_names(result.events()),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = pending_tool_call(result.events()).clone();
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert_eq!(executor.calls().len(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);

    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("cancelled loop tool execution must not record a tool result artifact");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
    assert!(
        result
            .events()
            .iter()
            .all(|event| !matches!(event.kind, RuntimeEventKind::ToolCallResolved { .. }))
    );

    let artifact_events = runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("post-cancel-artifact"), ArtifactKind::Text),
            merry_runtime::ArtifactContent::text("runtime permit released\n"),
        )
        .await
        .expect("runtime should release active permit after loop cancellation");
    assert_eq!(event_kind_names(&artifact_events), ["ArtifactRecorded"]);
}

#[tokio::test(flavor = "current_thread")]
async fn no_provider_loop_completes_like_skeleton_step() {
    let runtime = Runtime::builder(session_id("agent-loop-no-provider"))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "No provider.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.steps_run(), 1);
    assert_eq!(
        event_kind_names(result.events()),
        ["SessionStarted", "StepStarted", "StepCompleted"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_loop_returns_cancelled_status() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        "should not be requested",
    ))]]);
    let runtime = runtime_with_provider("agent-loop-pre-cancelled", provider);
    let token = CancellationToken::new();
    token.cancel();

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Cancelled.").expect("valid step input"),
            StepContext::new(token),
            AgentLoopConfig::default(),
        )
        .await
        .expect("pre-cancelled loop should return cancelled status");

    assert!(matches!(
        result.status(),
        AgentLoopStatus::Cancelled {
            diagnostic
        } if diagnostic.code() == "cancelled"
    ));
    assert_eq!(event_kind_names(result.events()), ["Cancelled"]);
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_running_step_rejects_concurrent_runtime_mutation() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingModelProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("agent-loop-active-step-admission"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let loop_runtime = runtime.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Block inside provider.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .await
            .expect("agent loop should complete after provider release")
    });

    started_rx
        .await
        .expect("provider should signal after the loop step starts");

    let err = runtime
        .record_artifact(
            ArtifactRef::new(
                artifact_id("agent-loop-concurrent-artifact"),
                ArtifactKind::Text,
            ),
            merry_runtime::ArtifactContent::text("should not record\n"),
        )
        .await
        .expect_err("active loop step should reject concurrent mutation");
    assert!(matches!(
        err,
        RuntimeError::StepAlreadyActive {
            session_id: active_session
        } if active_session == session_id("agent-loop-active-step-admission")
    ));

    release_tx
        .send(())
        .expect("blocking provider release receiver should still be active");
    let result = loop_handle
        .await
        .expect("agent loop task should not panic after release");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    let events = runtime
        .record_artifact(
            ArtifactRef::new(
                artifact_id("agent-loop-post-run-artifact"),
                ArtifactKind::Text,
            ),
            merry_runtime::ArtifactContent::text("runtime usable after loop\n"),
        )
        .await
        .expect("runtime should accept mutation after the loop step completes");
    assert_eq!(event_kind_names(&events), ["ArtifactRecorded"]);
}
