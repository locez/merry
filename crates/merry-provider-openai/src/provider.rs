//! Provider implementation for the OpenAI Responses adapter.

use crate::{
    OpenAiProtocol, OpenAiProviderConfig, OpenAiProviderError,
    chat_completions::parse::ChatStreamParser, parse::ResponsesStreamParser,
};
use futures_util::stream;
use merry_core::ProviderName;
use merry_llm::{
    ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelStreamContext, ProviderErrorKind,
};
use serde_json::Value;
use std::collections::VecDeque;
use std::time::Duration;
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
    pub(crate) client: reqwest::Client,
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

                let http_request = build_and_trace_openai_http_request(
                    &self.config,
                    &request,
                    &context,
                    &event_stream_span,
                )?;
                tracing::trace!(event = "runtime.provider.request.rendered");
                let endpoint_host = bounded_endpoint_host(&http_request.endpoint);
                let protocol = self.config.protocol();

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
                    response = request_builder.send() => response.map_err(|error| {
                        map_transport_error(error, protocol, endpoint_host.as_deref())
                    })?,
                };

                let status = response.status();
                if status.is_success() {
                    tracing::debug!("openai http status received and classified");
                } else {
                    let error_kind = classify_http_status(status);
                    tracing::debug!("openai http status received and classified");
                    let error = map_status_error(
                        response,
                        &token,
                        error_kind,
                        self.config.protocol(),
                        self.config.provider_name().as_str(),
                    )
                    .await;
                    if matches!(error, ModelError::Cancelled) {
                        tracing::debug!("openai stream setup cancelled");
                    }
                    return Err(error);
                }

                let event_stream = stream::unfold(
                    OpenAiEventStreamState::new(
                        response,
                        token,
                        event_stream_span,
                        self.config.protocol(),
                    ),
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
    endpoint_host: Option<String>,
    protocol: OpenAiProtocol,
    done: bool,
}

impl OpenAiEventStreamState {
    fn new(
        response: reqwest::Response,
        cancellation_token: tokio_util::sync::CancellationToken,
        span: tracing::Span,
        protocol: OpenAiProtocol,
    ) -> Self {
        let endpoint_host = bounded_endpoint_host(response.url());
        Self {
            response,
            events: OpenAiEventStreamEvents::new(protocol),
            cancellation_token,
            span,
            endpoint_host,
            protocol,
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
                        return Some((
                            Err(add_stream_endpoint_context(
                                error.into(),
                                self.protocol,
                                self.endpoint_host.as_deref(),
                            )),
                            self,
                        ));
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
                        return Some((
                            Err(add_stream_endpoint_context(
                                error.into(),
                                self.protocol,
                                self.endpoint_host.as_deref(),
                            )),
                            self,
                        ));
                    }
                },
                Err(error) => {
                    tracing::debug!("openai stream transport error");
                    self.done = true;
                    return Some((
                        Err(map_transport_error(
                            error,
                            self.protocol,
                            self.endpoint_host.as_deref(),
                        )),
                        self,
                    ));
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
    parser: OpenAiStreamParser,
    line_buffer: Vec<u8>,
    pending: VecDeque<ModelEvent>,
}

impl OpenAiEventStreamEvents {
    fn new(protocol: OpenAiProtocol) -> Self {
        Self {
            parser: match protocol {
                OpenAiProtocol::Responses => {
                    OpenAiStreamParser::Responses(ResponsesStreamParser::new())
                }
                OpenAiProtocol::ChatCompletions => {
                    OpenAiStreamParser::ChatCompletions(ChatStreamParser::new())
                }
            },
            line_buffer: Vec::new(),
            pending: VecDeque::from([ModelEvent::Started]),
        }
    }

    fn pop_pending(&mut self) -> Option<ModelEvent> {
        self.pending.pop_front()
    }

    fn parse_bytes(&mut self, bytes: &[u8]) -> Result<(), OpenAiProviderError> {
        for (byte_offset, byte) in bytes.iter().enumerate() {
            self.line_buffer.push(*byte);
            if *byte == b'\n'
                && let Err(error) = self.parse_buffered_line()
            {
                tracing::debug!(
                    chunk_byte_length = bytes.len(),
                    chunk_byte_offset = byte_offset,
                    buffered_line_byte_length = self.line_buffer.len(),
                    "openai stream SSE line rejected"
                );
                return Err(error);
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

enum OpenAiStreamParser {
    Responses(ResponsesStreamParser),
    ChatCompletions(ChatStreamParser),
}

impl OpenAiStreamParser {
    fn parse_sse_line(&mut self, line: &str) -> Result<Vec<ModelEvent>, OpenAiProviderError> {
        match self {
            Self::Responses(parser) => parser.parse_sse_line(line),
            Self::ChatCompletions(parser) => parser.parse_sse_line(line),
        }
    }

    fn finish(&self) -> Result<(), OpenAiProviderError> {
        match self {
            Self::Responses(parser) => parser.finish(),
            Self::ChatCompletions(parser) => parser.finish(),
        }
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
    context: &ModelStreamContext,
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
        body: crate::render::render_responses_request_with_prompt_cache_key(
            request,
            context
                .prompt_cache_key()
                .map(merry_core::SessionId::as_str),
        )?,
    })
}

fn build_openai_http_request(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
    context: &ModelStreamContext,
) -> Result<ResponsesHttpRequest, ModelError> {
    match config.protocol() {
        OpenAiProtocol::Responses => build_responses_http_request(config, request, context),
        OpenAiProtocol::ChatCompletions => {
            let mut http = build_responses_http_request(config, request, context)?;
            http.endpoint = chat_completions_endpoint(config.base_url())?;
            http.body = crate::chat_completions::render::render_chat_request(request)?;
            Ok(http)
        }
    }
}

fn build_and_trace_openai_http_request(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
    context: &ModelStreamContext,
    span: &tracing::Span,
) -> Result<ResponsesHttpRequest, ModelError> {
    let http_request = build_openai_http_request(config, request, context)?;
    span.record("endpoint_path", http_request.endpoint.path());
    trace_openai_request_metadata(config, request, http_request.endpoint.path());
    Ok(http_request)
}

#[cfg(test)]
fn build_and_trace_responses_http_request(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
    context: &ModelStreamContext,
    span: &tracing::Span,
) -> Result<ResponsesHttpRequest, ModelError> {
    let http_request = build_responses_http_request(config, request, context)?;
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

fn chat_completions_endpoint(base_url: &str) -> Result<reqwest::Url, ModelError> {
    let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    reqwest::Url::parse(&endpoint).map_err(|error| {
        OpenAiProviderError::invalid_config(format!(
            "base_url does not form a valid Chat Completions endpoint: {error}"
        ))
        .into()
    })
}

async fn map_status_error(
    mut response: reqwest::Response,
    cancellation_token: &tokio_util::sync::CancellationToken,
    kind: ProviderErrorKind,
    protocol: OpenAiProtocol,
    provider_name: &str,
) -> ModelError {
    let status = response.status();
    let request_host = bounded_endpoint_host(response.url());
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(parse_retry_after_header);
    let request_id = ["x-request-id", "request-id", "openai-request-id"]
        .into_iter()
        .find_map(|name| response.headers().get(name))
        .and_then(|value| value.to_str().ok())
        .and_then(bounded_provider_metadata);
    let mut body = Vec::new();
    while body.len() < 8 * 1024 {
        let chunk = tokio::select! {
            () = cancellation_token.cancelled() => return ModelError::Cancelled,
            chunk = response.chunk() => chunk,
        };
        match chunk {
            Ok(Some(chunk)) => {
                let remaining = 8 * 1024 - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    let details = provider_error_details(&body);
    let protocol_name = match protocol {
        OpenAiProtocol::Responses => "Responses",
        OpenAiProtocol::ChatCompletions => "Chat Completions",
    };
    tracing::warn!(
        event = "runtime.provider.http_error",
        provider_name,
        protocol = protocol_name,
        request_host = request_host.as_deref().unwrap_or(""),
        http_status = status.as_u16(),
        error_type = details
            .as_ref()
            .and_then(|details| details.error_type.as_deref())
            .unwrap_or(""),
        error_code = details
            .as_ref()
            .and_then(|details| details.code.as_deref())
            .unwrap_or(""),
        error_param = details
            .as_ref()
            .and_then(|details| details.param.as_deref())
            .unwrap_or(""),
        error_message = details
            .as_ref()
            .and_then(|details| details.message.as_deref())
            .unwrap_or(""),
        request_id = request_id.as_deref().unwrap_or(""),
        "provider returned an HTTP error"
    );
    let message = format_provider_error_message(
        protocol_name,
        status,
        request_host.as_deref(),
        details.as_ref(),
        request_id.as_deref(),
    );

    ModelError::from(OpenAiProviderError::provider_with_retry_after(
        kind,
        message,
        retry_after,
    ))
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
        400..=499 => ProviderErrorKind::InvalidRequest,
        500..=599 => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::Other,
    }
}

fn map_transport_error(
    error: reqwest::Error,
    protocol: OpenAiProtocol,
    request_host: Option<&str>,
) -> ModelError {
    let protocol_name = openai_protocol_name(protocol);
    let message = request_host.map_or_else(
        || format!("OpenAI {protocol_name} transport failed: {error}"),
        |host| {
            format!(
                "OpenAI {protocol_name} request to host {host} failed during transport: {error}"
            )
        },
    );
    ModelError::from(OpenAiProviderError::provider(
        ProviderErrorKind::Unavailable,
        message,
    ))
}

fn add_stream_endpoint_context(
    error: ModelError,
    protocol: OpenAiProtocol,
    request_host: Option<&str>,
) -> ModelError {
    let Some(host) = request_host else {
        return error;
    };
    let prefix = format!(
        "OpenAI {} stream from host {host}",
        openai_protocol_name(protocol)
    );
    match error {
        ModelError::InvalidRequest { reason } => {
            ModelError::invalid_request(format!("{prefix} failed: {reason}"))
        }
        ModelError::Provider {
            kind,
            message,
            retry_after,
        } => ModelError::provider_with_retry_after(
            kind,
            format!("{prefix} failed: {message}"),
            retry_after,
        ),
        ModelError::Cancelled => ModelError::Cancelled,
    }
}

fn openai_protocol_name(protocol: OpenAiProtocol) -> &'static str {
    match protocol {
        OpenAiProtocol::Responses => "Responses",
        OpenAiProtocol::ChatCompletions => "Chat Completions",
    }
}

fn parse_retry_after_header(value: &reqwest::header::HeaderValue) -> Option<Duration> {
    let seconds = value.to_str().ok()?.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds))
}

#[derive(Debug, Default)]
struct ProviderErrorDetails {
    error_type: Option<String>,
    code: Option<String>,
    param: Option<String>,
    message: Option<String>,
}

fn provider_error_details(body: &[u8]) -> Option<ProviderErrorDetails> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let error = value.get("error")?;
    let details = ProviderErrorDetails {
        error_type: error
            .get("type")
            .and_then(Value::as_str)
            .and_then(bounded_provider_metadata),
        code: error
            .get("code")
            .and_then(Value::as_str)
            .and_then(bounded_provider_metadata),
        param: error
            .get("param")
            .and_then(Value::as_str)
            .and_then(bounded_provider_metadata),
        message: error
            .get("message")
            .and_then(Value::as_str)
            .and_then(bounded_provider_error_message),
    };
    (details.error_type.is_some()
        || details.code.is_some()
        || details.param.is_some()
        || details.message.is_some())
    .then_some(details)
}

fn format_provider_error_message(
    protocol_name: &str,
    status: reqwest::StatusCode,
    request_host: Option<&str>,
    details: Option<&ProviderErrorDetails>,
    request_id: Option<&str>,
) -> String {
    let mut message = request_host.map_or_else(
        || format!("OpenAI {protocol_name} request returned HTTP {status}"),
        |host| format!("OpenAI {protocol_name} request to host {host} returned HTTP {status}"),
    );
    if let Some(details) = details {
        if let Some(error_type) = details.error_type.as_deref() {
            message.push_str(&format!(" (type: {error_type})"));
        }
        if let Some(code) = details.code.as_deref() {
            message.push_str(&format!(" (code: {code})"));
        }
        if let Some(param) = details.param.as_deref() {
            message.push_str(&format!(" (param: {param})"));
        }
        if let Some(provider_message) = details.message.as_deref() {
            message.push_str(&format!(" (server error: {provider_message})"));
        }
    }
    if let Some(request_id) = request_id {
        message.push_str(&format!(" (request_id: {request_id})"));
    }
    message
}

fn bounded_endpoint_host(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    let value = match url.port() {
        Some(port) if host.contains(':') => format!("[{host}]:{port}"),
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    (!value.is_empty() && value.chars().count() <= 256).then_some(value)
}

pub(crate) fn bounded_provider_error_message(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    if [
        "sk-",
        "rk-",
        "bearer ",
        "api_key",
        "apikey",
        "access_token",
        "password",
        "secret",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return None;
    }

    let sanitized: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let message = sanitized.trim();
    (!message.is_empty()).then(|| message.chars().take(1_024).collect())
}

pub(crate) fn bounded_provider_metadata(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-:/".contains(character)))
    .then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        OpenAiEventStreamEvents, build_and_trace_responses_http_request, build_openai_http_request,
        build_responses_http_request, classify_http_status, format_provider_error_message,
        trace_openai_request_metadata,
    };
    use crate::parse::ResponsesStreamParser;
    use crate::{OpenAiProtocol, OpenAiProviderConfig, OpenAiProviderError};
    use merry_core::SessionId;
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
            let context = ModelStreamContext::default();
            let span = tracing::debug_span!(
                "test.provider.request",
                endpoint_path = tracing::field::Empty
            );
            let http_request =
                build_and_trace_responses_http_request(config, request, &context, &span)
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

        let context = ModelStreamContext::default();
        let request = build_responses_http_request(&config, &request(), &context)
            .expect("request should build");

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
    fn builds_responses_http_request_with_prompt_cache_key_from_stream_context() {
        let config = OpenAiProviderConfig::new("sk-test").expect("valid config");
        let context = ModelStreamContext::default()
            .with_prompt_cache_key(SessionId::new("cache-session").expect("valid session id"));

        let request = build_responses_http_request(&config, &request(), &context)
            .expect("request should build");

        assert_eq!(request.body["prompt_cache_key"], "cache-session");
    }

    #[test]
    fn builds_chat_completions_http_request_without_responses_state() {
        let config = OpenAiProviderConfig::new("sk-test")
            .expect("valid config")
            .with_base_url("https://api.example.test/v1")
            .expect("valid base url")
            .with_protocol(OpenAiProtocol::ChatCompletions);
        let request =
            build_openai_http_request(&config, &request(), &ModelStreamContext::default())
                .expect("request should build");

        assert_eq!(
            request.endpoint.as_str(),
            "https://api.example.test/v1/chat/completions"
        );
        assert_eq!(request.body["model"], "debug-model");
        assert_eq!(request.body["stream"], true);
        assert_eq!(request.body["stream_options"]["include_usage"], true);
        assert!(request.body.get("store").is_none());
        assert!(request.body.get("prompt_cache_key").is_none());
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
            ProviderErrorKind::InvalidRequest
        );
    }

    #[test]
    fn provider_error_metadata_does_not_expose_error_body_content() {
        let body = br#"{
            "error": {
                "code": "invalid_request_error",
                "message": "prompt secret sk-test-sensitive user request"
            }
        }"#;

        let details = super::provider_error_details(body).expect("error metadata should parse");
        assert_eq!(details.code.as_deref(), Some("invalid_request_error"));
        let message = format_provider_error_message(
            "Responses",
            reqwest::StatusCode::BAD_REQUEST,
            None,
            Some(&details),
            None,
        );
        assert!(!message.contains("prompt secret"));
        assert!(!message.contains("sk-test"));
        assert!(super::bounded_provider_metadata("req_abc-123").is_some());
        assert!(super::bounded_provider_metadata("secret value with spaces").is_none());
    }

    #[test]
    fn provider_error_message_preserves_safe_server_details() {
        let body = br#"{
            "error": {
                "type": "invalid_request_error",
                "code": "invalid_json_schema",
                "param": "text.format.schema",
                "message": "Invalid schema for response_format 'compacted_checkpoint_candidate': Missing 'rationale'."
            }
        }"#;
        let details = super::provider_error_details(body).expect("error details should parse");
        let message = format_provider_error_message(
            "Responses",
            reqwest::StatusCode::BAD_REQUEST,
            Some("api.example.test:443"),
            Some(&details),
            Some("req_abc-123"),
        );

        assert!(message.contains("HTTP 400"));
        assert!(message.contains("host api.example.test:443"));
        assert!(message.contains("type: invalid_request_error"));
        assert!(message.contains("code: invalid_json_schema"));
        assert!(message.contains("param: text.format.schema"));
        assert!(message.contains("server error: Invalid schema for response_format"));
        assert!(message.contains("Missing 'rationale'."));
        assert!(message.contains("request_id: req_abc-123"));
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
    fn parser_accepts_standard_sse_metadata_and_data_without_space() {
        let mut parser = ResponsesStreamParser::new();
        let mut events = VecDeque::from([ModelEvent::Started]);

        for line in [
            ": provider heartbeat",
            "event: response.output_text.delta",
            "id: stream-1",
            "retry: 1000",
            "data:{\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}",
            "data:{\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\"}]}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}",
        ] {
            events.extend(
                parser
                    .parse_sse_line(line)
                    .expect("standard SSE line should parse or be ignored"),
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
                ModelEvent::Completed {
                    response: ModelResponse::new(
                        vec![ModelOutput::text("Hello")],
                        FinishReason::Stop,
                        Some(Usage::new(4, 1)),
                    )
                },
            ]
        );
    }

    #[test]
    fn parser_reports_unexpected_non_sse_stream_lines() {
        let mut parser = ResponsesStreamParser::new();

        let error = parser
            .parse_sse_line(r#"{"error":"bad upstream"}"#)
            .expect_err("non-SSE line should fail");

        assert!(matches!(error, OpenAiProviderError::Protocol { .. }));
        assert!(
            error
                .to_string()
                .contains("unexpected Responses stream line")
        );
        assert!(error.to_string().contains("expected an SSE `data:` field"));
    }

    #[test]
    fn responses_stream_error_preserves_safe_server_message() {
        let mut parser = ResponsesStreamParser::new();
        let error = parser
            .parse_sse_line(
                r#"data: {"type":"error","code":"invalid_request_error","message":"response schema is invalid"}"#,
            )
            .expect_err("provider stream error should be surfaced");

        assert!(error.to_string().contains("invalid_request_error"));
        assert!(error.to_string().contains("response schema is invalid"));
    }

    #[test]
    fn stream_state_emits_completed_from_final_usage_line_without_trailing_newline() {
        let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::Responses);

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
        let mut events = OpenAiEventStreamEvents::new(OpenAiProtocol::Responses);
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
