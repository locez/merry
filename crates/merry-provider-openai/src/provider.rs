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

mod request;
use request::build_and_trace_openai_http_request;

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
mod tests;
