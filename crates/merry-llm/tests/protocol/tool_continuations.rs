use crate::{
    assert_json_round_trip, test_diagnostic, test_request, test_tool_call, test_tool_call_with_id,
    test_tool_continuation, user_message, weather_tool,
};
use merry_core::{ErrorInfo, ToolCallResultStatus};
use merry_llm::{
    GenerationConfig, ModelInputItem, ModelName, ModelRequest, ModelToolBatchContinuation,
    ModelToolCallBatch, ModelToolCallId, ModelToolContinuation, ModelToolResult,
    ModelToolResultContent,
};
use serde_json::json;

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
fn model_tool_call_batch_rejects_empty_and_duplicate_calls() {
    assert!(ModelToolCallBatch::new(Vec::new()).is_err());

    let call = test_tool_call();
    assert!(ModelToolCallBatch::new(vec![call.clone(), call]).is_err());

    let batch = ModelToolCallBatch::new(vec![
        test_tool_call_with_id("call-a"),
        test_tool_call_with_id("call-b"),
    ])
    .expect("unique non-empty batch should be valid");
    assert_eq!(
        batch
            .calls()
            .iter()
            .map(|call| call.id().as_str())
            .collect::<Vec<_>>(),
        ["call-a", "call-b"]
    );
    assert_json_round_trip(&batch);
}

#[test]
fn model_tool_batch_continuation_validates_and_orders_results() {
    let call_a = test_tool_call_with_id("call-a");
    let call_b = test_tool_call_with_id("call-b");
    let batch = ModelToolCallBatch::new(vec![call_a.clone(), call_b.clone()])
        .expect("valid tool call batch");
    let result_a = ModelToolResult::succeeded(
        call_a.id().clone(),
        ModelToolResultContent::text("a").expect("valid content"),
    );
    let result_b = ModelToolResult::succeeded(
        call_b.id().clone(),
        ModelToolResultContent::text("b").expect("valid content"),
    );

    let continuation = ModelToolBatchContinuation::new(batch.clone(), vec![result_b, result_a])
        .expect("out-of-order complete results should normalize");
    assert_eq!(continuation.batch(), &batch);
    assert_eq!(
        continuation
            .results()
            .iter()
            .map(|result| result.call_id().as_str())
            .collect::<Vec<_>>(),
        ["call-a", "call-b"]
    );
    assert_json_round_trip(&continuation);

    assert!(ModelToolBatchContinuation::new(batch.clone(), Vec::new()).is_err());
    assert!(
        ModelToolBatchContinuation::new(
            batch,
            vec![ModelToolResult::succeeded(
                ModelToolCallId::new("call-unknown").expect("valid call id"),
                ModelToolResultContent::text("unknown").expect("valid content"),
            )],
        )
        .is_err()
    );
}

#[test]
fn model_request_accepts_grouped_tool_calls_and_results() {
    let call_a = test_tool_call_with_id("call-a");
    let call_b = test_tool_call_with_id("call-b");
    let result_a = ModelToolResult::succeeded(
        call_a.id().clone(),
        ModelToolResultContent::text("a").expect("valid content"),
    );
    let result_b = ModelToolResult::succeeded(
        call_b.id().clone(),
        ModelToolResultContent::text("b").expect("valid content"),
    );

    let request = ModelRequest::new_with_input_and_stable_prefix(
        ModelName::new("batch-model").expect("valid model name"),
        vec![
            ModelInputItem::Message(user_message("run both")),
            ModelInputItem::ToolCall(call_a),
            ModelInputItem::ToolCall(call_b),
            ModelInputItem::ToolResult(result_b),
            ModelInputItem::ToolResult(result_a),
        ],
        vec![weather_tool()],
        GenerationConfig::new(None, true).expect("valid generation config"),
        0,
    )
    .expect("grouped tool history should be valid");

    assert_eq!(request.batch_continuations().len(), 1);
    assert_eq!(request.continuations().len(), 2);
    assert_eq!(
        request.batch_continuations()[0]
            .results()
            .iter()
            .map(|result| result.call_id().as_str())
            .collect::<Vec<_>>(),
        ["call-a", "call-b"]
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

    let decoded_with_reasoning = serde_json::from_value::<ModelRequest>(json!({
        "model": "vendor/model-family:2025-04-14",
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": "Think carefully." }
        }],
        "tools": [],
        "generation": {
            "max_output_tokens": 128,
            "allow_parallel_tool_calls": false,
            "reasoning_effort": "high"
        }
    }))
    .expect("request JSON with reasoning effort should deserialize");
    assert_eq!(
        decoded_with_reasoning
            .generation()
            .reasoning_effort()
            .map(|effort| effort.as_str()),
        Some("high")
    );
}
