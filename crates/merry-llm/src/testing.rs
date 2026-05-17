//! Deterministic model provider utilities for tests.

use crate::{
    ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelStreamContext, ProviderErrorKind,
};
use futures_core::Stream;
use merry_core::ProviderName;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

/// Deterministic fake provider that replays scripted stream items.
#[derive(Debug, Clone)]
pub struct FakeModelProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    script: Arc<Vec<FakeStreamItem>>,
    recorded_requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl FakeModelProvider {
    /// Creates a fake provider from scripted stream items.
    pub fn new(script: Vec<Result<ModelEvent, ModelError>>) -> Self {
        Self {
            name: ProviderName::new("fake-model-provider").expect("static provider name is valid"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("static capabilities are valid"),
            script: Arc::new(script.into_iter().map(FakeStreamItem::from).collect()),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns the requests recorded after non-cancelled setup.
    #[must_use]
    pub fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.recorded_requests
            .lock()
            .expect("fake provider request mutex should not be poisoned")
            .clone()
    }
}

impl ModelProvider for FakeModelProvider {
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
                .expect("fake provider request mutex should not be poisoned")
                .push(request);

            let stream = FakeEventStream {
                script: Arc::clone(&self.script),
                index: 0,
                completed: false,
                cancellation_token: context.cancellation_token().clone(),
            };

            let stream: ModelEventStream = Box::pin(stream);
            Ok(stream)
        })
    }
}

#[derive(Debug, Clone)]
enum FakeStreamItem {
    Event(ModelEvent),
    Error(FakeErrorItem),
}

#[derive(Debug, Clone)]
enum FakeErrorItem {
    InvalidRequest(String),
    Cancelled,
    Provider(ProviderErrorKind, String),
}

impl From<Result<ModelEvent, ModelError>> for FakeStreamItem {
    fn from(item: Result<ModelEvent, ModelError>) -> Self {
        match item {
            Ok(event) => Self::Event(event),
            Err(ModelError::InvalidRequest { reason }) => {
                Self::Error(FakeErrorItem::InvalidRequest(reason))
            }
            Err(ModelError::Cancelled) => Self::Error(FakeErrorItem::Cancelled),
            Err(ModelError::Provider { kind, message }) => {
                Self::Error(FakeErrorItem::Provider(kind, message))
            }
        }
    }
}

impl FakeErrorItem {
    fn into_model_error(self) -> ModelError {
        match self {
            Self::InvalidRequest(reason) => ModelError::invalid_request(reason),
            Self::Cancelled => ModelError::Cancelled,
            Self::Provider(kind, message) => ModelError::provider(kind, message),
        }
    }
}

struct FakeEventStream {
    script: Arc<Vec<FakeStreamItem>>,
    index: usize,
    completed: bool,
    cancellation_token: tokio_util::sync::CancellationToken,
}

impl Stream for FakeEventStream {
    type Item = Result<ModelEvent, ModelError>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.completed {
            return Poll::Ready(None);
        }

        if self.cancellation_token.is_cancelled() {
            self.completed = true;
            return Poll::Ready(Some(Err(ModelError::Cancelled)));
        }

        let item = match self.script.get(self.index).cloned() {
            Some(item) => item,
            None => {
                self.completed = true;
                return Poll::Ready(None);
            }
        };
        self.index += 1;

        match item {
            FakeStreamItem::Event(event) => {
                if matches!(event, ModelEvent::Completed { .. }) {
                    self.completed = true;
                }
                Poll::Ready(Some(Ok(event)))
            }
            FakeStreamItem::Error(error) => {
                self.completed = true;
                Poll::Ready(Some(Err(error.into_model_error())))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FakeModelProvider;
    use crate::{
        FinishReason, GenerationConfig, ModelContent, ModelError, ModelEvent, ModelMessage,
        ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelRequest, ModelResponse,
        ModelStreamContext, ProviderErrorKind, Usage,
    };
    use futures_executor::block_on;
    use futures_util::TryStreamExt;
    use merry_core::{ToolInputSchema, ToolName, ToolSpec};
    use schemars::Schema;
    use serde_json::json;

    fn weather_tool() -> ToolSpec {
        let schema = Schema::try_from(json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        }))
        .expect("test schema should be a JSON schema");

        ToolSpec::new(
            ToolName::new("lookup_weather").expect("valid tool name"),
            "Look up weather for a city",
            ToolInputSchema::new(schema).expect("valid object schema"),
        )
        .expect("valid tool spec")
    }

    fn request_with_tools(tools: Vec<ToolSpec>) -> ModelRequest {
        ModelRequest::new(
            ModelName::new("fake/model").expect("valid model name"),
            vec![
                ModelMessage::new(
                    ModelMessageRole::User,
                    ModelContent::text("hello").expect("valid content"),
                )
                .expect("valid message"),
            ],
            tools,
            GenerationConfig::default(),
        )
        .expect("valid request")
    }

    fn request() -> ModelRequest {
        request_with_tools(Vec::new())
    }

    #[test]
    fn fake_provider_replays_scripted_events_in_order() {
        let response = ModelResponse::new(
            vec![ModelOutput::text("hello")],
            FinishReason::Stop,
            Some(Usage::new(3, 1)),
        );
        let provider = FakeModelProvider::new(vec![
            Ok(ModelEvent::Started),
            Ok(ModelEvent::OutputTextDelta {
                delta: "hel".to_owned(),
            }),
            Ok(ModelEvent::OutputTextDelta {
                delta: "lo".to_owned(),
            }),
            Ok(ModelEvent::Completed {
                response: response.clone(),
            }),
            Ok(ModelEvent::OutputTextDelta {
                delta: "must not emit".to_owned(),
            }),
        ]);

        let stream = block_on(provider.stream_model(request(), ModelStreamContext::default()))
            .expect("fake setup should succeed");
        let events: Vec<ModelEvent> =
            block_on(stream.try_collect()).expect("fake stream should succeed");

        assert_eq!(
            events,
            vec![
                ModelEvent::Started,
                ModelEvent::OutputTextDelta {
                    delta: "hel".to_owned()
                },
                ModelEvent::OutputTextDelta {
                    delta: "lo".to_owned()
                },
                ModelEvent::Completed { response },
            ]
        );
        assert_eq!(provider.recorded_requests().len(), 1);
    }

    #[test]
    fn fake_provider_records_requests_with_tool_specs_unchanged() {
        let provider = FakeModelProvider::new(vec![Ok(ModelEvent::Started)]);
        let tool = weather_tool();

        let stream = block_on(provider.stream_model(
            request_with_tools(vec![tool.clone()]),
            ModelStreamContext::default(),
        ))
        .expect("fake setup should succeed");
        let events: Vec<ModelEvent> =
            block_on(stream.try_collect()).expect("fake stream should succeed");

        assert_eq!(events, vec![ModelEvent::Started]);
        let recorded_requests = provider.recorded_requests();
        assert_eq!(recorded_requests.len(), 1);
        assert_eq!(recorded_requests[0].tools(), &[tool]);
    }

    #[test]
    fn fake_provider_can_emit_deterministic_stream_error() {
        let provider = FakeModelProvider::new(vec![Err(ModelError::provider(
            ProviderErrorKind::Protocol,
            "fixture protocol failure",
        ))]);

        let stream = block_on(provider.stream_model(request(), ModelStreamContext::default()))
            .expect("fake setup should succeed");
        let error = block_on(stream.try_collect::<Vec<_>>()).expect_err("stream should fail");

        assert_eq!(error.kind(), ProviderErrorKind::Protocol);
        assert!(error.to_string().contains("fixture protocol failure"));
        assert_eq!(provider.recorded_requests().len(), 1);
    }

    #[test]
    fn fake_provider_cancellation_before_setup_records_no_side_effects() {
        let provider = FakeModelProvider::new(vec![Ok(ModelEvent::Started)]);
        let context = ModelStreamContext::default();
        context.cancellation_token().cancel();

        let result = block_on(provider.stream_model(request(), context));
        let error = match result {
            Ok(_) => panic!("cancelled setup should fail deterministically"),
            Err(error) => error,
        };

        assert!(matches!(error, ModelError::Cancelled));
        assert!(provider.recorded_requests().is_empty());
    }
}
