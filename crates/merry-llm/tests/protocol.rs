use futures_executor::block_on;
use futures_util::StreamExt;
use merry_core::{
    ErrorInfo, ProviderName, ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelContent, ModelError, ModelEvent,
    ModelEventStream, ModelInputItem, ModelMessage, ModelMessageRole, ModelName, ModelOutput,
    ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse, ModelResponseFormat,
    ModelStreamContext, ModelStructuredOutputFormat, ModelToolCall, ModelToolCallId,
    ModelToolContinuation, ModelToolResult, ModelToolResultContent, ToolArguments, Usage,
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
    let mut arguments = Map::new();
    arguments.insert("city".to_owned(), Value::String("Shanghai".to_owned()));

    ModelToolCall::new(
        ModelToolCallId::new("call.provider/abc-123").expect("valid tool call id"),
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
    assert_json_round_trip(
        &ModelToolResultContent::text("Sunny").expect("valid text result content"),
    );
    assert_json_round_trip(
        &ModelToolResultContent::json(r#"{"temperature_c":22}"#)
            .expect("valid JSON result content"),
    );
    assert_json_round_trip(&test_tool_result());
    assert_json_round_trip(&ModelToolResult::failed(
        test_tool_call().id().clone(),
        ModelToolResultContent::text("Tool execution failed")
            .expect("valid failure result content"),
        test_diagnostic(),
    ));
    assert_json_round_trip(&test_tool_continuation());
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
fn model_request_records_stable_tool_profile_hash() {
    let first = ModelRequest::new(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![user_message("Use tools.")],
        vec![named_tool("search_notes"), named_tool("read_file")],
        GenerationConfig::default(),
    )
    .expect("valid request");
    let reordered = ModelRequest::new(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![user_message("Use tools.")],
        vec![named_tool("read_file"), named_tool("search_notes")],
        GenerationConfig::default(),
    )
    .expect("valid request");
    let changed = ModelRequest::new(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![user_message("Use tools.")],
        vec![named_tool("read_file")],
        GenerationConfig::default(),
    )
    .expect("valid request");

    assert!(first.tool_profile_hash().as_str().starts_with("fnv1a64:"));
    assert_eq!(first.tool_profile_hash(), reordered.tool_profile_hash());
    assert_ne!(first.tool_profile_hash(), changed.tool_profile_hash());
}

#[test]
fn model_request_rejects_mismatched_tool_profile_hash() {
    let mut value = serde_json::to_value(test_request()).expect("request should serialize");
    value["tool_profile_hash"] = Value::String("fnv1a64:0000000000000000".to_owned());

    assert!(serde_json::from_value::<ModelRequest>(value).is_err());
}

#[test]
fn model_request_stable_prefix_hash_tracks_base_instructions_and_tools() {
    let model = ModelName::new("vendor/model-family:2025-04-14").expect("valid model name");
    let first = ModelRequest::new_with_continuations_and_stable_prefix(
        model.clone(),
        vec![
            system_message("Base runtime instructions."),
            user_message("Use tools for request one."),
        ],
        vec![named_tool("search_notes"), named_tool("read_file")],
        Vec::new(),
        GenerationConfig::default(),
        1,
    )
    .expect("valid request");
    let changed_dynamic = ModelRequest::new_with_continuations_and_stable_prefix(
        model.clone(),
        vec![
            system_message("Base runtime instructions."),
            user_message("Use tools for request two."),
        ],
        vec![named_tool("read_file"), named_tool("search_notes")],
        Vec::new(),
        GenerationConfig::default(),
        1,
    )
    .expect("valid request");
    let changed_base = ModelRequest::new_with_continuations_and_stable_prefix(
        model.clone(),
        vec![
            system_message("Changed runtime instructions."),
            user_message("Use tools for request one."),
        ],
        vec![named_tool("search_notes"), named_tool("read_file")],
        Vec::new(),
        GenerationConfig::default(),
        1,
    )
    .expect("valid request");
    let changed_tool_profile = ModelRequest::new_with_continuations_and_stable_prefix(
        model,
        vec![
            system_message("Base runtime instructions."),
            user_message("Use tools for request one."),
        ],
        vec![named_tool("read_file")],
        Vec::new(),
        GenerationConfig::default(),
        1,
    )
    .expect("valid request");

    assert_eq!(first.stable_prefix_message_count(), 1);
    assert_eq!(first.stable_prefix_messages().len(), 1);
    assert_eq!(first.dynamic_messages().len(), 1);
    assert!(first.stable_prefix_hash().as_str().starts_with("fnv1a64:"));
    assert!(
        first
            .dynamic_context_hash()
            .as_str()
            .starts_with("fnv1a64:")
    );
    assert_eq!(
        first.stable_prefix_hash(),
        changed_dynamic.stable_prefix_hash()
    );
    assert_ne!(
        first.dynamic_context_hash(),
        changed_dynamic.dynamic_context_hash()
    );
    assert_ne!(
        first.stable_prefix_hash(),
        changed_base.stable_prefix_hash()
    );
    assert_ne!(
        first.stable_prefix_hash(),
        changed_tool_profile.stable_prefix_hash()
    );
}

#[test]
fn model_request_preserves_ordered_input_items_and_hashes_dynamic_tail() {
    let call = test_tool_call();
    let result = ModelToolResult::succeeded(
        call.id().clone(),
        ModelToolResultContent::text("file contents").expect("valid result content"),
    );

    let request = ModelRequest::new_with_input_and_stable_prefix(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![
            ModelInputItem::Message(system_message("Base runtime instructions.")),
            ModelInputItem::Message(user_message("first user")),
            ModelInputItem::ToolCall(call),
            ModelInputItem::ToolResult(result),
            ModelInputItem::Message(user_message("second user")),
        ],
        vec![weather_tool()],
        GenerationConfig::default(),
        1,
    )
    .expect("valid ordered request");

    assert_eq!(request.stable_prefix_item_count(), 1);
    assert_eq!(request.input().len(), 5);
    assert!(matches!(request.input()[2], ModelInputItem::ToolCall(_)));
    assert!(matches!(request.input()[3], ModelInputItem::ToolResult(_)));
    assert_eq!(request.dynamic_input().len(), 4);
    assert!(
        request
            .dynamic_input_hash()
            .as_str()
            .starts_with("fnv1a64:")
    );
}

#[test]
fn model_request_rejects_non_system_stable_prefix_message() {
    let err = ModelRequest::new_with_continuations_and_stable_prefix(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![user_message("User text must not be stable prefix.")],
        Vec::new(),
        Vec::new(),
        GenerationConfig::default(),
        1,
    )
    .expect_err("stable prefix should be system/developer layer only");

    assert!(err.to_string().contains("stable prefix messages"));
}

#[test]
fn model_request_rejects_mismatched_context_hashes() {
    let request = ModelRequest::new_with_continuations_and_stable_prefix(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![
            system_message("Base runtime instructions."),
            user_message("Use tools."),
        ],
        vec![named_tool("read_file")],
        Vec::new(),
        GenerationConfig::default(),
        1,
    )
    .expect("valid request");

    let mut stable_value = serde_json::to_value(&request).expect("request should serialize");
    stable_value["stable_prefix_hash"] = Value::String("fnv1a64:0000000000000000".to_owned());
    assert!(serde_json::from_value::<ModelRequest>(stable_value).is_err());

    let mut dynamic_value = serde_json::to_value(request).expect("request should serialize");
    dynamic_value["dynamic_context_hash"] = Value::String("fnv1a64:0000000000000000".to_owned());
    assert!(serde_json::from_value::<ModelRequest>(dynamic_value).is_err());
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
    assert_schema_compiles::<ModelToolResultContent>();
    assert_schema_compiles::<ModelToolResult>();
    assert_schema_compiles::<ModelToolContinuation>();
    assert_schema_compiles::<ModelContent>();
    assert_schema_compiles::<ModelMessageRole>();
    assert_schema_compiles::<ModelMessage>();
    assert_schema_compiles::<GenerationConfig>();
    assert_schema_compiles::<ModelStructuredOutputFormat>();
    assert_schema_compiles::<ModelResponseFormat>();
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
    assert!(ModelToolResultContent::text("").is_err());
    assert!(ModelToolResultContent::text("   ").is_err());
    assert!(ModelToolResultContent::json("").is_err());
    assert!(ModelToolResultContent::json("   ").is_err());
    assert!(
        serde_json::from_value::<ModelToolResultContent>(json!({ "type": "text", "text": "" }))
            .is_err()
    );
    assert!(
        serde_json::from_value::<ModelToolResultContent>(json!({ "type": "json", "json": "   " }))
            .is_err()
    );
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
    assert!(
        ModelToolResult::new(
            test_tool_call().id().clone(),
            ToolCallResultStatus::Succeeded,
            ModelToolResultContent::text("ok").expect("valid content"),
            Some(test_diagnostic()),
        )
        .is_err()
    );
    assert!(
        ModelToolResult::new(
            test_tool_call().id().clone(),
            ToolCallResultStatus::Failed,
            ModelToolResultContent::text("failed").expect("valid content"),
            None,
        )
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
    assert!(
        serde_json::from_value::<ModelToolCall>(json!({
            "id": "call.provider/abc-123",
            "name": "lookup_weather",
            "arguments": { "city": "Shanghai" },
            "unexpected": true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelToolResultContent>(
            json!({ "type": "text", "text": "hello", "unexpected": true })
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelToolResult>(json!({
            "call_id": "call.provider/abc-123",
            "status": "succeeded",
            "content": { "type": "text", "text": "hello" },
            "diagnostic": null,
            "unexpected": true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelToolContinuation>(json!({
            "call": test_tool_call(),
            "result": test_tool_result(),
            "unexpected": true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelRequest>(json!({
            "model": "model",
            "messages": [{ "role": "user", "content": { "type": "text", "text": "hello" } }],
            "tools": [],
            "continuations": [],
            "generation": { "max_output_tokens": null, "allow_parallel_tool_calls": false },
            "unexpected": true
        }))
        .is_err()
    );
}

#[test]
fn model_tool_result_enforces_diagnostic_constraints() {
    let call_id = test_tool_call().id().clone();
    let content = ModelToolResultContent::text("ok").expect("valid content");

    let succeeded = ModelToolResult::new(
        call_id.clone(),
        ToolCallResultStatus::Succeeded,
        content.clone(),
        None,
    )
    .expect("successful result without diagnostic should be valid");
    assert_eq!(succeeded.status(), ToolCallResultStatus::Succeeded);
    assert!(succeeded.diagnostic().is_none());

    let failed = ModelToolResult::new(
        call_id,
        ToolCallResultStatus::Failed,
        content,
        Some(test_diagnostic()),
    )
    .expect("failed result with diagnostic should be valid");
    assert_eq!(failed.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        failed.diagnostic().map(ErrorInfo::code),
        Some("tool_failed")
    );

    assert!(
        serde_json::from_value::<ModelToolResult>(json!({
            "call_id": "call.provider/abc-123",
            "status": "succeeded",
            "content": { "type": "text", "text": "ok" },
            "diagnostic": { "code": "tool_failed", "message": "Tool failed with status 2" }
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ModelToolResult>(json!({
            "call_id": "call.provider/abc-123",
            "status": "failed",
            "content": { "type": "text", "text": "failed" },
            "diagnostic": null
        }))
        .is_err()
    );
}

#[test]
fn tool_continuation_rejects_call_result_id_mismatch() {
    let mismatched_result = ModelToolResult::succeeded(
        ModelToolCallId::new("call.provider/other").expect("valid call id"),
        ModelToolResultContent::text("ok").expect("valid content"),
    );

    assert!(ModelToolContinuation::new(test_tool_call(), mismatched_result).is_err());
    assert!(
        serde_json::from_value::<ModelToolContinuation>(json!({
            "call": test_tool_call(),
            "result": {
                "call_id": "call.provider/other",
                "status": "succeeded",
                "content": { "type": "text", "text": "ok" },
                "diagnostic": null
            }
        }))
        .is_err()
    );
}

#[test]
fn model_request_constructors_preserve_compatibility_and_continuations() {
    let request = test_request();
    assert!(request.continuations().is_empty());

    let continuation = test_tool_continuation();
    let request_with_continuations = ModelRequest::new_with_continuations(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![user_message("Continue after checking the weather.")],
        vec![weather_tool()],
        vec![continuation.clone()],
        GenerationConfig::new(Some(128), false).expect("valid generation config"),
    )
    .expect("valid request with continuations");
    assert_eq!(request_with_continuations.continuations(), &[continuation]);

    let decoded_without_continuations = serde_json::from_value::<ModelRequest>(json!({
        "model": "vendor/model-family:2025-04-14",
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": "What is the weather in Shanghai?" }
        }],
        "tools": [],
        "generation": { "max_output_tokens": 128, "allow_parallel_tool_calls": false }
    }))
    .expect("old request JSON without continuations should deserialize");
    assert!(decoded_without_continuations.continuations().is_empty());
}

#[test]
fn model_request_can_carry_structured_output_contract() {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" }
        },
        "required": ["answer"],
        "additionalProperties": false
    }))
    .expect("test schema should parse");
    let format = ModelResponseFormat::StructuredOutput(
        ModelStructuredOutputFormat::new("answer_payload", schema.clone())
            .expect("valid structured output format"),
    );
    let request = ModelRequest::new_with_response_format(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![user_message("Answer as JSON.")],
        Vec::new(),
        GenerationConfig::default(),
        Some(format.clone()),
    )
    .expect("valid structured request");

    assert_eq!(request.response_format(), Some(&format));

    let value = serde_json::to_value(&request).expect("request should serialize");
    assert_eq!(value["response_format"]["type"], json!("structured_output"));
    assert_eq!(value["response_format"]["name"], json!("answer_payload"));
    assert_eq!(value["response_format"]["strict"], json!(true));
    assert_eq!(
        value["response_format"]["schema"],
        serde_json::to_value(schema).expect("schema serializes")
    );

    let decoded = serde_json::from_value::<ModelRequest>(value).expect("request should decode");
    assert_eq!(decoded.response_format(), Some(&format));
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
    let request = ModelRequest::new_with_continuations(
        ModelName::new("vendor/model-family:2025-04-14").expect("valid model name"),
        vec![user_message("Continue after checking the weather.")],
        vec![weather_tool()],
        vec![test_tool_continuation()],
        GenerationConfig::new(Some(128), false).expect("valid generation config"),
    )
    .expect("valid request");
    let value = serde_json::to_value(request).expect("request should serialize");

    assert!(value.get("previous_response_id").is_none());
    assert!(value.get("thread_id").is_none());
    assert!(value.get("store").is_none());
    assert!(value.get("session_id").is_none());
    assert!(value.get("ledger_id").is_none());
    assert!(value.get("tool_call_id").is_none());
    assert!(value.get("tool_calls").is_none());

    let tool = &value["tools"][0];
    assert_eq!(tool["name"], json!("lookup_weather"));
    assert!(tool.get("function").is_none());
    assert_ne!(tool.get("type"), Some(&json!("function")));

    let continuation = &value["continuations"][0];
    assert!(continuation["call"].get("tool_call_id").is_none());
    assert!(continuation["result"].get("tool_call_id").is_none());
    assert_eq!(
        continuation["result"]["call_id"],
        json!("call.provider/abc-123")
    );
}
