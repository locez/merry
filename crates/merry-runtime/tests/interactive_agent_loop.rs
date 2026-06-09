use futures_util::{StreamExt, stream};
use merry_core::{ProviderName, SessionId};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelMessageRole,
    ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext,
};
use merry_runtime::{
    AgentLoopConfig, InteractiveRunEvent, InteractiveRunState, QueueKind, Runtime, StepContext,
};
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

fn completed_text_event(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

#[derive(Clone)]
struct RecordingProvider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    steps: Arc<Mutex<Vec<Vec<Result<ModelEvent, ModelError>>>>>,
}

impl RecordingProvider {
    fn new() -> Self {
        Self::new_with_steps(vec![vec![Ok(completed_text_event("done"))]])
    }

    fn new_with_steps(steps: Vec<Vec<Result<ModelEvent, ModelError>>>) -> Self {
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
        InteractiveRunEvent::StateChanged {
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
        InteractiveRunEvent::StateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));

    let receipt = input.submit_next("hello").await.expect("input queued");
    assert_eq!(receipt.queue, QueueKind::Next);

    let mut saw_accepted = false;
    while let Some(event) = stream.next().await {
        if matches!(event, InteractiveRunEvent::InputAccepted { .. }) {
            saw_accepted = true;
            break;
        }
    }
    assert!(saw_accepted);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn enqueue_while_waiting_does_not_start_until_resume_backlog() {
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
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.enqueue("later").await.expect("backlog queued");
    assert!(provider.recorded_requests().is_empty());

    control.resume_backlog().await.expect("backlog resumes");
    let mut saw_accepted = false;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            InteractiveRunEvent::InputAccepted {
                queue: QueueKind::Backlog,
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
    let first = first.expect("first queued").id;
    let second = second.expect("second queued").id;

    release_tx.send(()).expect("first provider step released");

    let mut saw_two = false;
    while let Some(event) = stream.next().await {
        if let InteractiveRunEvent::InputAccepted {
            ids,
            queue: QueueKind::Next,
        } = event
            && ids == vec![first, second]
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
    let provider = RecordingProvider::new();
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
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    let backlog = input.enqueue("backlog").await.expect("backlog queued").id;
    let next = input.submit_next("next").await.expect("next queued").id;

    let mut accepted = Vec::new();
    while let Some(event) = stream.next().await {
        if let InteractiveRunEvent::InputAccepted { ids, .. } = event {
            accepted.extend(ids);
            if accepted.contains(&next) {
                break;
            }
        }
    }
    assert_eq!(accepted, vec![next]);

    control.resume_backlog().await.expect("backlog resumes");
    while let Some(event) = stream.next().await {
        if let InteractiveRunEvent::InputAccepted {
            ids,
            queue: QueueKind::Backlog,
        } = event
        {
            assert_eq!(ids, vec![backlog]);
            break;
        }
    }
}
