//! Provider implementation for the OpenAI-compatible adapter.

use crate::{OpenAiProviderConfig, OpenAiProviderError, parse::ChatCompletionStreamParser};
use futures_util::stream;
use merry_core::ProviderName;
use merry_llm::{
    ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelStreamContext, ProviderErrorKind,
};
use serde_json::Value;
use std::collections::VecDeque;

const AUTHORIZATION_HEADER: &str = "Authorization";
const ACCEPT_HEADER: &str = "Accept";
const SSE_ACCEPT_HEADER_VALUE: &str = "text/event-stream";
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
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            let http_request = build_chat_completion_http_request(&self.config, &request)?;
            let mut request_builder = self
                .client
                .post(http_request.endpoint)
                .json(&http_request.body);

            for header in http_request.headers {
                request_builder = request_builder.header(header.name, header.value);
            }

            let token = context.cancellation_token().clone();
            let response = tokio::select! {
                () = token.cancelled() => return Err(ModelError::Cancelled),
                response = request_builder.send() => response.map_err(map_transport_error)?,
            };

            if !response.status().is_success() {
                return Err(map_status_error(response, &token).await);
            }

            let event_stream = stream::unfold(
                OpenAiEventStreamState::new(response, token),
                |state| async move { state.next_item().await },
            );
            let event_stream: ModelEventStream = Box::pin(event_stream);
            Ok(event_stream)
        })
    }
}

struct OpenAiEventStreamState {
    response: reqwest::Response,
    events: OpenAiEventStreamEvents,
    cancellation_token: tokio_util::sync::CancellationToken,
    done: bool,
}

impl OpenAiEventStreamState {
    fn new(
        response: reqwest::Response,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            response,
            events: OpenAiEventStreamEvents::new(),
            cancellation_token,
            done: false,
        }
    }

    async fn next_item(mut self) -> Option<(Result<ModelEvent, ModelError>, Self)> {
        loop {
            if self.done {
                return None;
            }

            if self.cancellation_token.is_cancelled() {
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
                () = self.cancellation_token.cancelled() => {
                    self.done = true;
                    return Some((Err(ModelError::Cancelled), self));
                }
                chunk = self.response.chunk() => chunk,
            };

            match chunk {
                Ok(Some(chunk)) => {
                    if let Err(error) = self.events.parse_bytes(chunk.as_ref()) {
                        self.done = true;
                        return Some((Err(error.into()), self));
                    }
                }
                Ok(None) => match self.events.finish_stream_and_pop_pending() {
                    Ok(Some(event)) => {
                        if matches!(event, ModelEvent::Completed { .. }) {
                            self.done = true;
                        }
                        return Some((Ok(event), self));
                    }
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

struct OpenAiEventStreamEvents {
    parser: ChatCompletionStreamParser,
    line_buffer: Vec<u8>,
    pending: VecDeque<ModelEvent>,
}

impl OpenAiEventStreamEvents {
    fn new() -> Self {
        Self {
            parser: ChatCompletionStreamParser::new(),
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
struct ChatCompletionHttpRequest {
    endpoint: reqwest::Url,
    headers: Vec<ChatCompletionHttpHeader>,
    body: Value,
}

impl ChatCompletionHttpRequest {
    #[cfg(test)]
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatCompletionHttpHeader {
    name: &'static str,
    value: String,
}

fn build_chat_completion_http_request(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
) -> Result<ChatCompletionHttpRequest, ModelError> {
    let mut headers = vec![
        ChatCompletionHttpHeader {
            name: AUTHORIZATION_HEADER,
            value: format!("Bearer {}", config.api_key()),
        },
        ChatCompletionHttpHeader {
            name: ACCEPT_HEADER,
            value: SSE_ACCEPT_HEADER_VALUE.to_owned(),
        },
    ];
    if let Some(organization) = config.organization() {
        headers.push(ChatCompletionHttpHeader {
            name: OPENAI_ORGANIZATION_HEADER,
            value: organization.to_owned(),
        });
    }
    if let Some(project) = config.project() {
        headers.push(ChatCompletionHttpHeader {
            name: OPENAI_PROJECT_HEADER,
            value: project.to_owned(),
        });
    }

    Ok(ChatCompletionHttpRequest {
        endpoint: chat_completions_endpoint(config.base_url())?,
        headers,
        body: crate::render::render_chat_completion_request(request)?,
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
    response: reqwest::Response,
    cancellation_token: &tokio_util::sync::CancellationToken,
) -> ModelError {
    let status = response.status();
    let body = tokio::select! {
        () = cancellation_token.cancelled() => return ModelError::Cancelled,
        body = response.text() => body.unwrap_or_else(|error| {
            format!("failed to read provider error body: {error}")
        }),
    };
    let message = format!(
        "OpenAI Chat Completions request returned HTTP {status}: {}",
        truncate_for_error(body.trim())
    );

    ModelError::from(OpenAiProviderError::provider(
        classify_http_status(status),
        message,
    ))
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
        format!("OpenAI Chat Completions transport failed: {error}"),
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
        OpenAiEventStreamEvents, build_chat_completion_http_request, classify_http_status,
    };
    use crate::OpenAiProviderConfig;
    use crate::parse::ChatCompletionStreamParser;
    use merry_llm::{
        FinishReason, GenerationConfig, ModelContent, ModelEvent, ModelMessage, ModelMessageRole,
        ModelName, ModelOutput, ModelProvider, ModelRequest, ModelResponse, ModelStreamContext,
        ProviderErrorKind, Usage,
    };
    use std::collections::VecDeque;
    use tokio_util::sync::CancellationToken;

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

    #[test]
    fn builds_chat_completion_http_request_without_network() {
        let config = OpenAiProviderConfig::new("sk-test")
            .expect("valid config")
            .with_base_url("https://api.example.test/v1/")
            .expect("valid base url")
            .with_organization("org-test")
            .expect("valid organization")
            .with_project("proj-test")
            .expect("valid project");

        let request =
            build_chat_completion_http_request(&config, &request()).expect("request should build");

        assert_eq!(
            request.endpoint.as_str(),
            "https://api.example.test/v1/chat/completions"
        );
        assert_eq!(request.header("Authorization"), Some("Bearer sk-test"));
        assert_eq!(request.header("Accept"), Some("text/event-stream"));
        assert_eq!(request.header("OpenAI-Organization"), Some("org-test"));
        assert_eq!(request.header("OpenAI-Project"), Some("proj-test"));
        assert_eq!(request.body["model"], "debug-model");
        assert_eq!(request.body["stream"], true);
        assert_eq!(request.body["stream_options"]["include_usage"], true);
        assert_eq!(request.body["parallel_tool_calls"], false);
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
        let mut parser = ChatCompletionStreamParser::new();
        let mut events = VecDeque::from([ModelEvent::Started]);

        for line in [
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}],\"usage\":null}",
            "data: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}],\"usage\":null}",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":null}",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}",
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
            .parse_bytes(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"Done\"},\"finish_reason\":null}],\"usage\":null}\n",
            )
            .expect("text delta should parse");
        assert_eq!(
            events.pop_pending(),
            Some(ModelEvent::OutputTextDelta {
                delta: "Done".to_owned()
            })
        );
        events
            .parse_bytes(
                b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":null}\n",
            )
            .expect("finish reason should parse");
        events
            .parse_bytes(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1,\"total_tokens\":5}}")
            .expect("unterminated final usage line should buffer");

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
            .parse_bytes(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"Done\"},\"finish_reason\":null}],\"usage\":null}\n",
            )
            .expect("text delta should parse");
        assert_eq!(
            events.pop_pending(),
            Some(ModelEvent::OutputTextDelta {
                delta: "Done".to_owned()
            })
        );
        events
            .parse_bytes(
                b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":null}\n",
            )
            .expect("finish reason should parse");
        events
            .parse_bytes(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1,\"total_tokens\":5}}")
            .expect("unterminated final usage line should buffer");

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
