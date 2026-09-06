use futures_util::stream;
use merry_core::{ModelUsage, ProviderName, ToolName};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use serde_json::json;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

pub(crate) fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

pub(crate) type ScriptedModelEvents = Vec<Result<ModelEvent, ModelError>>;

pub(crate) type ScriptedProviderSteps = Vec<ScriptedModelEvents>;

pub(crate) fn completed_text_event(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

pub(crate) fn completed_text_event_with_usage(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(text)],
            FinishReason::Stop,
            Some(ModelUsage::new(100, 10)),
        ),
    }
}

pub(crate) fn model_tool_call(id: &str, name: &str) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::try_from(json!({"query": "test query"})).expect("valid model arguments"),
    )
}

pub(crate) fn completed_tool_call_event(call: ModelToolCall) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    }
}

pub(crate) fn completed_tool_call_batch_event(calls: Vec<ModelToolCall>) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            calls.into_iter().map(ModelOutput::tool_call).collect(),
            FinishReason::ToolCalls,
            None,
        ),
    }
}

#[derive(Clone)]
pub(crate) struct RecordingProvider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    steps: Arc<Mutex<ScriptedProviderSteps>>,
}

impl RecordingProvider {
    pub(crate) fn new() -> Self {
        Self::new_with_steps(vec![vec![Ok(completed_text_event("done"))]])
    }

    pub(crate) fn new_with_steps(steps: ScriptedProviderSteps) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
        }
    }

    pub(crate) fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    pub(crate) fn next_step(&self) -> Vec<Result<ModelEvent, ModelError>> {
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
pub(crate) struct BlockingFirstProvider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    first_started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    first_release: Arc<AsyncMutex<Option<oneshot::Receiver<()>>>>,
}

impl BlockingFirstProvider {
    pub(crate) fn new(started: oneshot::Sender<()>, release: oneshot::Receiver<()>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            first_started: Arc::new(Mutex::new(Some(started))),
            first_release: Arc::new(AsyncMutex::new(Some(release))),
        }
    }

    pub(crate) fn recorded_requests(&self) -> Vec<ModelRequest> {
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
