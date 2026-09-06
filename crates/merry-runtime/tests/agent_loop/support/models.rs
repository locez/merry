use futures_util::stream;
use merry_core::{ModelUsage, ProviderName, ToolName};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

pub(crate) fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

pub(crate) fn model_tool_call(id: &str, name: &str) -> ModelToolCall {
    model_tool_call_with_arguments(id, name, json!({"query": "test query"}))
}

pub(crate) fn model_tool_call_with_arguments(
    id: &str,
    name: &str,
    arguments: Value,
) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::try_from(arguments).expect("valid model tool arguments"),
    )
}

pub(crate) fn completed_text_event(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

pub(crate) fn completed_text_event_with_usage(text: &str, usage: ModelUsage) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(text)],
            FinishReason::Stop,
            Some(usage),
        ),
    }
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

#[derive(Debug, Clone)]
pub(crate) struct ScriptedModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<Vec<ScriptedModelStep>>>,
    recorded_requests: Arc<Mutex<Vec<ModelRequest>>>,
}

pub(crate) type ScriptedModelStep = Vec<Result<ModelEvent, ModelError>>;

impl ScriptedModelProvider {
    pub(crate) fn new(steps: Vec<ScriptedModelStep>) -> Self {
        Self {
            name: ProviderName::new("agent-loop-scripted-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn with_capabilities(mut self, capabilities: ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub(crate) fn recorded_requests(&self) -> Vec<ModelRequest> {
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
pub(crate) struct BlockingModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    started_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    release_rx: Arc<AsyncMutex<Option<oneshot::Receiver<()>>>>,
}

impl BlockingModelProvider {
    pub(crate) fn new(started_tx: oneshot::Sender<()>, release_rx: oneshot::Receiver<()>) -> Self {
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
