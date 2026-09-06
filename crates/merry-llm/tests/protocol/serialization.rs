use crate::{
    assert_json_round_trip, assert_schema_compiles, test_diagnostic, test_request, test_response,
    test_tool_call, test_tool_continuation, test_tool_result, user_message,
};
use merry_core::ToolCallResultStatus;
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelContent, ModelEvent, ModelMessage,
    ModelMessageRole, ModelName, ModelOutput, ModelRequest, ModelResponse, ModelResponseFormat,
    ModelStructuredOutputFormat, ModelToolCall, ModelToolCallId, ModelToolContinuation,
    ModelToolResult, ModelToolResultContent, ParallelToolCalls, ReasoningEffort, ToolArguments,
    Usage,
};
use serde_json::json;

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
    assert_schema_compiles::<ParallelToolCalls>();
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
    assert!(ReasoningEffort::new("").is_err());
    assert!(ReasoningEffort::new(" leading").is_err());
    assert!(ReasoningEffort::new("has\nnewline").is_err());
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
fn broad_provider_identifiers_are_allowed_without_tool_name_rules() {
    let model = ModelName::new("vendor/model.family:2026-05-17").expect("valid model name");
    let call_id = ModelToolCallId::new("call.provider/opaque.id:42").expect("valid call id");

    assert_eq!(model.as_str(), "vendor/model.family:2026-05-17");
    assert_eq!(call_id.as_str(), "call.provider/opaque.id:42");
}

#[test]
fn usage_total_and_optional_subcounts_are_provider_neutral() {
    let basic = Usage::new(7, 9);
    assert_eq!(basic.input_tokens(), 7);
    assert_eq!(basic.cached_input_tokens(), None);
    assert_eq!(basic.output_tokens(), 9);
    assert_eq!(basic.reasoning_output_tokens(), None);
    assert_eq!(basic.total_tokens(), 16);

    let detailed = Usage::with_details(11, Some(8), 5, Some(2), 16);
    assert_eq!(detailed.cached_input_tokens(), Some(8));
    assert_eq!(detailed.reasoning_output_tokens(), Some(2));
    assert_eq!(detailed.total_tokens(), 16);
}
