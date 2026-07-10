use crate::{
    AnthropicProviderConfig, AnthropicProviderError, parse::AnthropicStreamParser,
    render::render_anthropic_request,
};
use futures_util::stream;
use merry_core::ProviderName;
use merry_llm::{
    ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelStreamContext, ProviderErrorKind,
};
use serde_json::Value;
use std::{collections::VecDeque, time::Duration};
use tracing::Instrument;

const USER_AGENT_VALUE: &str = concat!("merry/", env!("CARGO_PKG_VERSION"));

/// Config-backed Anthropic Messages provider.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    config: AnthropicProviderConfig,
    pub(crate) client: reqwest::Client,
}

impl AnthropicProvider {
    #[must_use]
    pub fn new(config: AnthropicProviderConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    #[must_use]
    pub fn config(&self) -> &AnthropicProviderConfig {
        &self.config
    }
}

impl ModelProvider for AnthropicProvider {
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
        let span = tracing::debug_span!(
            "runtime.provider.stream",
            event = "runtime.provider.stream",
            provider_name = self.config.provider_name().as_str(),
            model = request.model().as_str(),
            message_count = request.messages().len(),
            tool_count = request.tools().len(),
            continuation_count = request.continuations().len(),
            max_output_tokens = request.generation().max_output_tokens(),
            allow_parallel_tool_calls = request.generation().allow_parallel_tool_calls(),
            endpoint_path = "/v1/messages",
        );
        let stream_span = span.clone();
        Box::pin(
            async move {
                if context.cancellation_token().is_cancelled() {
                    return Err(ModelError::Cancelled);
                }
                let endpoint = messages_endpoint(self.config.base_url())?;
                let body = render_anthropic_request(&self.config, &request)?;
                tracing::debug!(
                    event = "runtime.provider.request",
                    provider_name = self.config.provider_name().as_str(),
                    model = request.model().as_str(),
                    message_count = request.messages().len(),
                    tool_count = request.tools().len(),
                    continuation_count = request.continuations().len(),
                    endpoint_path = endpoint.path(),
                    "runtime provider request metadata"
                );
                let request_builder = self
                    .client
                    .post(endpoint)
                    .header("x-api-key", self.config.api_key())
                    .header("anthropic-version", self.config.api_version())
                    .header(reqwest::header::ACCEPT, "text/event-stream")
                    .header(reqwest::header::USER_AGENT, USER_AGENT_VALUE)
                    .json(&body);
                let token = context.cancellation_token().clone();
                let response = tokio::select! {
                    () = token.cancelled() => return Err(ModelError::Cancelled),
                    response = request_builder.send() => response.map_err(map_transport_error)?,
                };
                let status = response.status();
                if !status.is_success() {
                    let kind = classify_http_status(status);
                    return Err(map_status_error(response, &token, kind).await);
                }
                let event_stream = stream::unfold(
                    AnthropicEventStreamState::new(response, token, stream_span),
                    |state| async move { state.next_item().await },
                );
                Ok(Box::pin(event_stream) as ModelEventStream)
            }
            .instrument(span),
        )
    }
}

struct AnthropicEventStreamState {
    response: reqwest::Response,
    events: AnthropicEventStreamEvents,
    token: tokio_util::sync::CancellationToken,
    span: tracing::Span,
    done: bool,
}

impl AnthropicEventStreamState {
    fn new(
        response: reqwest::Response,
        token: tokio_util::sync::CancellationToken,
        span: tracing::Span,
    ) -> Self {
        Self {
            response,
            events: AnthropicEventStreamEvents::new(),
            token,
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
            if self.token.is_cancelled() {
                self.done = true;
                return Some((Err(ModelError::Cancelled), self));
            }
            if let Some(event) = self.events.pop_pending() {
                if matches!(event, ModelEvent::Completed { .. }) {
                    self.done = true;
                }
                return Some((Ok(event), self));
            }
            let chunk = tokio::select! {
                () = self.token.cancelled() => {
                    self.done = true;
                    return Some((Err(ModelError::Cancelled), self));
                }
                chunk = self.response.chunk() => chunk,
            };
            match chunk {
                Ok(Some(chunk)) => {
                    if let Err(error) = self.events.parse_bytes(&chunk) {
                        self.done = true;
                        return Some((Err(error.into()), self));
                    }
                }
                Ok(None) => match self.events.finish_stream_and_pop_pending() {
                    Ok(Some(event)) => return Some((Ok(event), self)),
                    Ok(None) => {
                        self.done = true;
                        return None;
                    }
                    Err(error) => {
                        self.done = true;
                        return Some((Err(error.into()), self));
                    }
                },
                Err(error) => {
                    self.done = true;
                    return Some((Err(map_transport_error(error)), self));
                }
            }
        }
    }
}

struct AnthropicEventStreamEvents {
    parser: AnthropicStreamParser,
    line_buffer: Vec<u8>,
    pending: VecDeque<ModelEvent>,
}

impl AnthropicEventStreamEvents {
    fn new() -> Self {
        Self {
            parser: AnthropicStreamParser::new(),
            line_buffer: Vec::new(),
            pending: VecDeque::from([ModelEvent::Started]),
        }
    }

    fn pop_pending(&mut self) -> Option<ModelEvent> {
        self.pending.pop_front()
    }

    fn parse_bytes(&mut self, bytes: &[u8]) -> Result<(), AnthropicProviderError> {
        for byte in bytes {
            self.line_buffer.push(*byte);
            if *byte == b'\n' {
                self.parse_buffered_line()?;
            }
        }
        Ok(())
    }

    fn parse_buffered_line(&mut self) -> Result<(), AnthropicProviderError> {
        let line = std::str::from_utf8(&self.line_buffer).map_err(|error| {
            AnthropicProviderError::protocol(format!("stream line is not UTF-8: {error}"))
        })?;
        self.pending.extend(self.parser.parse_sse_line(line)?);
        self.line_buffer.clear();
        Ok(())
    }

    fn finish_stream_and_pop_pending(
        &mut self,
    ) -> Result<Option<ModelEvent>, AnthropicProviderError> {
        if !self.line_buffer.is_empty() {
            self.parse_buffered_line()?;
        }
        self.parser.finish()?;
        Ok(self.pop_pending())
    }
}

fn messages_endpoint(base_url: &str) -> Result<reqwest::Url, ModelError> {
    let endpoint = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    reqwest::Url::parse(&endpoint).map_err(|error| {
        AnthropicProviderError::invalid_config(format!(
            "base_url does not form a valid Messages endpoint: {error}"
        ))
        .into()
    })
}

async fn map_status_error(
    mut response: reqwest::Response,
    token: &tokio_util::sync::CancellationToken,
    kind: ProviderErrorKind,
) -> ModelError {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs);
    let request_id = response
        .headers()
        .get("request-id")
        .or_else(|| response.headers().get("x-request-id"))
        .and_then(|value| value.to_str().ok())
        .and_then(bounded_metadata);
    let mut body = Vec::new();
    while body.len() < 8 * 1024 {
        let chunk = tokio::select! {
            () = token.cancelled() => return ModelError::Cancelled,
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
    let error_type = anthropic_error_type(&body);
    let mut message = format!("Anthropic Messages request returned HTTP {status}");
    if let Some(error_type) = error_type {
        message.push_str(&format!(" (type: {error_type})"));
    }
    if let Some(request_id) = request_id {
        message.push_str(&format!(" (request_id: {request_id})"));
    }
    AnthropicProviderError::provider_with_retry_after(kind, message, retry_after).into()
}

fn anthropic_error_type(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("error")?
        .get("type")?
        .as_str()
        .and_then(bounded_metadata)
}

fn bounded_metadata(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-:/".contains(character)))
    .then(|| value.to_owned())
}

fn classify_http_status(status: reqwest::StatusCode) -> ProviderErrorKind {
    match status.as_u16() {
        400 => ProviderErrorKind::InvalidRequest,
        401 | 403 => ProviderErrorKind::Authentication,
        429 => ProviderErrorKind::RateLimited,
        500..=599 => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::Other,
    }
}

fn map_transport_error(error: reqwest::Error) -> ModelError {
    AnthropicProviderError::provider_with_retry_after(
        ProviderErrorKind::Unavailable,
        format!("Anthropic Messages transport failed: {error}"),
        None,
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_and_error_metadata_are_bounded() {
        assert_eq!(
            messages_endpoint("https://api.example.test")
                .expect("valid endpoint")
                .as_str(),
            "https://api.example.test/v1/messages"
        );
        assert_eq!(
            anthropic_error_type(
                br#"{"error":{"type":"invalid_request_error","message":"secret prompt"}}"#
            )
            .as_deref(),
            Some("invalid_request_error")
        );
    }
}
