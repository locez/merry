use super::model_request;
use crate::provider::request::{
    build_and_trace_openai_http_request, build_openai_http_request, trace_openai_request_metadata,
};
use crate::{OpenAiProtocol, OpenAiProviderConfig};
use merry_core::SessionId;
use merry_llm::{GenerationConfig, ModelContent, ModelMessage, ModelMessageRole, ModelName};
use merry_llm::{ModelProvider, ModelRequest, ModelStreamContext};
use std::{
    fmt,
    sync::{Arc, Mutex},
};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
    metadata::{LevelFilter, Metadata},
    span::{Attributes, Id, Record},
};

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

fn capture_stream_model_span_fields(config: OpenAiProviderConfig, request: ModelRequest) -> String {
    capture_trace_fields(|| {
        let provider = crate::provider::OpenAiProvider::new(config);
        let stream = provider.stream_model(request, ModelStreamContext::default());
        drop(stream);
    })
}

fn trace_rendered_request_fields(config: &OpenAiProviderConfig, request: &ModelRequest) -> String {
    capture_trace_fields(|| {
        let context = ModelStreamContext::default();
        let span = tracing::debug_span!(
            "test.provider.request",
            endpoint_path = tracing::field::Empty
        );
        let http_request = build_and_trace_openai_http_request(config, request, &context, &span)
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
    let request = build_openai_http_request(&config, &model_request(), &context)
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

    let request = build_openai_http_request(&config, &model_request(), &context)
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
        build_openai_http_request(&config, &model_request(), &ModelStreamContext::default())
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
