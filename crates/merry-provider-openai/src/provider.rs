//! Provider implementation for the OpenAI Responses adapter.

use crate::{OpenAiProviderConfig, OpenAiProviderError, parse::ResponsesStreamParser};
use futures_util::stream;
use merry_core::ProviderName;
use merry_llm::{
    ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelStreamContext, ProviderErrorKind,
};
use serde_json::Value;
use std::collections::VecDeque;
use tracing::Instrument;

const AUTHORIZATION_HEADER: &str = "Authorization";
const ACCEPT_HEADER: &str = "Accept";
const SSE_ACCEPT_HEADER_VALUE: &str = "text/event-stream";
const USER_AGENT_HEADER: &str = "User-Agent";
const USER_AGENT_HEADER_VALUE: &str = concat!("merry/", env!("CARGO_PKG_VERSION"));
const OPENAI_ORGANIZATION_HEADER: &str = "OpenAI-Organization";
const OPENAI_PROJECT_HEADER: &str = "OpenAI-Project";

/// Config-backed OpenAI-compatible provider.
#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    config: OpenAiProviderConfig,
    client: reqwest::Client,
}

impl OpenAiProvider {
    /// Creates a provider from validated config.
    #[must_use]
    pub fn new(config: OpenAiProviderConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Returns the provider configuration.
    #[must_use]
    pub fn config(&self) -> &OpenAiProviderConfig {
        &self.config
    }
}

impl ModelProvider for OpenAiProvider {
    fn name(&self) -> &ProviderName {
        self.config.provider_name()
    }

    fn capabilities(&self) -> &ModelCapabilities {
        self.config.capabilities()
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        let stream_span = tracing::debug_span!(
            "runtime.provider.stream",
            event = "runtime.provider.stream",
            provider_name = self.config.provider_name().as_str(),
            model = request.model().as_str(),
            message_count = request.messages().len(),
            tool_count = request.tools().len(),
            continuation_count = request.continuations().len(),
            max_output_tokens = request.generation().max_output_tokens(),
            allow_parallel_tool_calls = request.generation().allow_parallel_tool_calls(),
            endpoint_path = tracing::field::Empty,
        );
        let event_stream_span = stream_span.clone();

        Box::pin(
            async move {
                if context.cancellation_token().is_cancelled() {
                    tracing::debug!("openai stream setup cancelled");
                    return Err(ModelError::Cancelled);
                }

                let http_request = build_and_trace_responses_http_request(
                    &self.config,
                    &request,
                    &event_stream_span,
                )?;
                tracing::trace!(event = "runtime.provider.request.rendered");

                let mut request_builder = self
                    .client
                    .post(http_request.endpoint)
                    .json(&http_request.body);

                for header in http_request.headers {
                    request_builder = request_builder.header(header.name, header.value);
                }

                let token = context.cancellation_token().clone();
                tracing::debug!("openai http send start");
                let response = tokio::select! {
                    () = token.cancelled() => {
                        tracing::debug!("openai stream setup cancelled");
                        return Err(ModelError::Cancelled);
                    }
                    response = request_builder.send() => response.map_err(map_transport_error)?,
                };

                let status = response.status();
                if status.is_success() {
                    tracing::debug!("openai http status received and classified");
                } else {
                    let error_kind = classify_http_status(status);
                    tracing::debug!("openai http status received and classified");
                    let error = map_status_error(response, &token, error_kind).await;
                    if matches!(error, ModelError::Cancelled) {
                        tracing::debug!("openai stream setup cancelled");
                    }
                    return Err(error);
                }

                let event_stream = stream::unfold(
                    OpenAiEventStreamState::new(response, token, event_stream_span),
                    |state| async move { state.next_item().await },
                );
                let event_stream: ModelEventStream = Box::pin(event_stream);
                tracing::debug!("openai event stream created");
                Ok(event_stream)
            }
            .instrument(stream_span),
        )
    }
}

struct OpenAiEventStreamState {
    response: reqwest::Response,
    events: OpenAiEventStreamEvents,
    cancellation_token: tokio_util::sync::CancellationToken,
    span: tracing::Span,
    done: bool,
}

impl OpenAiEventStreamState {
    fn new(
        response: reqwest::Response,
        cancellation_token: tokio_util::sync::CancellationToken,
        span: tracing::Span,
    ) -> Self {
        Self {
            response,
            events: OpenAiEventStreamEvents::new(),
            cancellation_token,
            span,
            done: false,
        }
    }

    async fn next_item(self) -> Option<(Result<ModelEvent, ModelError>, Self)> {
        let span = self.span.clone();
        async move { self.next_item_inner().await }
            .instrument(span)
            .await
    }

    async fn next_item_inner(mut self) -> Option<(Result<ModelEvent, ModelError>, Self)> {
        loop {
            if self.done {
                return None;
            }

            if self.cancellation_token.is_cancelled() {
                tracing::debug!("openai stream cancelled");
                self.done = true;
                return Some((Err(ModelError::Cancelled), self));
            }

            if let Some(event) = self.events.pop_pending() {
                self.trace_pending_event(&event);
                return Some((Ok(event), self));
            }

            let chunk = tokio::select! {
                () = self.cancellation_token.cancelled() => {
                    tracing::debug!("openai stream cancelled");
                    self.done = true;
                    return Some((Err(ModelError::Cancelled), self));
                }
                chunk = self.response.chunk() => chunk,
            };

            match chunk {
                Ok(Some(chunk)) => {
                    tracing::trace!(
                        chunk_byte_length = chunk.len(),
                        "openai stream chunk received"
                    );
                    if let Err(error) = self.events.parse_bytes(chunk.as_ref()) {
                        tracing::debug!("openai stream protocol error");
                        self.done = true;
                        return Some((Err(error.into()), self));
                    }
                }
                Ok(None) => match self.events.finish_stream_and_pop_pending() {
                    Ok(Some(event)) => {
                        self.trace_pending_event(&event);
                        return Some((Ok(event), self));
                    }
                    Ok(None) => {
                        self.done = true;
                        return None;
                    }
                    Err(error) => {
                        tracing::debug!("openai stream protocol error");
                        self.done = true;
                        return Some((Err(error.into()), self));
                    }
                },
                Err(error) => {
                    tracing::debug!("openai stream transport error");
                    self.done = true;
                    return Some((Err(map_transport_error(error)), self));
                }
            }
        }
    }

    fn trace_pending_event(&mut self, event: &ModelEvent) {
        tracing::trace!(
            pending_event_category = model_event_category(event),
            "openai stream pending event"
        );
        if matches!(event, ModelEvent::Completed { .. }) {
            tracing::debug!("openai stream completed");
            self.done = true;
        }
    }
}

struct OpenAiEventStreamEvents {
    parser: ResponsesStreamParser,
    line_buffer: Vec<u8>,
    pending: VecDeque<ModelEvent>,
}

impl OpenAiEventStreamEvents {
    fn new() -> Self {
        Self {
            parser: ResponsesStreamParser::new(),
            line_buffer: Vec::new(),
            pending: VecDeque::from([ModelEvent::Started]),
        }
    }

    fn pop_pending(&mut self) -> Option<ModelEvent> {
        self.pending.pop_front()
    }

    fn parse_bytes(&mut self, bytes: &[u8]) -> Result<(), OpenAiProviderError> {
        for byte in bytes {
            self.line_buffer.push(*byte);
            if *byte == b'\n' {
                self.parse_buffered_line()?;
            }
        }

        Ok(())
    }

    fn parse_buffered_line(&mut self) -> Result<(), OpenAiProviderError> {
        let line = std::str::from_utf8(&self.line_buffer).map_err(|error| {
            OpenAiProviderError::protocol(format!("stream line is not valid UTF-8: {error}"))
        })?;
        self.pending.extend(self.parser.parse_sse_line(line)?);
        self.line_buffer.clear();
        Ok(())
    }

    fn finish_stream(&mut self) -> Result<(), OpenAiProviderError> {
        if !self.line_buffer.is_empty() {
            self.parse_buffered_line()?;
        }

        self.parser.finish()
    }

    fn finish_stream_and_pop_pending(&mut self) -> Result<Option<ModelEvent>, OpenAiProviderError> {
        self.finish_stream()?;
        Ok(self.pop_pending())
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ResponsesHttpRequest {
    endpoint: reqwest::Url,
    headers: Vec<ResponsesHttpHeader>,
    body: Value,
}

impl ResponsesHttpRequest {
    #[cfg(test)]
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResponsesHttpHeader {
    name: &'static str,
    value: String,
}

fn build_responses_http_request(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
) -> Result<ResponsesHttpRequest, ModelError> {
    let mut headers = vec![
        ResponsesHttpHeader {
            name: AUTHORIZATION_HEADER,
            value: format!("Bearer {}", config.api_key()),
        },
        ResponsesHttpHeader {
            name: ACCEPT_HEADER,
            value: SSE_ACCEPT_HEADER_VALUE.to_owned(),
        },
        ResponsesHttpHeader {
            name: USER_AGENT_HEADER,
            value: USER_AGENT_HEADER_VALUE.to_owned(),
        },
    ];
    if let Some(organization) = config.organization() {
        headers.push(ResponsesHttpHeader {
            name: OPENAI_ORGANIZATION_HEADER,
            value: organization.to_owned(),
        });
    }
    if let Some(project) = config.project() {
        headers.push(ResponsesHttpHeader {
            name: OPENAI_PROJECT_HEADER,
            value: project.to_owned(),
        });
    }

    Ok(ResponsesHttpRequest {
        endpoint: responses_endpoint(config.base_url())?,
        headers,
        body: crate::render::render_responses_request(request)?,
    })
}

fn build_and_trace_responses_http_request(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
    span: &tracing::Span,
) -> Result<ResponsesHttpRequest, ModelError> {
    let http_request = build_responses_http_request(config, request)?;
    span.record("endpoint_path", http_request.endpoint.path());
    trace_openai_request_metadata(config, request, http_request.endpoint.path());
    Ok(http_request)
}

fn trace_openai_request_metadata(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
    endpoint_path: &str,
) {
    tracing::debug!(
        event = "runtime.provider.request",
        provider_name = config.provider_name().as_str(),
        model = request.model().as_str(),
        message_count = request.messages().len(),
        tool_count = request.tools().len(),
        continuation_count = request.continuations().len(),
        max_output_tokens = request.generation().max_output_tokens(),
        allow_parallel_tool_calls = request.generation().allow_parallel_tool_calls(),
        endpoint_path,
        "runtime provider request metadata"
    );
}

fn responses_endpoint(base_url: &str) -> Result<reqwest::Url, ModelError> {
    let endpoint = format!("{}/responses", base_url.trim_end_matches('/'));
    reqwest::Url::parse(&endpoint).map_err(|error| {
        OpenAiProviderError::invalid_config(format!(
            "base_url does not form a valid Responses endpoint: {error}"
        ))
        .into()
    })
}

async fn map_status_error(
    response: reqwest::Response,
    cancellation_token: &tokio_util::sync::CancellationToken,
    kind: ProviderErrorKind,
) -> ModelError {
    let status = response.status();
    let body = tokio::select! {
        () = cancellation_token.cancelled() => return ModelError::Cancelled,
        body = response.text() => body.unwrap_or_else(|error| {
            format!("failed to read provider error body: {error}")
        }),
    };
    let message = format!(
        "OpenAI Responses request returned HTTP {status}: {}",
        truncate_for_error(body.trim())
    );

    ModelError::from(OpenAiProviderError::provider(kind, message))
}

fn model_event_category(event: &ModelEvent) -> &'static str {
    match event {
        ModelEvent::Started => "started",
        ModelEvent::OutputTextDelta { .. } => "output_text_delta",
        ModelEvent::ToolCallRequested { .. } => "tool_call_requested",
        ModelEvent::Completed { .. } => "completed",
    }
}

fn classify_http_status(status: reqwest::StatusCode) -> ProviderErrorKind {
    match status.as_u16() {
        401 | 403 => ProviderErrorKind::Authentication,
        429 => ProviderErrorKind::RateLimited,
        500..=599 => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::Other,
    }
}

fn map_transport_error(error: reqwest::Error) -> ModelError {
    ModelError::from(OpenAiProviderError::provider(
        ProviderErrorKind::Unavailable,
        format!("OpenAI Responses transport failed: {error}"),
    ))
}

fn truncate_for_error(value: &str) -> String {
    const MAX_LEN: usize = 512;
    let mut output = String::new();
    for character in value.chars().take(MAX_LEN) {
        output.push(character);
    }
    if value.chars().count() > MAX_LEN {
        output.push_str("...");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        OpenAiEventStreamEvents, build_and_trace_responses_http_request,
        build_responses_http_request, classify_http_status, trace_openai_request_metadata,
    };
    use crate::OpenAiProviderConfig;
    use crate::parse::ResponsesStreamParser;
    use merry_llm::{
        FinishReason, GenerationConfig, ModelContent, ModelEvent, ModelMessage, ModelMessageRole,
        ModelName, ModelOutput, ModelProvider, ModelRequest, ModelResponse, ModelStreamContext,
        ProviderErrorKind, Usage,
    };
    use std::{
        collections::VecDeque,
        fmt,
        sync::{Arc, Mutex},
    };
    use tokio_util::sync::CancellationToken;
    use tracing::{
        Event, Level, Subscriber,
        field::{Field, Visit},
        metadata::{LevelFilter, Metadata},
        span::{Attributes, Id, Record},
    };

    fn request() -> ModelRequest {
        ModelRequest::new(
            ModelName::new("debug-model").expect("valid model name"),
            vec![
                ModelMessage::new(
                    ModelMessageRole::User,
                    ModelContent::text("Hello").expect("valid content"),
                )
                .expect("valid message"),
            ],
            Vec::new(),
            GenerationConfig::default(),
        )
        .expect("valid request")
    }

    #[derive(Debug, Clone)]
    struct CapturedTraceFields(Arc<Mutex<Vec<String>>>);

    impl CapturedTraceFields {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn joined(&self) -> String {
            self.0
                .lock()
                .expect("trace buffer should not be poisoned")
                .join(" ")
        }
    }

    struct CapturingSubscriber {
        fields: CapturedTraceFields,
    }

    impl CapturingSubscriber {
        fn new(fields: CapturedTraceFields) -> Self {
            Self { fields }
        }
    }

    impl Subscriber for CapturingSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() <= Level::DEBUG
        }

        fn max_level_hint(&self) -> Option<LevelFilter> {
            Some(LevelFilter::DEBUG)
        }

        fn new_span(&self, span: &Attributes<'_>) -> Id {
            let metadata = span.metadata();
            self.fields
                .0
                .lock()
                .expect("trace buffer should not be poisoned")
                .push(format!("span={:?}", metadata.name()));
            let mut visitor = TraceFieldVisitor::default();
            span.record(&mut visitor);
            self.fields
                .0
                .lock()
                .expect("trace buffer should not be poisoned")
                .extend(visitor.fields);
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, values: &Record<'_>) {
            let mut visitor = TraceFieldVisitor::default();
            values.record(&mut visitor);
            self.fields
                .0
                .lock()
                .expect("trace buffer should not be poisoned")
                .extend(visitor.fields);
        }

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut visitor = TraceFieldVisitor::default();
            event.record(&mut visitor);
            self.fields
                .0
                .lock()
                .expect("trace buffer should not be poisoned")
                .extend(visitor.fields);
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    #[derive(Default)]
    struct TraceFieldVisitor {
        fields: Vec<String>,
    }

    impl Visit for TraceFieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }

    fn capture_trace_fields<F>(operation: F) -> String
    where
        F: FnOnce(),
    {
        let fields = CapturedTraceFields::new();
        let subscriber = CapturingSubscriber::new(fields.clone());
        tracing::subscriber::with_default(subscriber, operation);
        fields.joined()
    }

    fn capture_stream_model_span_fields(
        config: OpenAiProviderConfig,
        request: ModelRequest,
    ) -> String {
        capture_trace_fields(|| {
            let provider = super::OpenAiProvider::new(config);
            let stream = provider.stream_model(request, ModelStreamContext::default());
            drop(stream);
        })
    }

    fn trace_rendered_request_fields(
        config: &OpenAiProviderConfig,
        request: &ModelRequest,
    ) -> String {
        capture_trace_fields(|| {
            let span = tracing::debug_span!(
                "test.provider.request",
                endpoint_path = tracing::field::Empty
            );
            let http_request = build_and_trace_responses_http_request(config, request, &span)
                .expect("request should build and trace");
            drop(http_request);
        })
    }

    #[test]
    fn builds_responses_http_request_without_network() {
        let config = OpenAiProviderConfig::new("sk-test")
            .expect("valid config")
            .with_base_url("https://api.example.test/v1/")
            .expect("valid base url")
            .with_organization("org-test")
            .expect("valid organization")
            .with_project("proj-test")
            .expect("valid project");

        let request =
            build_responses_http_request(&config, &request()).expect("request should build");

        assert_eq!(
            request.endpoint.as_str(),
            "https://api.example.test/v1/responses"
        );
        assert_eq!(request.header("Authorization"), Some("Bearer sk-test"));
        assert_eq!(request.header("Accept"), Some("text/event-stream"));
        assert_eq!(request.header("User-Agent"), Some("merry/0.1.0"));
        assert_eq!(request.header("OpenAI-Organization"), Some("org-test"));
        assert_eq!(request.header("OpenAI-Project"), Some("proj-test"));
        assert_eq!(request.body["model"], "debug-model");
        assert_eq!(request.body["stream"], true);
        assert_eq!(request.body["store"], false);
        assert_eq!(request.body["parallel_tool_calls"], false);
    }

    #[test]
    fn provider_trace_metadata_does_not_include_api_key_or_prompt_text() {
        let config = OpenAiProviderConfig::new("sk-secret-trace-key")
            .expect("valid config")
            .with_provider_name("openai-test")
            .expect("valid provider name");
        let request = ModelRequest::new(
            ModelName::new("trace-model").expect("valid model name"),
            vec![
                ModelMessage::new(
                    ModelMessageRole::User,
                    ModelContent::text("do not log this prompt text").expect("valid content"),
                )
                .expect("valid message"),
            ],
            Vec::new(),
            GenerationConfig::new(Some(128), false).expect("valid generation config"),
        )
        .expect("valid request");

        let fields = capture_trace_fields(|| {
            trace_openai_request_metadata(&config, &request, "/v1/responses");
        });

        assert!(fields.contains("event=\"runtime.provider.request\""));
        assert!(fields.contains("provider_name=\"openai-test\""));
        assert!(fields.contains("model=\"trace-model\""));
        assert!(fields.contains("message_count=1"));
        assert!(fields.contains("tool_count=0"));
        assert!(fields.contains("continuation_count=0"));
        assert!(fields.contains("max_output_tokens=128"));
        assert!(fields.contains("allow_parallel_tool_calls=false"));
        assert!(fields.contains("endpoint_path=\"/v1/responses\""));
        assert!(!fields.contains("sk-secret-trace-key"));
        assert!(!fields.contains("do not log this prompt text"));
    }

    #[test]
    fn provider_render_path_traces_request_metadata_without_api_key_or_prompt_text() {
        let config = OpenAiProviderConfig::new("sk-render-secret")
            .expect("valid config")
            .with_provider_name("openai-render-test")
            .expect("valid provider name")
            .with_base_url("https://api.example.test/v1")
            .expect("valid base url");
        let request = ModelRequest::new(
            ModelName::new("render-model").expect("valid model name"),
            vec![
                ModelMessage::new(
                    ModelMessageRole::User,
                    ModelContent::text("render prompt must not be logged").expect("valid content"),
                )
                .expect("valid message"),
            ],
            Vec::new(),
            GenerationConfig::new(Some(32), false).expect("valid generation config"),
        )
        .expect("valid request");

        let fields = trace_rendered_request_fields(&config, &request);

        assert!(fields.contains("event=\"runtime.provider.request\""));
        assert!(fields.contains("provider_name=\"openai-render-test\""));
        assert!(fields.contains("model=\"render-model\""));
        assert!(fields.contains("message_count=1"));
        assert!(fields.contains("tool_count=0"));
        assert!(fields.contains("continuation_count=0"));
        assert!(fields.contains("max_output_tokens=32"));
        assert!(fields.contains("allow_parallel_tool_calls=false"));
        assert!(fields.contains("endpoint_path=\"/v1/responses\""));
        assert!(!fields.contains("sk-render-secret"));
        assert!(!fields.contains("render prompt must not be logged"));
    }

    #[test]
    fn provider_stream_span_uses_runtime_request_metadata_fields_without_prompt_text() {
        let config = OpenAiProviderConfig::new("sk-span-secret")
            .expect("valid config")
            .with_provider_name("openai-span-test")
            .expect("valid provider name");
        let request = ModelRequest::new(
            ModelName::new("span-model").expect("valid model name"),
            vec![
                ModelMessage::new(
                    ModelMessageRole::User,
                    ModelContent::text("span prompt must not be logged").expect("valid content"),
                )
                .expect("valid message"),
            ],
            Vec::new(),
            GenerationConfig::new(Some(64), false).expect("valid generation config"),
        )
        .expect("valid request");

        let fields = capture_stream_model_span_fields(config, request);

        assert!(fields.contains("span=\"runtime.provider.stream\""));
        assert!(fields.contains("event=\"runtime.provider.stream\""));
        assert!(fields.contains("provider_name=\"openai-span-test\""));
        assert!(fields.contains("model=\"span-model\""));
        assert!(fields.contains("message_count=1"));
        assert!(fields.contains("tool_count=0"));
        assert!(fields.contains("continuation_count=0"));
        assert!(fields.contains("max_output_tokens="));
        assert!(fields.contains("allow_parallel_tool_calls=false"));
        assert!(!fields.contains("openai.stream_model"));
        assert!(!fields.contains("sk-span-secret"));
        assert!(!fields.contains("span prompt must not be logged"));
    }

    #[test]
    fn classifies_http_status_without_network() {
        assert_eq!(
            classify_http_status(reqwest::StatusCode::UNAUTHORIZED),
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            classify_http_status(reqwest::StatusCode::FORBIDDEN),
            ProviderErrorKind::Authentication
        );
        assert_eq!(
            classify_http_status(reqwest::StatusCode::TOO_MANY_REQUESTS),
            ProviderErrorKind::RateLimited
        );
        assert_eq!(
            classify_http_status(reqwest::StatusCode::BAD_GATEWAY),
            ProviderErrorKind::Unavailable
        );
        assert_eq!(
            classify_http_status(reqwest::StatusCode::BAD_REQUEST),
            ProviderErrorKind::Other
        );
    }

    #[test]
    fn parses_sse_lines_to_started_deltas_and_completed_without_network() {
        let mut parser = ResponsesStreamParser::new();
        let mut events = VecDeque::from([ModelEvent::Started]);

        for line in [
            "data: {\"type\":\"response.created\"}",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\" world\"}",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}]}],\"usage\":{\"input_tokens\":9,\"output_tokens\":3}}}",
            "data: [DONE]",
        ] {
            events.extend(
                parser
                    .parse_sse_line(line)
                    .expect("stream line should parse"),
            );
        }
        parser.finish().expect("stream should complete");

        assert_eq!(
            events.into_iter().collect::<Vec<_>>(),
            vec![
                ModelEvent::Started,
                ModelEvent::OutputTextDelta {
                    delta: "Hello".to_owned()
                },
                ModelEvent::OutputTextDelta {
                    delta: " world".to_owned()
                },
                ModelEvent::Completed {
                    response: ModelResponse::new(
                        vec![ModelOutput::text("Hello world")],
                        FinishReason::Stop,
                        Some(Usage::new(9, 3)),
                    )
                },
            ]
        );
    }

    #[test]
    fn stream_state_emits_completed_from_final_usage_line_without_trailing_newline() {
        let mut events = OpenAiEventStreamEvents::new();

        assert_eq!(events.pop_pending(), Some(ModelEvent::Started));
        events
            .parse_bytes(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\"}\n")
            .expect("text delta should parse");
        assert_eq!(
            events.pop_pending(),
            Some(ModelEvent::OutputTextDelta {
                delta: "Done".to_owned()
            })
        );
        events
            .parse_bytes(b"data: {\"type\":\"response.created\"}\n")
            .expect("created event should parse");
        events
            .parse_bytes(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Done\"}]}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}")
            .expect("unterminated completion line should buffer");

        events.finish_stream().expect("stream should finish");

        assert_eq!(
            events.pop_pending(),
            Some(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("Done")],
                    FinishReason::Stop,
                    Some(Usage::new(4, 1)),
                )
            })
        );
        assert_eq!(events.pop_pending(), None);
    }

    #[test]
    fn eof_finalization_returns_completed_from_final_usage_line_without_trailing_newline() {
        let mut events = OpenAiEventStreamEvents::new();
        assert_eq!(events.pop_pending(), Some(ModelEvent::Started));
        events
            .parse_bytes(b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Done\"}\n")
            .expect("text delta should parse");
        assert_eq!(
            events.pop_pending(),
            Some(ModelEvent::OutputTextDelta {
                delta: "Done".to_owned()
            })
        );
        events
            .parse_bytes(b"data: {\"type\":\"response.created\"}\n")
            .expect("created event should parse");
        events
            .parse_bytes(b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Done\"}]}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}")
            .expect("unterminated completion line should buffer");

        let event = events
            .finish_stream_and_pop_pending()
            .expect("EOF finalization should succeed");

        assert_eq!(
            event,
            Some(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("Done")],
                    FinishReason::Stop,
                    Some(Usage::new(4, 1)),
                )
            })
        );
        assert_eq!(events.pop_pending(), None);
    }

    #[tokio::test]
    async fn pre_cancelled_stream_setup_fails_before_network_request() {
        let token = CancellationToken::new();
        token.cancel();

        let config = OpenAiProviderConfig::new("sk-test")
            .expect("valid config")
            .with_base_url("https://api.example.test/v1")
            .expect("valid base URL");
        let error = super::OpenAiProvider::new(config)
            .stream_model(request(), ModelStreamContext::new(token))
            .await;
        let error = match error {
            Ok(_) => panic!("pre-cancelled setup should fail before sending"),
            Err(error) => error,
        };

        assert!(matches!(error, merry_llm::ModelError::Cancelled));
    }
}
