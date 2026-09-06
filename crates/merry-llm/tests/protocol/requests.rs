use crate::{
    named_tool, system_message, test_request, test_tool_call, test_tool_continuation, user_message,
    weather_tool,
};
use merry_llm::{
    GenerationConfig, ModelInputItem, ModelName, ModelRequest, ModelResponseFormat,
    ModelStructuredOutputFormat, ModelToolResult, ModelToolResultContent,
};
use schemars::Schema;
use serde_json::{Value, json};

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
