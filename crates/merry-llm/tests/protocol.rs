use futures_executor::block_on;
use futures_util::StreamExt;
use merry_core::{ProviderName, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelContent, ModelError, ModelEvent,
    ModelEventStream, ModelMessage, ModelMessageRole, ModelName, ModelOutput, ModelProvider,
    ModelProviderFuture, ModelRequest, ModelResponse, ModelStreamContext, ModelToolCall,
    ModelToolCallId, ToolArguments, Usage,
};
use schemars::{JsonSchema, Schema};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{sync::Arc, task::Poll};

fn assert_json_round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_string(value).expect("value should serialize");
    let decoded = serde_json::from_str::<T>(&encoded).expect("value should deserialize");
    assert_eq!(&decoded, value);
}

fn assert_schema_compiles<T: JsonSchema>() {
    let _schema = schemars::schema_for!(T);
}

fn object_schema() -> ToolInputSchema {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "city": { "type": "string" }
        },
        "required": ["city"]
    }))
    .expect("test schema should be a JSON schema");

    ToolInputSchema::new(schema).expect("object schema should be valid")
}

fn weather_tool() -> ToolSpec {
    ToolSpec::new(
        ToolName::new("lookup_weather").expect("valid tool name"),
        "Look up weather for a city",
        object_schema(),
    )
    .expect("valid tool spec")
}

fn user_message(text: &str) -> ModelMessage {
    ModelMessage::new(
        ModelMessageRole::User,
        ModelContent::text(text).expect("valid text content"),
    )
    .expect("valid model message")
}

fn test_request() -> ModelRequest {
    ModelRequest::new(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![user_message("What is the weather in Shanghai?")],
        vec![weather_tool()],
        GenerationConfig::new(Some(128), false).expect("valid generation config"),
    )
    .expect("valid model request")
}

fn test_tool_call() -> ModelToolCall {
    let mut arguments = Map::new();
    arguments.insert("city".to_owned(), Value::String("Shanghai".to_owned()));

    ModelToolCall::new(
        ModelToolCallId::new("call.provider/abc-123").expect("valid tool call id"),
        ToolName::new("lookup_weather").expect("valid tool name"),
        ToolArguments::new(arguments),
    )
}

fn test_response() -> ModelResponse {
    ModelResponse::new(
        vec![
            ModelOutput::text("Checking the weather."),
            ModelOutput::tool_call(test_tool_call()),
        ],
        FinishReason::ToolCalls,
        Some(Usage::new(11, 7)),
    )
}

#[test]
fn model_provider_trait_is_object_safe() {
    struct EmptyProvider {
        name: ProviderName,
        capabilities: ModelCapabilities,
    }

    impl EmptyProvider {
        fn new() -> Self {
            Self {
                name: ProviderName::new("empty-provider").expect("valid provider name"),
                capabilities: ModelCapabilities::new(true, false, false, false, None, None)
                    .expect("valid capabilities"),
            }
        }
    }

    impl ModelProvider for EmptyProvider {
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
            Box::pin(async {
                let stream: ModelEventStream =
                    Box::pin(futures_util::stream::poll_fn(|_| Poll::Ready(None)));
                Ok(stream)
            })
        }
    }

    let provider: Arc<dyn ModelProvider> = Arc::new(EmptyProvider::new());
    assert_eq!(provider.name().as_str(), "empty-provider");
    assert!(provider.capabilities().supports_streaming());

    let stream = block_on(provider.stream_model(test_request(), ModelStreamContext::default()))
        .expect("empty provider should return a stream");
    let events = block_on(stream.collect::<Vec<_>>());
    assert!(events.is_empty());
}

#[test]
fn protocol_types_round_trip_through_json() {
    assert_json_round_trip(&ModelName::new("vendor/model:latest").expect("valid model name"));
    assert_json_round_trip(&ModelToolCallId::new("call.vendor/123").expect("valid call id"));
    assert_json_round_trip(&ModelContent::text("hello").expect("valid text content"));
    assert_json_round_trip(&user_message("hello"));
    assert_json_round_trip(
        &GenerationConfig::new(Some(256), false).expect("valid generation config"),
    );
    assert_json_round_trip(&test_request());
    assert_json_round_trip(&Usage::new(5, 8));
    assert_json_round_trip(
        &ModelCapabilities::new(true, true, false, true, Some(8192), Some(2048))
            .expect("valid capabilities"),
    );
    assert_json_round_trip(&test_tool_call());
    assert_json_round_trip(&ModelOutput::text("hello"));
    assert_json_round_trip(&test_response());
    assert_json_round_trip(&ModelEvent::Started);
    assert_json_round_trip(&ModelEvent::OutputTextDelta {
        delta: "partial".to_owned(),
    });
    assert_json_round_trip(&ModelEvent::ToolCallRequested {
        call: test_tool_call(),
    });
    assert_json_round_trip(&ModelEvent::Completed {
        response: test_response(),
    });
}

#[test]
fn model_text_content_uses_validated_constructor_and_accessor() {
    let content = ModelContent::text("hello").expect("valid text content");

    assert_eq!(content.as_text(), "hello");
    assert_eq!(
        serde_json::to_value(&content).expect("content should serialize"),
        json!({ "type": "text", "text": "hello" })
    );

    let decoded =
        serde_json::from_value::<ModelContent>(json!({ "type": "text", "text": "hello" }))
            .expect("valid text content should deserialize");
    assert_eq!(decoded.as_text(), "hello");
}

#[test]
fn schema_generation_compiles_for_public_protocol_types() {
    assert_schema_compiles::<ModelName>();
    assert_schema_compiles::<ModelToolCallId>();
    assert_schema_compiles::<ToolArguments>();
    assert_schema_compiles::<ModelToolCall>();
    assert_schema_compiles::<ModelContent>();
    assert_schema_compiles::<ModelMessageRole>();
    assert_schema_compiles::<ModelMessage>();
    assert_schema_compiles::<GenerationConfig>();
    assert_schema_compiles::<ModelRequest>();
    assert_schema_compiles::<ModelOutput>();
    assert_schema_compiles::<FinishReason>();
    assert_schema_compiles::<Usage>();
    assert_schema_compiles::<ModelResponse>();
    assert_schema_compiles::<ModelEvent>();
    assert_schema_compiles::<ModelCapabilities>();
}

#[test]
fn validation_rejects_invalid_protocol_values() {
    assert!(
        ModelRequest::new(
            ModelName::new("model").expect("valid model name"),
            Vec::new(),
            Vec::new(),
            GenerationConfig::default(),
        )
        .is_err()
    );

    assert!(ModelContent::text("").is_err());
    assert!(ModelContent::text("   ").is_err());
    assert!(serde_json::from_value::<ModelContent>(json!({ "type": "text", "text": "" })).is_err());
    assert!(
        serde_json::from_value::<ModelContent>(json!({ "type": "text", "text": "   " })).is_err()
    );
    assert!(
        ModelMessage::new(
            ModelMessageRole::User,
            ModelContent::text("valid").expect("valid content"),
        )
        .is_ok()
    );

    for invalid in ["", "   ", " leading", "trailing ", "has\nnewline"] {
        assert!(
            ModelName::new(invalid).is_err(),
            "{invalid:?} should reject"
        );
        assert!(
            ModelToolCallId::new(invalid).is_err(),
            "{invalid:?} should reject"
        );
    }

    let overlong = "m".repeat(257);
    assert!(ModelName::new(&overlong).is_err());
    assert!(ModelToolCallId::new(&overlong).is_err());
    assert!(GenerationConfig::new(Some(0), false).is_err());
    assert!(ModelCapabilities::new(true, false, false, false, Some(0), None).is_err());

    assert!(ToolArguments::try_from(json!("not an object")).is_err());
    assert!(ToolArguments::try_from(json!([["city", "Shanghai"]])).is_err());
    assert!(serde_json::from_value::<ToolArguments>(json!(null)).is_err());
    assert!(
        serde_json::from_value::<ModelMessage>(json!({
            "role": "user",
            "content": { "type": "text", "text": "" }
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelRequest>(json!({
            "model": "model",
            "messages": [],
            "tools": [],
            "generation": { "max_output_tokens": null, "allow_parallel_tool_calls": false }
        }))
        .is_err()
    );
}

#[test]
fn tagged_protocol_json_rejects_unknown_fields() {
    assert!(
        serde_json::from_value::<ModelContent>(
            json!({ "type": "text", "text": "hello", "unexpected": true })
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelEvent>(json!({
            "type": "started",
            "unexpected": true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelEvent>(json!({
            "type": "output_text_delta",
            "delta": "partial",
            "unexpected": true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelOutput>(json!({
            "type": "text",
            "text": "hello",
            "unexpected": true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelOutput>(json!({
            "type": "tool_call",
            "call": test_tool_call(),
            "unexpected": true
        }))
        .is_err()
    );
}

#[test]
fn text_content_allows_multiline_compiled_context() {
    let content = ModelContent::text("First line\nSecond line").expect("multiline text is valid");
    assert_eq!(content.as_text(), "First line\nSecond line");
}

#[test]
fn broad_provider_identifiers_are_allowed_without_tool_name_rules() {
    let model = ModelName::new("vendor/model.family:2026-05-17").expect("valid model name");
    let call_id = ModelToolCallId::new("call.provider/opaque.id:42").expect("valid call id");

    assert_eq!(model.as_str(), "vendor/model.family:2026-05-17");
    assert_eq!(call_id.as_str(), "call.provider/opaque.id:42");
}

#[test]
fn usage_total_is_overflow_safe() {
    assert_eq!(Usage::new(7, 9).total_tokens(), Some(16));
    assert_eq!(Usage::new(u64::MAX, 1).total_tokens(), None);
}

#[test]
fn model_request_json_has_no_provider_conversation_or_openai_tool_wrappers() {
    let value = serde_json::to_value(test_request()).expect("request should serialize");

    assert!(value.get("previous_response_id").is_none());
    assert!(value.get("thread_id").is_none());
    assert!(value.get("store").is_none());
    assert!(value.get("session_id").is_none());
    assert!(value.get("ledger_id").is_none());

    let tool = &value["tools"][0];
    assert_eq!(tool["name"], json!("lookup_weather"));
    assert!(tool.get("function").is_none());
    assert_ne!(tool.get("type"), Some(&json!("function")));
}
