use merry_core::{ErrorInfo, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{
    FinishReason, GenerationConfig, ModelContent, ModelMessage, ModelMessageRole, ModelName,
    ModelOutput, ModelRequest, ModelResponse, ModelToolCall, ModelToolCallId,
    ModelToolContinuation, ModelToolResult, ModelToolResultContent, ToolArguments, Usage,
};
use schemars::{JsonSchema, Schema};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

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

fn system_message(text: &str) -> ModelMessage {
    ModelMessage::new(
        ModelMessageRole::System,
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

fn named_tool(name: &str) -> ToolSpec {
    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Run a deterministic test tool",
        object_schema(),
    )
    .expect("valid tool spec")
}

fn test_tool_call() -> ModelToolCall {
    test_tool_call_with_id("call.provider/abc-123")
}

fn test_tool_call_with_id(id: &str) -> ModelToolCall {
    let mut arguments = Map::new();
    arguments.insert("city".to_owned(), Value::String("Shanghai".to_owned()));

    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid tool call id"),
        ToolName::new("lookup_weather").expect("valid tool name"),
        ToolArguments::new(arguments),
    )
}

fn test_diagnostic() -> ErrorInfo {
    ErrorInfo::new("tool_failed", "Tool failed with status 2").expect("valid diagnostic")
}

fn test_tool_result() -> ModelToolResult {
    ModelToolResult::succeeded(
        test_tool_call().id().clone(),
        ModelToolResultContent::json(r#"{"temperature_c":22}"#).expect("valid result content"),
    )
}

fn test_tool_continuation() -> ModelToolContinuation {
    ModelToolContinuation::new(test_tool_call(), test_tool_result())
        .expect("matching tool call continuation")
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

#[path = "protocol/content.rs"]
mod content;

#[path = "protocol/generation.rs"]
mod generation;

#[path = "protocol/provider_boundary.rs"]
mod provider_boundary;

#[path = "protocol/requests.rs"]
mod requests;

#[path = "protocol/serialization.rs"]
mod serialization;

#[path = "protocol/tool_continuations.rs"]
mod tool_continuations;
