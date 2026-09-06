use futures_util::stream;
use merry_core::{ProviderName, ToolName};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub(crate) type GatedModelEventReceiver = mpsc::Receiver<Result<ModelEvent, ModelError>>;

pub(crate) fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

pub(crate) fn completed_event() -> ModelEvent {
    completed_event_with_finish(FinishReason::Stop)
}

pub(crate) fn completed_event_with_finish(finish_reason: FinishReason) -> ModelEvent {
    completed_outputs_event(vec![ModelOutput::text("model result")], finish_reason)
}

pub(crate) fn completed_text_event(text: &str) -> ModelEvent {
    completed_outputs_event(vec![ModelOutput::text(text)], FinishReason::Stop)
}

pub(crate) fn completed_outputs_event(
    outputs: Vec<ModelOutput>,
    finish_reason: FinishReason,
) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(outputs, finish_reason, None),
    }
}

pub(crate) fn model_tool_call() -> ModelToolCall {
    model_tool_call_with_id("call-1")
}

pub(crate) fn model_tool_call_with_id(id: &str) -> ModelToolCall {
    model_tool_call_with_args(
        id,
        "search_notes",
        Map::from_iter([("query".to_owned(), json!("test query"))]),
    )
}

pub(crate) fn model_tool_call_with_args(
    id: &str,
    name: &str,
    arguments: Map<String, Value>,
) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::new(arguments),
    )
}

#[derive(Debug, Clone)]
pub(crate) struct ScriptedModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    steps: Arc<Mutex<Vec<ScriptedProviderStep>>>,
    recorded_requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[derive(Debug)]
pub(crate) enum ScriptedProviderStep {
    Stream(Vec<Result<ModelEvent, ModelError>>),
    SetupError(ModelError),
}

impl ScriptedModelProvider {
    pub(crate) fn new(scripts: Vec<Vec<Result<ModelEvent, ModelError>>>) -> Self {
        Self::new_steps(
            scripts
                .into_iter()
                .map(ScriptedProviderStep::Stream)
                .collect(),
        )
    }

    pub(crate) fn new_steps(steps: Vec<ScriptedProviderStep>) -> Self {
        Self {
            name: ProviderName::new("scripted-model-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
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

#[derive(Clone)]
pub(crate) struct GatedStreamingProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    receiver: Arc<Mutex<Option<GatedModelEventReceiver>>>,
}

impl GatedStreamingProvider {
    pub(crate) fn new(receiver: GatedModelEventReceiver) -> Self {
        Self {
            name: ProviderName::new("gated-runtime-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            receiver: Arc::new(Mutex::new(Some(receiver))),
        }
    }
}

impl ModelProvider for GatedStreamingProvider {
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
