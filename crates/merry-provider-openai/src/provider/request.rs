use crate::{OpenAiProtocol, OpenAiProviderConfig, OpenAiProviderError};
use merry_llm::{ModelError, ModelRequest, ModelStreamContext};
use serde_json::Value;

const AUTHORIZATION_HEADER: &str = "Authorization";
const ACCEPT_HEADER: &str = "Accept";
const SSE_ACCEPT_HEADER_VALUE: &str = "text/event-stream";
const USER_AGENT_HEADER: &str = "User-Agent";
const USER_AGENT_HEADER_VALUE: &str = concat!("merry/", env!("CARGO_PKG_VERSION"));
const OPENAI_ORGANIZATION_HEADER: &str = "OpenAI-Organization";
const OPENAI_PROJECT_HEADER: &str = "OpenAI-Project";

#[derive(Debug, Clone, PartialEq)]
pub(super) struct OpenAiHttpRequest {
    pub(super) endpoint: reqwest::Url,
    pub(super) headers: Vec<OpenAiHttpHeader>,
    pub(super) body: Value,
}

impl OpenAiHttpRequest {
    #[cfg(test)]
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenAiHttpHeader {
    pub(super) name: &'static str,
    pub(super) value: String,
}

fn request_headers(config: &OpenAiProviderConfig) -> Vec<OpenAiHttpHeader> {
    let mut headers = vec![
        OpenAiHttpHeader {
            name: AUTHORIZATION_HEADER,
            value: format!("Bearer {}", config.api_key()),
        },
        OpenAiHttpHeader {
            name: ACCEPT_HEADER,
            value: SSE_ACCEPT_HEADER_VALUE.to_owned(),
        },
        OpenAiHttpHeader {
            name: USER_AGENT_HEADER,
            value: USER_AGENT_HEADER_VALUE.to_owned(),
        },
    ];
    if let Some(organization) = config.organization() {
        headers.push(OpenAiHttpHeader {
            name: OPENAI_ORGANIZATION_HEADER,
            value: organization.to_owned(),
        });
    }
    if let Some(project) = config.project() {
        headers.push(OpenAiHttpHeader {
            name: OPENAI_PROJECT_HEADER,
            value: project.to_owned(),
        });
    }

    headers
}

pub(super) fn build_openai_http_request(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
    context: &ModelStreamContext,
) -> Result<OpenAiHttpRequest, ModelError> {
    let (endpoint, body) = match config.protocol() {
        OpenAiProtocol::Responses => (
            responses_endpoint(config.base_url())?,
            crate::render::render_responses_request_with_prompt_cache_key(
                request,
                context
                    .prompt_cache_key()
                    .map(merry_core::SessionId::as_str),
            )?,
        ),
        OpenAiProtocol::ChatCompletions => (
            chat_completions_endpoint(config.base_url())?,
            crate::chat_completions::render::render_chat_request(request)?,
        ),
    };
    Ok(OpenAiHttpRequest {
        endpoint,
        headers: request_headers(config),
        body,
    })
}

pub(super) fn build_and_trace_openai_http_request(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
    context: &ModelStreamContext,
    span: &tracing::Span,
) -> Result<OpenAiHttpRequest, ModelError> {
    let http_request = build_openai_http_request(config, request, context)?;
    span.record("endpoint_path", http_request.endpoint.path());
    trace_openai_request_metadata(config, request, http_request.endpoint.path());
    Ok(http_request)
}

pub(super) fn trace_openai_request_metadata(
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
