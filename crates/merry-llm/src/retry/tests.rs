use super::{ModelRetryPolicy, RetryModelStreamContext, RetryingModelProvider};
use crate::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelContent, ModelError, ModelEvent,
    ModelEventStream, ModelMessage, ModelMessageRole, ModelName, ModelOutput, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext, ProviderErrorKind,
};
use futures_core::Stream;
use futures_util::{StreamExt, stream};
use merry_core::ProviderName;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc, oneshot},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

type AttemptScript = Vec<Result<ModelEvent, ModelError>>;
type AttemptScripts = Vec<AttemptScript>;
type GatedReceiver = mpsc::Receiver<Result<ModelEvent, ModelError>>;

fn request() -> ModelRequest {
    ModelRequest::new(
        ModelName::new("debug-model").expect("valid model name"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("hello").expect("valid text"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        GenerationConfig::default(),
    )
    .expect("valid request")
}

fn completed(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

fn expect_ok_event(item: Option<Result<ModelEvent, ModelError>>) -> ModelEvent {
    item.expect("model event stream should remain open")
        .expect("model event should succeed")
}

#[derive(Clone)]
struct AttemptScriptProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    attempts: Arc<Mutex<AttemptScripts>>,
}

impl AttemptScriptProvider {
    fn new(attempts: AttemptScripts) -> Self {
        Self {
            name: ProviderName::new("attempt-script").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            attempts: Arc::new(Mutex::new(attempts)),
        }
    }

    fn attempts_remaining(&self) -> usize {
        self.attempts
            .lock()
            .expect("attempts lock should not be poisoned")
            .len()
    }
}

impl ModelProvider for AttemptScriptProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let attempt = self
                .attempts
                .lock()
                .expect("attempts lock should not be poisoned")
                .remove(0);
            let stream: ModelEventStream = Box::pin(stream::iter(attempt));
            Ok(stream)
        })
    }
}

#[tokio::test]
async fn retrying_provider_does_not_retry_after_visible_output() {
    let inner = AttemptScriptProvider::new(vec![
        vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "partial".to_owned(),
            }),
            Err(ModelError::provider(
                ProviderErrorKind::Unavailable,
                "stream broke",
            )),
        ],
        vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "final".to_owned(),
            }),
            Ok(completed("final")),
        ],
    ]);
    let retrying = RetryingModelProvider::new(
        Arc::new(inner.clone()),
        ModelRetryPolicy::new(
            true,
            3,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(100),
            false,
        )
        .expect("valid policy"),
    );
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);

    let events = retrying
        .stream_model_with_retry_events(
            request(),
            RetryModelStreamContext::new(ModelStreamContext::default()).with_retry_events(sender),
        )
        .await
        .expect("retrying setup should succeed")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], Ok(ModelEvent::Started)));
    assert_eq!(
        events[1].as_ref().expect("delta should succeed"),
        &ModelEvent::OutputTextDelta {
            delta: "partial".to_owned(),
        }
    );
    assert!(matches!(
        &events[2],
        Err(error) if error.kind() == ProviderErrorKind::Unavailable
    ));
    assert_eq!(inner.attempts_remaining(), 1);

    let mut retry_events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        retry_events.push(event);
    }
    assert_eq!(retry_events.len(), 1);
    assert!(matches!(
        retry_events[0],
        super::ModelRetryEvent::AttemptStarted { attempt: 1, .. }
    ));
}

#[tokio::test]
async fn retrying_provider_retries_before_visible_output_with_one_started_event() {
    let inner = AttemptScriptProvider::new(vec![
        vec![Err(ModelError::provider(
            ProviderErrorKind::Unavailable,
            "setup stream failed",
        ))],
        vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "recovered".to_owned(),
            }),
            Ok(completed("recovered")),
        ],
    ]);
    let retrying = RetryingModelProvider::new(
        Arc::new(inner.clone()),
        ModelRetryPolicy::new(
            true,
            3,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(100),
            false,
        )
        .expect("valid retry policy"),
    );

    let events = retrying
        .stream_model(request(), ModelStreamContext::default())
        .await
        .expect("retry stream setup should succeed")
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("pre-output retry should recover");

    assert_eq!(
        events,
        vec![
            ModelEvent::Started,
            ModelEvent::OutputTextDelta {
                delta: "recovered".to_owned(),
            },
            completed("recovered"),
        ]
    );
    assert_eq!(inner.attempts_remaining(), 0);
}

#[derive(Clone)]
struct GatedProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    receiver: Arc<Mutex<Option<GatedReceiver>>>,
}

impl GatedProvider {
    fn new(receiver: GatedReceiver) -> Self {
        Self {
            name: ProviderName::new("gated-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            receiver: Arc::new(Mutex::new(Some(receiver))),
        }
    }
}

impl ModelProvider for GatedProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let receiver = self
                .receiver
                .lock()
                .expect("gated provider lock should not be poisoned")
                .take()
                .expect("gated provider supports one attempt");
            let stream = stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|item| (item, receiver))
            });
            let stream: ModelEventStream = Box::pin(stream);
            Ok(stream)
        })
    }
}

#[tokio::test]
async fn retrying_provider_yields_delta_before_attempt_completes() {
    let (sender, receiver) = mpsc::channel(8);
    let retrying = RetryingModelProvider::new(
        Arc::new(GatedProvider::new(receiver)),
        ModelRetryPolicy::new(
            true,
            2,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_secs(5),
            false,
        )
        .expect("valid retry policy"),
    );

    sender
        .send(Ok(ModelEvent::Started))
        .await
        .expect("provider receiver should be open");
    sender
        .send(Ok(ModelEvent::OutputTextDelta {
            delta: "live".to_owned(),
        }))
        .await
        .expect("provider receiver should be open");

    let mut stream = timeout(
        Duration::from_millis(100),
        retrying.stream_model(request(), ModelStreamContext::default()),
    )
    .await
    .expect("retry stream setup should not wait for completion")
    .expect("retry stream setup should succeed");

    assert_eq!(
        expect_ok_event(
            timeout(Duration::from_millis(100), stream.next())
                .await
                .expect("outer started event should arrive"),
        ),
        ModelEvent::Started
    );
    assert_eq!(
        expect_ok_event(
            timeout(Duration::from_millis(100), stream.next())
                .await
                .expect("live delta should arrive"),
        ),
        ModelEvent::OutputTextDelta {
            delta: "live".to_owned(),
        }
    );

    sender
        .send(Ok(completed("live")))
        .await
        .expect("provider receiver should be open");
    assert_eq!(
        expect_ok_event(
            timeout(Duration::from_millis(100), stream.next())
                .await
                .expect("completion should arrive"),
        ),
        completed("live")
    );
}

#[derive(Clone)]
struct FloodProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    buffer_filled: Arc<Notify>,
    dropped_sender: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl FloodProvider {
    fn new() -> (Self, oneshot::Receiver<()>) {
        let (dropped_sender, dropped_receiver) = oneshot::channel();
        (
            Self {
                name: ProviderName::new("flood-provider").expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities"),
                buffer_filled: Arc::new(Notify::new()),
                dropped_sender: Arc::new(Mutex::new(Some(dropped_sender))),
            },
            dropped_receiver,
        )
    }
}

impl ModelProvider for FloodProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        let buffer_filled = Arc::clone(&self.buffer_filled);
        let dropped_sender = self
            .dropped_sender
            .lock()
            .expect("flood provider lock should not be poisoned")
            .take()
            .expect("flood provider supports one attempt");
        Box::pin(async move {
            let stream: ModelEventStream = Box::pin(FloodStream {
                poll_count: 0,
                buffer_filled,
                dropped_sender: Some(dropped_sender),
            });
            Ok(stream)
        })
    }
}

struct FloodStream {
    poll_count: usize,
    buffer_filled: Arc<Notify>,
    dropped_sender: Option<oneshot::Sender<()>>,
}

impl Stream for FloodStream {
    type Item = Result<ModelEvent, ModelError>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.poll_count += 1;
        if self.poll_count >= super::RETRY_STREAM_BUFFER {
            self.buffer_filled.notify_one();
        }
        Poll::Ready(Some(Ok(ModelEvent::OutputTextDelta {
            delta: "x".to_owned(),
        })))
    }
}

impl Drop for FloodStream {
    fn drop(&mut self) {
        if let Some(sender) = self.dropped_sender.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test]
async fn cancellation_drops_provider_stream_while_output_buffer_is_full() {
    let (inner, dropped_receiver) = FloodProvider::new();
    let buffer_filled = Arc::clone(&inner.buffer_filled);
    let token = CancellationToken::new();
    let _stream =
        RetryingModelProvider::new(Arc::new(inner), ModelRetryPolicy::coding_agent_default())
            .stream_model(request(), ModelStreamContext::new(token.clone()))
            .await
            .expect("retry stream setup should succeed");

    timeout(Duration::from_millis(100), buffer_filled.notified())
        .await
        .expect("provider should fill the outer buffer");
    token.cancel();

    timeout(Duration::from_millis(100), dropped_receiver)
        .await
        .expect("cancellation should drop the blocked provider stream")
        .expect("drop notification sender should remain valid");
}

#[tokio::test]
async fn dropping_outer_stream_drops_blocked_provider_stream() {
    let (inner, dropped_receiver) = FloodProvider::new();
    let buffer_filled = Arc::clone(&inner.buffer_filled);
    let stream =
        RetryingModelProvider::new(Arc::new(inner), ModelRetryPolicy::coding_agent_default())
            .stream_model(request(), ModelStreamContext::default())
            .await
            .expect("retry stream setup should succeed");

    timeout(Duration::from_millis(100), buffer_filled.notified())
        .await
        .expect("provider should fill the outer buffer");
    drop(stream);

    timeout(Duration::from_millis(100), dropped_receiver)
        .await
        .expect("dropping the consumer should drop the provider stream")
        .expect("drop notification sender should remain valid");
}
