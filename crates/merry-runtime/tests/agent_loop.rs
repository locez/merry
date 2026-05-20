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
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopConfigError, AgentLoopStatus, ArtifactError,
    ContextSummary, DEFAULT_AGENT_LOOP_CONTINUATION_INPUT, Runtime, RuntimeError, StepContext,
    StepInput, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_util::sync::CancellationToken;

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
        .register_tool(merry_runtime::RegisteredTool::new(
            tool_spec("search_notes"),
            Arc::new(executor),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build")
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
    assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::User);
    assert_eq!(
        requests[0].messages()[0].content().as_text(),
        "Search notes."
    );
    assert!(requests[0].continuations().is_empty());
    assert_eq!(
        requests[1].messages()[0].content().as_text(),
        DEFAULT_AGENT_LOOP_CONTINUATION_INPUT
    );
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
async fn executor_infrastructure_error_preserves_events_and_pending_call() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-infra-error", "search_notes"),
    ))]]);
    let executor = ScriptedToolExecutor::infrastructure_error("temporary executor outage");
    let runtime = runtime_with_tool("agent-loop-infra-error", provider, executor);

    let err = runtime
        .run_agent_loop(
            StepInput::user_text("Search notes.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .await
        .expect_err("infrastructure error should stop the loop as a method error");

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
