use futures_util::{StreamExt, stream};
use merry_core::{
    InteractiveRunState, PendingToolCall, ProviderName, QueuedInputLane, RuntimeEvent, SessionId,
    ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelMessageRole,
    ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_runtime::{
    AgentLoopConfig, InterruptReason, Runtime, StepContext, ToolExecutionContext,
    ToolExecutionError, ToolExecutor, ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::json;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::{
    sync::{Mutex as AsyncMutex, oneshot},
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

type ScriptedModelEvents = Vec<Result<ModelEvent, ModelError>>;
type ScriptedProviderSteps = Vec<ScriptedModelEvents>;

fn completed_text_event(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

fn model_tool_call(id: &str, name: &str) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::try_from(json!({"query": "test query"})).expect("valid model arguments"),
    )
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

fn tool_spec(name: &str) -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" }
        },
        "required": ["query"]
    }))
    .expect("test schema should be valid JSON schema");

    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Search test notes",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

#[derive(Clone)]
struct RecordingProvider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    steps: Arc<Mutex<ScriptedProviderSteps>>,
}

impl RecordingProvider {
    fn new() -> Self {
        Self::new_with_steps(vec![vec![Ok(completed_text_event("done"))]])
    }

    fn new_with_steps(steps: ScriptedProviderSteps) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
        }
    }

    fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    fn next_step(&self) -> Vec<Result<ModelEvent, ModelError>> {
        self.steps
            .lock()
            .expect("steps lock")
            .pop()
            .unwrap_or_else(|| vec![Ok(completed_text_event("done"))])
    }
}

impl ModelProvider for RecordingProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("interactive-test-provider").expect("valid provider"))
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.requests.lock().expect("requests lock").push(request);
            let event_stream: ModelEventStream = Box::pin(stream::iter(self.next_step()));
            Ok(event_stream)
        })
    }
}

#[derive(Clone)]
struct BlockingFirstProvider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    first_started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    first_release: Arc<AsyncMutex<Option<oneshot::Receiver<()>>>>,
}

impl BlockingFirstProvider {
    fn new(started: oneshot::Sender<()>, release: oneshot::Receiver<()>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            first_started: Arc::new(Mutex::new(Some(started))),
            first_release: Arc::new(AsyncMutex::new(Some(release))),
        }
    }

    fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl ModelProvider for BlockingFirstProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| {
            ProviderName::new("interactive-blocking-provider").expect("valid provider")
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.requests.lock().expect("requests lock").push(request);

            let blocks_first = self
                .first_started
                .lock()
                .expect("started lock")
                .take()
                .map(|sender| {
                    let _ = sender.send(());
                })
                .is_some();

            if blocks_first {
                let release = self
                    .first_release
                    .lock()
                    .await
                    .take()
                    .expect("first release receiver exists");
                release.await.expect("test releases first provider step");
                let event_stream: ModelEventStream =
                    Box::pin(stream::iter([Ok(completed_text_event("first done"))]));
                return Ok(event_stream);
            }

            let event_stream: ModelEventStream =
                Box::pin(stream::iter([Ok(completed_text_event("done"))]));
            Ok(event_stream)
        })
    }
}

#[derive(Clone)]
struct BlockingToolExecutor {
    started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl ToolExecutor for BlockingToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if let Some(started) = self.started.lock().expect("started lock").take() {
                let _ = started.send(());
            }
            context.cancellation_token().cancelled().await;
            Err(ToolExecutionError::Cancelled)
        })
    }
}

#[tokio::test]
async fn interactive_run_starts_waiting_for_input() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-waiting"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, _control) = run.split();

    let event = stream.next().await.expect("state event");
    assert!(matches!(
        event,
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test]
async fn submit_next_while_waiting_starts_model_turn() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-submit-next"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    assert!(matches!(
        stream.next().await.expect("state event"),
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));

    let item = input.submit_next("hello").await.expect("input queued");
    assert_eq!(item.lane(), QueuedInputLane::Next);
    assert_eq!(item.text(), "hello");

    let mut saw_accepted = false;
    while let Some(event) = stream.next().await {
        if matches!(event, RuntimeEvent::QueuedInputAccepted { .. }) {
            saw_accepted = true;
            break;
        }
    }
    assert!(saw_accepted);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn enqueue_while_waiting_starts_backlog_turn() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-backlog-waiting"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    let item = input.enqueue("later").await.expect("backlog queued");
    assert_eq!(item.lane(), QueuedInputLane::Backlog);
    assert_eq!(item.text(), "later");

    let mut saw_accepted = false;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            RuntimeEvent::QueuedInputAccepted {
                lane: QueuedInputLane::Backlog,
                ..
            }
        ) {
            saw_accepted = true;
            break;
        }
    }
    assert!(saw_accepted);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn next_burst_before_boundary_becomes_two_user_messages_in_one_request() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-next-burst"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let first = input.submit_next("first");
    let second = input.submit_next("second");
    let (first, second) = timeout(Duration::from_millis(200), async move {
        tokio::join!(first, second)
    })
    .await
    .expect("running step should keep accepting queued next input");
    let first = first.expect("first queued");
    let second = second.expect("second queued");

    release_tx.send(()).expect("first provider step released");

    let mut saw_two = false;
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Next,
        } = event
            && inputs
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>()
                == vec![first.text(), second.text()]
        {
            saw_two = true;
            break;
        }
    }
    assert!(saw_two);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let user_texts = requests[1]
        .messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::User)
        .map(|message| message.content().as_text().to_owned())
        .collect::<Vec<_>>();
    assert!(user_texts.ends_with(&["first".to_owned(), "second".to_owned()]));
}

#[tokio::test]
async fn next_burst_does_not_reorder_backlog() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-backlog-order"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    input.enqueue("backlog").await.expect("backlog queued");
    input.submit_next("next").await.expect("next queued");

    release_tx.send(()).expect("first provider step released");

    let mut accepted = Vec::new();
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted { inputs, .. } = event {
            accepted.extend(
                inputs
                    .into_iter()
                    .map(|item| item.text)
                    .filter(|text| text == "next" || text == "backlog"),
            );
            if accepted.contains(&"next".to_owned()) {
                break;
            }
        }
    }
    assert_eq!(accepted, vec!["next".to_owned()]);

    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Backlog,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["backlog"]
            );
            break;
        }
    }
}

#[tokio::test]
async fn input_handle_updates_removes_and_reorders_pending_items() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-edit-queue"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let mut first = input.enqueue("first").await.expect("first queued");
    let second = input.enqueue("second").await.expect("second queued");

    first
        .update("updated")
        .await
        .expect("pending input updates");
    let mut snapshot = input.snapshot().await.expect("snapshot");
    snapshot.backlog.swap(0, 1);
    input
        .replace_pending_order(QueuedInputLane::Backlog, &snapshot.backlog)
        .await
        .expect("pending input reorders");
    second.remove().await.expect("pending input removes");

    let snapshot = input.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.backlog.len(), 1);
    assert_eq!(snapshot.backlog[0].text(), "updated");

    release_tx.send(()).expect("first provider step released");

    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Backlog,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["updated"]
            );
            break;
        }
    }
}

#[tokio::test]
async fn interrupt_moves_existing_next_to_suspended_and_post_interrupt_next_runs_alone() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-esc-suspended"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("initial provider step starts");
    input.submit_next("x").await.expect("x queued");
    input.submit_next("y").await.expect("y queued");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let snapshot = input.snapshot().await.expect("snapshot");
    assert_eq!(
        snapshot
            .suspended
            .iter()
            .map(|item| item.text())
            .collect::<Vec<_>>(),
        vec!["x", "y"]
    );

    input.submit_next("z").await.expect("z queued");
    drop(release_tx);

    let mut saw_z = false;
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Next,
        } = event
            && inputs.iter().any(|item| item.text == "z")
        {
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].text, "z");
            saw_z = true;
            break;
        }
    }
    assert!(saw_z);
}

#[tokio::test]
async fn resume_suspended_accepts_suspended_burst_when_waiting() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-resume-suspended"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("initial provider step starts");
    input.submit_next("suspended").await.expect("queued");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");
    drop(release_tx);

    let mut waiting = false;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            waiting = true;
            break;
        }
    }
    assert!(waiting);

    control.resume_suspended().await.expect("suspended resumes");

    let mut saw_suspended = false;
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Suspended,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["suspended"]
            );
            saw_suspended = true;
            break;
        }
    }
    assert!(saw_suspended);
}

#[tokio::test]
async fn interrupt_during_tool_execution_closes_pending_tool_call() {
    let (started_tx, started_rx) = oneshot::channel();
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-blocking",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("after cancel"))],
    ]);
    let tool = BlockingToolExecutor {
        started: Arc::new(Mutex::new(Some(started_tx))),
    };
    let runtime = Runtime::builder(session_id("interactive-tool-interrupt"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(tool),
        ))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("start").await.expect("start queued");
    started_rx.await.expect("tool starts");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let mut saw_resolved = false;
    while let Some(event) = stream.next().await {
        if matches!(event, RuntimeEvent::ToolCallFinished { .. }) {
            saw_resolved = true;
            break;
        }
    }
    assert!(saw_resolved);
    assert!(runtime.pending_tool_calls().await.is_empty());
}
