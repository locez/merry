use futures_util::{StreamExt, stream};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, ModelUsage,
    PendingToolCall, ProviderName, RuntimeEvent, RuntimeJournalEvent, RuntimeJournalPayload,
    SessionId, ToolCallId, ToolCallResult, ToolCallResultStatus, ToolInputSchema, ToolName,
    ToolSpec,
};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelInputItem,
    ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
    ModelResponse, ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_runtime::{
    ActionExecutionEvidence, ActionProposal, ActionProposalEvidence, AgentLoopBlockedReason,
    AgentLoopConfig, AgentLoopConfigError, AgentLoopStatus, AgentLoopStreamMessage, ArtifactError,
    AutomaticCompactionConfig, CitationCompactionPolicy, ContextEvidence, ContextSummary,
    FINAL_OUTPUT_TOOL_NAME, FinalOutputContract, ProcessActionIntent, ProcessEnvPolicy,
    ProcessExitStatus, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerFuture, ProcessRunnerOutput, ProjectRules, Runtime, RuntimeError,
    RuntimeModelRole, StepContext, StepInput, TaskAnchor, ToolActionKind, ToolActionPreflight,
    ToolActionProposalFuture, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
    process_command_tool,
};
use schemars::Schema;
use serde_json::{Value, json};
use std::{
    future::Future,
    num::NonZeroUsize,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::{Barrier, Mutex as AsyncMutex, oneshot};
use tokio_util::sync::CancellationToken;

#[path = "agent_loop/diagnostics.rs"]
mod diagnostics;

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

static TRACE_CAPTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn capture_traces_for<F, R>(trace_marker: &str, future: F) -> (R, String)
where
    F: Future<Output = R>,
{
    let _capture_guard = TRACE_CAPTURE_LOCK.lock().await;
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

fn final_output_contract() -> FinalOutputContract {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Short final summary."
            }
        },
        "required": ["summary"],
        "additionalProperties": false
    }))
    .expect("test schema should be a JSON schema");

    FinalOutputContract::new(ToolInputSchema::new(schema).expect("valid final output schema"))
        .expect("valid final output contract")
}

fn model_tool_call(id: &str, name: &str) -> ModelToolCall {
    model_tool_call_with_arguments(id, name, json!({"query": "test query"}))
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

fn completed_text_event_with_usage(text: &str, usage: ModelUsage) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(text)],
            FinishReason::Stop,
            Some(usage),
        ),
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

fn completed_tool_call_batch_event(calls: Vec<ModelToolCall>) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            calls.into_iter().map(ModelOutput::tool_call).collect(),
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
    ScriptedOutcomes(Arc<Mutex<Vec<ToolExecutionOutcome>>>),
    InfrastructureError(String),
}

impl ScriptedToolExecutor {
    fn succeeding_text(text: &str) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response: ToolExecutorResponse::Outcome(ToolExecutionOutcome::succeeded_text(text)),
        }
    }

    fn succeeding_texts(texts: Vec<String>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            response: ToolExecutorResponse::ScriptedOutcomes(Arc::new(Mutex::new(
                texts
                    .into_iter()
                    .map(|text| ToolExecutionOutcome::succeeded_text(&text))
                    .rev()
                    .collect(),
            ))),
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
                ToolExecutorResponse::ScriptedOutcomes(outcomes) => outcomes
                    .lock()
                    .expect("scripted outcomes mutex should not be poisoned")
                    .pop()
                    .ok_or_else(|| ToolExecutionError::infrastructure("no scripted outcome")),
                ToolExecutorResponse::InfrastructureError(message) => {
                    Err(ToolExecutionError::infrastructure(message.clone()))
                }
            }
        })
    }
}

#[derive(Clone)]
struct BarrierToolExecutor {
    barrier: Arc<Barrier>,
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    markers: Arc<Mutex<Vec<String>>>,
}

impl BarrierToolExecutor {
    fn new(parties: usize) -> Self {
        Self::with_markers(parties, Arc::new(Mutex::new(Vec::new())))
    }

    fn with_markers(parties: usize, markers: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            barrier: Arc::new(Barrier::new(parties)),
            calls: Arc::new(Mutex::new(Vec::new())),
            markers,
        }
    }

    fn calls(&self) -> Vec<PendingToolCall> {
        self.calls
            .lock()
            .expect("tool calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for BarrierToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("tool calls mutex should not be poisoned")
                .push(call.clone());
            self.markers
                .lock()
                .expect("markers mutex should not be poisoned")
                .push(format!("start:{}", call.id()));
            self.barrier.wait().await;
            self.markers
                .lock()
                .expect("markers mutex should not be poisoned")
                .push(format!("end:{}", call.id()));
            Ok(ToolExecutionOutcome::succeeded_text(format!(
                "result for {}",
                call.id()
            )))
        })
    }
}

#[derive(Clone)]
struct MarkerToolExecutor {
    marker: &'static str,
    markers: Arc<Mutex<Vec<String>>>,
}

impl MarkerToolExecutor {
    fn new(marker: &'static str, markers: Arc<Mutex<Vec<String>>>) -> Self {
        Self { marker, markers }
    }
}

impl ToolExecutor for MarkerToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.markers
                .lock()
                .expect("markers mutex should not be poisoned")
                .push(self.marker.to_owned());
            Ok(ToolExecutionOutcome::succeeded_text(self.marker))
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

fn runtime_with_bridge_tool(session: &str, provider: ScriptedModelProvider) -> Runtime {
    Runtime::builder(session_id(session))
        .allow_bridge_tools()
        .register_tool(merry_runtime::RegisteredTool::bridge(tool_spec(
            "search_notes",
        )))
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

#[tokio::test]
async fn run_agent_loop_result_includes_session_usage_snapshot() {
    let usage = ModelUsage::with_details(21, Some(13), 8, Some(2), 29);
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event_with_usage(
        "usage final",
        usage,
    ))]]);
    let runtime = Runtime::builder(session_id("agent-loop-result-usage"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Report usage.").await;

    let result_usage = result
        .session_usage()
        .cloned()
        .expect("agent loop result should include usage");
    assert_eq!(result_usage.last, usage);
    assert_eq!(result_usage.total, usage);
    assert_eq!(runtime.usage().await, Some(result_usage));
}

#[tokio::test]
async fn run_agent_loop_stream_result_includes_session_usage_snapshot() {
    let usage = ModelUsage::with_details(31, Some(19), 11, None, 42);
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event_with_usage(
        "stream usage final",
        usage,
    ))]]);
    let runtime = Runtime::builder(session_id("agent-loop-stream-result-usage"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Report stream usage.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    let result = stream.result().await.expect("stream should produce result");

    let result_usage = result
        .session_usage()
        .cloned()
        .expect("stream result should include usage");
    assert_eq!(result_usage.last, usage);
    assert_eq!(result_usage.total, usage);
    assert_eq!(runtime.usage().await, Some(result_usage));
}

#[tokio::test]
async fn run_agent_loop_stream_yields_step_events_before_provider_finishes() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let runtime = Runtime::builder(session_id("agent-loop-live-stream"))
        .model_provider(
            Arc::new(BlockingModelProvider::new(started_tx, release_rx)),
            model_name(),
        )
        .build()
        .expect("runtime should build");

    let mut events = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Wait for the provider.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    started_rx
        .await
        .expect("provider should start and wait for release");
    let first = tokio::time::timeout(Duration::from_millis(100), events.next())
        .await
        .expect("stream should yield before provider finishes")
        .expect("session-start event should be present");
    let second = tokio::time::timeout(Duration::from_millis(100), events.next())
        .await
        .expect("stream should yield before provider finishes")
        .expect("step-start event should be present");

    assert_eq!(
        public_event_kind_names(&[first, second]),
        ["SessionStarted", "StepStarted"]
    );

    release_tx
        .send(())
        .expect("provider release receiver should still be waiting");
    let remaining = events.collect::<Vec<_>>().await;
    assert_eq!(
        public_event_kind_names(&remaining),
        ["AssistantMessage", "StepCompleted"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_agent_loop_stream_resumes_same_loop_after_bridge_tool_result() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-bridge",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let runtime = runtime_with_bridge_tool("agent-loop-stream-bridge-resume", provider);
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Search notes.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    let mut events = Vec::new();
    while let Some(message) = stream.next_driver_message().await {
        match message {
            AgentLoopStreamMessage::Event(event) => events.push(event),
            AgentLoopStreamMessage::BridgeToolRequest { call } => {
                assert_eq!(call.id().as_str(), "call-bridge");
                let artifact = ArtifactRef::new(
                    ArtifactId::new("sdk-bridge-result").expect("valid artifact id"),
                    ArtifactKind::Json,
                );
                stream
                    .submit_bridge_tool_result(
                        ToolCallResult::succeeded(tool_call_id("call-bridge"), artifact),
                        merry_runtime::ArtifactContent::json(r#"{"ok":true}"#),
                    )
                    .await
                    .expect("bridge result should submit to the active loop");
            }
            _ => {}
        }
    }

    let result = stream.result().await.expect("stream should produce result");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        public_event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallStarted",
            "ToolCallFinished",
            "StepStarted",
            "AssistantMessage",
            "StepCompleted",
        ]
    );
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "BridgeToolCallRequested",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_agent_loop_stream_resumes_after_multiple_bridge_results_in_model_order() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-bridge-1", "search_notes"),
            model_tool_call("call-bridge-2", "search_notes"),
        ]))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let runtime = runtime_with_bridge_tool("agent-loop-stream-bridge-batch", provider.clone());
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Search two sources.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    let mut requested = Vec::new();
    while let Some(message) = stream.next_driver_message().await {
        if let AgentLoopStreamMessage::BridgeToolRequest { call } = message {
            let call_id = call.id().clone();
            requested.push(call_id.as_str().to_owned());
            let artifact = ArtifactRef::new(
                ArtifactId::new(&format!("sdk-result-{}", call_id.as_str()))
                    .expect("valid artifact id"),
                ArtifactKind::Json,
            );
            stream
                .submit_bridge_tool_result(
                    ToolCallResult::succeeded(call_id, artifact),
                    merry_runtime::ArtifactContent::json(r#"{"ok":true}"#),
                )
                .await
                .expect("bridge result should submit to the active loop");
        }
    }

    let result = stream.result().await.expect("stream should produce result");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(requested, ["call-bridge-1", "call-bridge-2"]);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].batch_continuations()[0]
            .results()
            .iter()
            .map(|result| result.call_id().as_str())
            .collect::<Vec<_>>(),
        ["call-bridge-1", "call-bridge-2"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_agent_loop_stream_completes_final_output_without_continuation_budget() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call_with_arguments(
            "call-final",
            FINAL_OUTPUT_TOOL_NAME,
            json!({"summary": "Order A123 shipped."}),
        ),
    ))]]);
    let runtime = runtime_with_provider("agent-loop-stream-final-output-budget", provider);
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(1)
                .expect("valid non-zero budget")
                .with_final_output_contract(final_output_contract()),
        )
        .expect("agent loop stream should start");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    let result = stream.result().await.expect("stream should produce result");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        result
            .final_output_json()
            .expect("structured final output should be recorded")
            .json(),
        r#"{"summary":"Order A123 shipped."}"#
    );
    assert_eq!(
        public_event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallStarted",
            "FinalOutputRecorded",
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_completes_when_model_calls_final_output_tool() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call_with_arguments(
            "call-final",
            FINAL_OUTPUT_TOOL_NAME,
            json!({"summary": "Order A123 shipped."}),
        ),
    ))]]);
    let runtime = runtime_with_provider("agent-loop-final-output", provider);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        result
            .final_output_json()
            .expect("structured final output should be recorded")
            .json(),
        r#"{"summary":"Order A123 shipped."}"#
    );
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "FinalOutputRecorded",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn final_output_tool_call_is_not_replayed_into_next_step_transcript() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-final",
                FINAL_OUTPUT_TOOL_NAME,
                json!({"summary": "Order A123 shipped."}),
            ),
        ))],
        vec![Ok(completed_text_event("next answer"))],
    ]);
    let runtime = runtime_with_provider("agent-loop-final-output-then-next-step", provider.clone());

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let next_step = runtime
        .step(
            StepInput::user_text("Handle the next request.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("next step should start");
    let next_events = next_step.collect::<Vec<_>>().await;

    assert_eq!(
        event_kind_names(&next_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .input()
            .iter()
            .all(|item| matches!(item, ModelInputItem::Message(_))),
        "runtime final-output tool calls must not be replayed into later provider input"
    );
    assert!(requests[1].continuations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_final_output_tool_arguments_retry_as_failed_tool_result() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments("call-final-invalid", FINAL_OUTPUT_TOOL_NAME, json!({})),
        ))],
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-final-valid",
                FINAL_OUTPUT_TOOL_NAME,
                json!({"summary": "Order A123 shipped."}),
            ),
        ))],
    ]);
    let runtime = runtime_with_provider("agent-loop-final-output-invalid-retry", provider.clone());

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        result
            .final_output_json()
            .expect("structured final output should be recorded")
            .json(),
        r#"{"summary":"Order A123 shipped."}"#
    );
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "FinalOutputRecorded",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-final-invalid");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("schema failure should carry diagnostic")
            .code(),
        "tool_input_schema_invalid"
    );
    assert!(
        continuation
            .result()
            .content()
            .as_str()
            .contains("tool_input_schema_invalid")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_executes_runtime_tool_before_final_output_tool() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-search",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-final",
                FINAL_OUTPUT_TOOL_NAME,
                json!({"summary": "Order A123 shipped."}),
            ),
        ))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool("agent-loop-tool-then-final-output", provider, executor);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search notes and return structured status.")
                .expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        result
            .final_output_json()
            .expect("structured final output should be recorded")
            .json(),
        r#"{"summary":"Order A123 shipped."}"#
    );
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "FinalOutputRecorded",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_rejects_final_output_mixed_with_other_tool_calls_before_execution() {
    let provider =
        ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-search", "search_notes"),
            model_tool_call_with_arguments(
                "call-final",
                FINAL_OUTPUT_TOOL_NAME,
                json!({"summary": "Order A123 shipped."}),
            ),
        ]))]]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-mixed-final-output-batch",
        provider,
        executor.clone(),
    );

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search notes and return structured status.")
                .expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("protocol failure should be returned as loop status");

    assert_eq!(
        result.status(),
        &AgentLoopStatus::Failed {
            diagnostic: merry_core::ErrorInfo::new(
                "final_output_tool_batch_mixed",
                "final-output tool calls must be the only call in their model batch",
            )
            .expect("valid diagnostic"),
        }
    );
    assert!(executor.calls().is_empty());
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_blocks_text_completion_when_final_output_contract_is_active() {
    let provider =
        ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("Order A123 shipped."))]]);
    let runtime = runtime_with_provider("agent-loop-final-output-text-blocked", provider);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(
        result.status(),
        &AgentLoopStatus::Blocked {
            reason: AgentLoopBlockedReason::FinalOutputToolNotCalled,
        }
    );
    assert!(result.final_output().is_none());
    assert!(result.final_output_json().is_none());
}

fn event_kind_names(events: &[RuntimeJournalEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event.payload {
            RuntimeJournalPayload::SessionStarted => "SessionStarted",
            RuntimeJournalPayload::StepStarted => "StepStarted",
            RuntimeJournalPayload::CompactionStarted => "CompactionStarted",
            RuntimeJournalPayload::CompactionCompleted { .. } => "CompactionCompleted",
            RuntimeJournalPayload::SessionUsageUpdated { .. } => "SessionUsageUpdated",
            RuntimeJournalPayload::StepCompleted => "StepCompleted",
            RuntimeJournalPayload::Cancelled { .. } => "Cancelled",
            RuntimeJournalPayload::Failed { .. } => "Failed",
            RuntimeJournalPayload::ArtifactRecorded { .. } => "ArtifactRecorded",
            RuntimeJournalPayload::AssistantOutputDelta { .. } => "AssistantOutputDelta",
            RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
            RuntimeJournalPayload::EvidenceReferenced { .. } => "EvidenceReferenced",
            RuntimeJournalPayload::ToolCallPending { .. } => "ToolCallPending",
            RuntimeJournalPayload::ToolCallBatchPending { .. } => "ToolCallBatchPending",
            RuntimeJournalPayload::BridgeToolCallRequested { .. } => "BridgeToolCallRequested",
            RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
            RuntimeJournalPayload::FinalOutputRecorded { .. } => "FinalOutputRecorded",
            RuntimeJournalPayload::SkillUsed { .. } => "SkillUsed",
            _ => "Unknown",
        })
        .collect()
}

fn public_event_kind_names(events: &[RuntimeEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            RuntimeEvent::SessionStarted { .. } => "SessionStarted",
            RuntimeEvent::StepStarted { .. } => "StepStarted",
            RuntimeEvent::StepCompleted { .. } => "StepCompleted",
            RuntimeEvent::CompactionStarted { .. } => "CompactionStarted",
            RuntimeEvent::CompactionCompleted { .. } => "CompactionCompleted",
            RuntimeEvent::AssistantMessage { .. } => "AssistantMessage",
            RuntimeEvent::ToolCallStarted { .. } => "ToolCallStarted",
            RuntimeEvent::ToolCallBatchStarted { .. } => "ToolCallBatchStarted",
            RuntimeEvent::ToolCallFinished { .. } => "ToolCallFinished",
            RuntimeEvent::FinalOutputRecorded { .. } => "FinalOutputRecorded",
            RuntimeEvent::ModelRetryAttemptStarted { .. } => "ModelRetryAttemptStarted",
            RuntimeEvent::ModelRetryScheduled { .. } => "ModelRetryScheduled",
            RuntimeEvent::ModelRetryExhausted { .. } => "ModelRetryExhausted",
            RuntimeEvent::EvidenceReferenced { .. } => "EvidenceReferenced",
            RuntimeEvent::SkillUsed { .. } => "SkillUsed",
            RuntimeEvent::SubagentSpawned { .. } => "SubagentSpawned",
            RuntimeEvent::SubagentStarted { .. } => "SubagentStarted",
            RuntimeEvent::SubagentStatusChanged { .. } => "SubagentStatusChanged",
            RuntimeEvent::SubagentCompleted { .. } => "SubagentCompleted",
            RuntimeEvent::SubagentFailed { .. } => "SubagentFailed",
            RuntimeEvent::SubagentCancelled { .. } => "SubagentCancelled",
            RuntimeEvent::RunFailed { .. } => "RunFailed",
            RuntimeEvent::RunCancelled { .. } => "RunCancelled",
            RuntimeEvent::InteractiveRunStateChanged { .. } => "InteractiveRunStateChanged",
            RuntimeEvent::QueuedInputAccepted { .. } => "QueuedInputAccepted",
            RuntimeEvent::QueuedInputsChanged { .. } => "QueuedInputsChanged",
            RuntimeEvent::Closed => "Closed",
            _ => "Unknown",
        })
        .collect()
}

fn assert_continuation_request_body(request: &ModelRequest, original_task: &str) {
    let dynamic_text = request
        .dynamic_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n---\n");

    assert!(dynamic_text.contains(original_task));
    assert!(
        !dynamic_text.contains("Continue after tool result."),
        "agent-loop continuation must not inject a synthetic user prompt"
    );
    assert!(
        !dynamic_text.contains("Original task:"),
        "agent-loop continuation must not inject the original task label"
    );
}

fn pending_tool_call(events: &[RuntimeJournalEvent]) -> &PendingToolCall {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => Some(call),
            _ => None,
        })
        .expect("pending tool call should be emitted")
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_blocks_for_bridge_tool_runner_instead_of_executing_runtime_tool() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-bridge", "search_notes"),
    ))]]);
    let runtime = runtime_with_bridge_tool("agent-loop-bridge-blocks", provider);

    let result = run_default_loop(&runtime, "Search notes.").await;

    assert_eq!(
        result.status(),
        &AgentLoopStatus::Blocked {
            reason: AgentLoopBlockedReason::BridgeToolCallRequested {
                call_id: tool_call_id("call-bridge"),
                tool_name: ToolName::new("search_notes").expect("valid tool name"),
            },
        }
    );
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "BridgeToolCallRequested",
        ]
    );
    assert_eq!(
        runtime.pending_tool_calls().await,
        vec![pending_tool_call(result.events()).clone()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_continues_after_invalid_bridge_tool_arguments_are_resolved() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments("call-invalid-bridge", "search_notes", json!({})),
        ))],
        vec![Ok(completed_text_event("final after invalid bridge args"))],
    ]);
    let runtime = runtime_with_bridge_tool("agent-loop-invalid-bridge-continues", provider.clone());

    let result = run_default_loop(&runtime, "Search notes.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-invalid-bridge");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("schema failure should carry diagnostic")
            .code(),
        "tool_input_schema_invalid"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_stream_continues_after_invalid_bridge_tool_arguments_are_resolved() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments("call-invalid-bridge", "search_notes", json!({})),
        ))],
        vec![Ok(completed_text_event("final after invalid bridge args"))],
    ]);
    let runtime = runtime_with_bridge_tool(
        "agent-loop-stream-invalid-bridge-continues",
        provider.clone(),
    );
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Search notes.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    let result = stream.result().await.expect("stream should produce result");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        public_event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallStarted",
            "ToolCallFinished",
            "StepStarted",
            "AssistantMessage",
            "StepCompleted",
        ]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].result().status(),
        ToolCallResultStatus::Failed
    );
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
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
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
async fn agent_loop_executes_parallel_safe_batch_and_continues_in_model_order() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-1", "search_notes"),
            model_tool_call("call-2", "search_notes"),
        ]))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let executor = BarrierToolExecutor::new(2);
    let runtime = Runtime::builder(session_id("agent-loop-parallel-safe-batch"))
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("search_notes"),
                Arc::new(executor.clone()),
            )
            .with_parallel_safe_execution(),
        )
        .max_parallel_tool_calls(NonZeroUsize::new(2).expect("non-zero limit"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run_default_loop(&runtime, "Search notes twice."),
    )
    .await
    .expect("parallel-safe calls should reach the barrier together");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallBatchPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(executor.calls().len(), 2);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].batch_continuations().len(), 1);
    assert_eq!(
        requests[1].batch_continuations()[0]
            .results()
            .iter()
            .map(|result| result.call_id().as_str())
            .collect::<Vec<_>>(),
        ["call-1", "call-2"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_treats_exclusive_tool_as_barrier_between_parallel_waves() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-1", "parallel_before"),
            model_tool_call("call-2", "parallel_before"),
            model_tool_call("call-3", "exclusive"),
            model_tool_call("call-4", "parallel_after"),
        ]))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let markers = Arc::new(Mutex::new(Vec::new()));
    let before = BarrierToolExecutor::with_markers(2, Arc::clone(&markers));
    let runtime = Runtime::builder(session_id("agent-loop-exclusive-batch-barrier"))
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("parallel_before"),
                Arc::new(before),
            )
            .with_parallel_safe_execution(),
        )
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("exclusive"),
            Arc::new(MarkerToolExecutor::new("exclusive", Arc::clone(&markers))),
        ))
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("parallel_after"),
                Arc::new(MarkerToolExecutor::new("after", Arc::clone(&markers))),
            )
            .with_parallel_safe_execution(),
        )
        .max_parallel_tool_calls(NonZeroUsize::new(2).expect("non-zero limit"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run_default_loop(&runtime, "Run the mixed batch."),
    )
    .await
    .expect("parallel wave should complete before the exclusive barrier");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    let markers = markers
        .lock()
        .expect("markers mutex should not be poisoned")
        .clone();
    let exclusive_index = markers
        .iter()
        .position(|marker| marker == "exclusive")
        .expect("exclusive marker should be present");
    let after_index = markers
        .iter()
        .position(|marker| marker == "after")
        .expect("after marker should be present");
    assert_eq!(
        markers[..exclusive_index]
            .iter()
            .filter(|marker| marker.starts_with("end:"))
            .count(),
        2
    );
    assert!(after_index > exclusive_index);
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_preserves_transcript_tool_exchanges_until_compaction() {
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
        "agent-loop-tool-exchange-continuity",
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
    assert_eq!(result.model_turns_run(), 3);
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
        "adding the second tool exchange should change only dynamic request context"
    );
    assert_eq!(
        requests[1].stable_prefix_hash(),
        requests[2].stable_prefix_hash(),
        "tool exchange growth must not move the cacheable stable prefix"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_keeps_tool_exchanges_after_final_answer_until_compaction() {
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
        "agent-loop-tool-exchange-continuity-final",
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
        "terminal assistant completion is not compaction; old tool exchanges remain raw"
    );
    assert_eq!(
        requests[2].continuations()[0].call().id().as_str(),
        "call-first"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn compaction_removes_only_covered_tool_exchanges_after_successful_install() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-old",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("tail assistant"))],
        vec![Ok(completed_text_event(
            r#"{
              "claims": [
                {
                  "id": "c1",
                  "kind": "completed_action",
                  "text": "The old tool result was compacted.",
                  "refs": ["r1", "r2"]
                }
              ],
              "working_intent": null
            }"#,
        ))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let runtime = runtime_with_tool(
        "agent-loop-compaction-removes-continuations",
        provider.clone(),
        ScriptedToolExecutor::succeeding_text("old search result"),
    );

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Use the tool once.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(2).expect("valid config"),
        )
        .await
        .expect("loop runs");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);

    runtime
        .compact_context_once(
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
            StepContext::default(),
        )
        .await
        .expect("compaction succeeds")
        .expect("compaction runs");

    let stream = runtime
        .step(
            StepInput::user_text("Continue after compaction.").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts");
    let _events: Vec<RuntimeJournalEvent> = stream.collect().await;

    let requests = provider.recorded_requests();
    let final_request = requests.last().expect("final request exists");
    assert!(
        final_request.continuations().is_empty(),
        "covered tool exchanges should be removed only after successful compaction"
    );
    let final_text = final_request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(final_text.contains("The old tool result was compacted."));
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_user_and_assistant_messages_remain_ordered_without_task_anchor() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("first final answer"))],
        vec![Ok(completed_text_event("second final answer"))],
    ]);
    let runtime = Runtime::builder(session_id("agent-loop-transcript-body"))
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
        "transcript body growth must not move the stable prefix"
    );
    assert_ne!(
        requests[0].dynamic_context_hash(),
        requests[1].dynamic_context_hash(),
        "transcript growth should change only dynamic request context"
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
async fn task_anchor_is_dynamic_control_segment_before_transcript_body() {
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
        "transcript body growth must not move the stable prefix when task anchor is set"
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
        !final_request_text.contains("Continue after tool result."),
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
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(executor.proposed_calls().len(), 1);
    assert_eq!(executor.executed_calls().len(), 1);

    let resolved = result
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
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
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
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
async fn provider_step_auto_compacts_before_hard_watermark_request() {
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old assistant sentinel"))],
        vec![Ok(completed_text_event("tail assistant sentinel"))],
        vec![Ok(completed_text_event("final after automatic compaction"))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(4_000), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "Old turn was compacted automatically.",
              "refs": ["r1", "r2"]
            }
          ],
          "working_intent": null
        }"#,
    ))]]);
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-hard-watermark"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, &"old user sentinel ".repeat(450)).await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "tail user sentinel").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);
    let third = run_default_loop(&runtime, "current user sentinel").await;
    assert_eq!(third.status(), &AgentLoopStatus::Completed);

    assert_eq!(
        compactor.recorded_requests().len(),
        1,
        "runtime should compact before sending the hard-watermark request"
    );
    let compaction_request_text = compactor.recorded_requests()[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compaction_request_text.contains("old user sentinel"));
    assert!(!compaction_request_text.contains("tail user sentinel"));
    assert!(!compaction_request_text.contains("current user sentinel"));

    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 3);
    let final_request = primary_requests.last().expect("final request exists");
    let final_text = final_request
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(final_text.contains("compacted-checkpoint:"));
    assert!(final_text.contains("Old turn was compacted automatically."));
    assert!(final_text.contains("tail user sentinel"));
    assert!(final_text.contains("tail assistant sentinel"));
    assert!(final_text.contains("current user sentinel"));
    assert!(
        !final_text.contains("old user sentinel"),
        "covered raw history should be replaced by checkpoint projection"
    );
    assert!(
        !final_text.contains("old assistant sentinel"),
        "covered assistant history should be replaced by checkpoint projection"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn auto_compaction_config_controls_retained_raw_tail() {
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old assistant configurable tail"))],
        vec![Ok(completed_text_event("tail one assistant"))],
        vec![Ok(completed_text_event("tail two assistant"))],
        vec![Ok(completed_text_event(
            "final after configurable automatic compaction",
        ))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(4_000), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "Only the old configurable-tail turn was compacted.",
              "refs": ["r1", "r2"]
            }
          ],
          "working_intent": null
        }"#,
    ))]]);
    let policy = CitationCompactionPolicy::new(192, None, 8192, 4, 1200, 16).expect("valid policy");
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-config-tail"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(policy))
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, &"old configurable tail user ".repeat(450)).await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "tail one user").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);
    let third = run_default_loop(&runtime, "tail two user").await;
    assert_eq!(third.status(), &AgentLoopStatus::Completed);
    let fourth = run_default_loop(&runtime, "current configurable tail user").await;
    assert_eq!(fourth.status(), &AgentLoopStatus::Completed);

    assert_eq!(compactor.recorded_requests().len(), 1);
    let compaction_request_text = compactor.recorded_requests()[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(compaction_request_text.contains("old configurable tail user"));
    assert!(!compaction_request_text.contains("tail one user"));
    assert!(!compaction_request_text.contains("tail two user"));
    assert!(!compaction_request_text.contains("current configurable tail user"));

    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 4);
    let final_text = primary_requests
        .last()
        .expect("final request exists")
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(final_text.contains("Only the old configurable-tail turn was compacted."));
    assert!(final_text.contains("tail one user"));
    assert!(final_text.contains("tail one assistant"));
    assert!(final_text.contains("tail two user"));
    assert!(final_text.contains("tail two assistant"));
    assert!(final_text.contains("current configurable tail user"));
    assert!(!final_text.contains("old configurable tail user"));
    assert!(!final_text.contains("old assistant configurable tail"));
}

#[tokio::test(flavor = "current_thread")]
async fn auto_compacted_agent_loop_continuation_preserves_original_task_text() {
    let original_task = format!(
        "original long task sentinel {}",
        "keep-this-exact-task ".repeat(80)
    );
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-auto-compact-continuation",
            "search_notes",
        )))],
        vec![Ok(completed_text_event(
            "final after compacted continuation",
        ))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(520), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "current_state",
              "text": "The opening task turn was compacted before tool continuation.",
              "refs": ["r1"]
            }
          ],
          "working_intent": null
        }"#,
    ))]]);
    let policy = CitationCompactionPolicy::new(192, None, 8192, 1, 1200, 16).expect("valid policy");
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-keeps-original-task"))
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_text(
                "tool result sentinel\n",
            )),
        ))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(policy))
        .build()
        .expect("runtime should build");

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&original_task).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(2).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        compactor.recorded_requests().len(),
        1,
        "continuation request should compact the covered opening task turn"
    );

    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 2);
    let continuation_request_text = primary_requests[1]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(continuation_request_text.contains("compacted-checkpoint:"));
    assert!(
        continuation_request_text
            .contains("The opening task turn was compacted before tool continuation.")
    );
    assert!(!continuation_request_text.contains("Continue after tool result."));
    assert!(!continuation_request_text.contains("Original task:"));
    assert!(
        !continuation_request_text.contains(&original_task),
        "covered task text should move behind checkpoint refs instead of remaining raw"
    );

    let ref_excerpt = runtime
        .read_checkpoint_ref(
            &merry_runtime::CheckpointId::new(
                "checkpoint-agent-loop-auto-compaction-keeps-original-task-3",
            )
            .expect("valid checkpoint id"),
            &merry_runtime::CheckpointRefId::new("r1").expect("valid ref id"),
        )
        .await
        .expect("checkpoint ref resolves");
    assert!(
        ref_excerpt
            .excerpt()
            .contains("original long task sentinel")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn auto_compacted_agent_loop_continuation_keeps_checkpoint_refs_and_stable_prefix() {
    let original_task = format!(
        "long coding loop task sentinel {}",
        "inspect-read-patch-verify ".repeat(80)
    );
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-covered-search",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-retained-search",
            "search_notes",
        )))],
        vec![Ok(completed_text_event(
            "final after checkpointed continuation",
        ))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(520), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event(
            r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "The covered opening task was checkpointed.",
              "refs": ["r1"]
            }
          ],
          "working_intent": {
            "text": "Continue the original coding-loop task from the raw continuation request.",
            "refs": ["r1"],
            "confidence": 0.77
          }
        }"#,
        ))],
        vec![Ok(completed_text_event(
            r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "The prior checkpoint and first covered tool result were checkpointed.",
              "refs": ["prior-c1", "r1"]
            }
          ],
          "working_intent": null
        }"#,
        ))],
    ]);
    let policy = CitationCompactionPolicy::new(192, None, 8192, 1, 1200, 16).expect("valid policy");
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-checkpoint-refs"))
        .project_rules(
            ProjectRules::new("AGENTS.md", "Stable prefix rules sentinel.")
                .expect("valid project rules"),
        )
        .task_anchor(
            TaskAnchor::new("Complete the disposable coding-loop fixture.")
                .expect("valid task anchor"),
        )
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(ScriptedToolExecutor::succeeding_texts(vec![
                format!(
                    "covered tool result sentinel {}\n",
                    "covered-result-evidence ".repeat(90)
                ),
                "retained tool result sentinel\n".to_owned(),
            ])),
        ))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::enabled(policy))
        .build()
        .expect("runtime should build");

    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&original_task).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(3).expect("valid loop config"),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 3);
    assert_eq!(compactor.recorded_requests().len(), 2);

    let compactor_requests = compactor.recorded_requests();
    let first_compaction_request_text = compactor_requests[0]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(first_compaction_request_text.contains("long coding loop task sentinel"));
    assert!(!first_compaction_request_text.contains("covered tool result sentinel"));
    assert!(!first_compaction_request_text.contains("Continue after tool result."));

    let second_compaction_request_text = compactor_requests[1]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second_compaction_request_text.contains("The covered opening task was checkpointed."));
    assert!(second_compaction_request_text.contains("covered tool result sentinel"));
    assert!(!second_compaction_request_text.contains("retained tool result sentinel"));
    assert!(!second_compaction_request_text.contains("Continue after tool result."));

    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 3);
    let opening_request = &primary_requests[0];
    let first_continuation_request = &primary_requests[1];
    let final_continuation_request = &primary_requests[2];
    assert_eq!(
        opening_request.stable_prefix_hash(),
        final_continuation_request.stable_prefix_hash(),
        "auto-installed checkpoints and continuations must not move stable prefix"
    );
    assert!(
        first_continuation_request.dynamic_context_hash()
            != final_continuation_request.dynamic_context_hash(),
        "checkpoint projection and continuation should change dynamic context"
    );
    assert!(
        final_continuation_request
            .continuations()
            .iter()
            .all(|continuation| continuation.call().id().as_str() != "call-covered-search"),
        "covered tool continuation should be removed after successful auto compaction"
    );
    assert_eq!(
        final_continuation_request.continuations().len(),
        1,
        "latest retained tool continuation should remain raw after compaction"
    );
    assert_eq!(
        final_continuation_request.continuations()[0]
            .call()
            .id()
            .as_str(),
        "call-retained-search"
    );

    let stable_text = final_continuation_request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(stable_text.contains("Stable prefix rules sentinel."));
    assert!(!stable_text.contains("compacted-checkpoint:"));

    let dynamic_text = final_continuation_request
        .dynamic_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(dynamic_text.contains("task-anchor:"));
    assert!(dynamic_text.contains("compacted-checkpoint:"));
    assert!(
        dynamic_text
            .contains("The prior checkpoint and first covered tool result were checkpointed.")
    );
    assert!(!dynamic_text.contains("The covered opening task was checkpointed."));
    assert!(!dynamic_text.contains("Continue after tool result."));
    assert!(!dynamic_text.contains("Original task:"));
    assert!(
        !dynamic_text.contains(&original_task),
        "covered task text should stay behind checkpoint refs after rolling compaction"
    );
    assert!(!dynamic_text.contains("covered tool result sentinel"));

    let ref_excerpt = runtime
        .read_checkpoint_ref(
            &merry_runtime::CheckpointId::new(
                "checkpoint-agent-loop-auto-compaction-checkpoint-refs-5",
            )
            .expect("valid checkpoint id"),
            &merry_runtime::CheckpointRefId::new("r1").expect("valid ref id"),
        )
        .await
        .expect("checkpoint ref resolves");
    assert!(
        ref_excerpt
            .excerpt()
            .contains("covered tool result sentinel")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn auto_compaction_config_can_disable_hard_watermark_compaction() {
    let primary = ScriptedModelProvider::new(vec![
        vec![Ok(completed_text_event("old assistant no auto compaction"))],
        vec![Ok(completed_text_event(
            "final without automatic compaction",
        ))],
    ])
    .with_capabilities(
        ModelCapabilities::new(true, true, false, true, Some(360), Some(16))
            .expect("valid capabilities"),
    );
    let compactor = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "This checkpoint should not be requested.",
              "refs": ["r1"]
            }
          ],
          "working_intent": null
        }"#,
    ))]]);
    let runtime = Runtime::builder(session_id("agent-loop-auto-compaction-disabled"))
        .model_provider(Arc::new(primary.clone()), model_name())
        .model_provider_for_role(
            RuntimeModelRole::ContextCompaction,
            Arc::new(compactor.clone()),
            ModelName::new("fake/compactor").expect("valid model"),
        )
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime should build");

    let first = run_default_loop(&runtime, &"old no auto compaction user ".repeat(70)).await;
    assert_eq!(first.status(), &AgentLoopStatus::Completed);
    let second = run_default_loop(&runtime, "current no auto compaction user").await;
    assert_eq!(second.status(), &AgentLoopStatus::Completed);

    assert!(
        compactor.recorded_requests().is_empty(),
        "disabled automatic compaction must not call the compactor"
    );
    let primary_requests = primary.recorded_requests();
    assert_eq!(primary_requests.len(), 2);
    let final_text = primary_requests[1]
        .messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!final_text.contains("compacted-checkpoint:"));
    assert!(final_text.contains("old no auto compaction user"));
    assert!(final_text.contains("current no auto compaction user"));
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
    assert_eq!(result.model_turns_run(), 2);
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
            merry_core::ToolCallArguments::try_from(json!({"query": "test query"}))
                .expect("valid tool arguments"),
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

    let artifact = ArtifactRef::new(artifact_id("post-run-context-source"), ArtifactKind::Text);
    runtime
        .record_artifact(
            artifact.clone(),
            merry_runtime::ArtifactContent::text("post-run exact evidence\n"),
        )
        .await
        .expect("runtime should accept artifact after the loop completes");
    let evidence = EvidenceRef::new(artifact.id().clone(), EvidenceLocator::whole_artifact());
    runtime
        .record_context_summary(
            ContextSummary::new(
                "post-run-summary",
                "Raw context write may resume.",
                vec![
                    ContextEvidence::new("post-run source", evidence)
                        .expect("context evidence builds"),
                ],
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
async fn max_model_turns_blocks_before_infinite_tool_loop_and_leaves_pending() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-loop", "search_notes"),
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text("should not run\n");
    let runtime = runtime_with_tool("agent-loop-max-model-turns", provider, executor);

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
            reason: AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns: 1 },
        }
    );
    assert_eq!(result.model_turns_run(), 1);
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
fn agent_loop_config_rejects_zero_max_model_turns() {
    let err = AgentLoopConfig::new(0).expect_err("zero budget should be rejected");

    assert_eq!(err, AgentLoopConfigError::MaxModelTurnsMustBeNonZero);
}

#[tokio::test(flavor = "current_thread")]
async fn no_provider_loop_completes_like_skeleton_step() {
    let runtime = Runtime::builder(session_id("agent-loop-no-provider"))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "No provider.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 1);
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
