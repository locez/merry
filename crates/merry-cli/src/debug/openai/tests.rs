use super::*;
use crate::testing::ScriptedProvider;
use merry_core::{RuntimeJournalEvent, ToolCallResultStatus, ToolName};
use merry_llm::{
    FinishReason, GenerationConfig, ModelEvent, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments,
};
use merry_runtime::{Runtime, StepContext, StepInput};
use serde_json::Map;
use std::sync::Arc;

#[tokio::test]
async fn tool_helper_executes_one_pending_call_and_continues() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-debug").expect("valid tool call id"),
        ToolName::new(DEBUG_TOOL_NAME).expect("valid tool name"),
        ToolArguments::new(Map::new()),
    );
    let provider = ScriptedProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("continued after tool")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-tool").unwrap())
        .register_tool(echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")))
        .model_provider(
            Arc::new(provider.clone()),
            merry_llm::ModelName::new("debug-model").unwrap(),
        )
        .build()
        .expect("runtime should build");
    let input = StepInput::user_text("please call the tool").expect("valid input");
    let context = StepContext::default().with_generation_config(
        GenerationConfig::new(Some(16), false).expect("valid generation config"),
    );
    let mut output = Vec::new();

    write_tool_events(&runtime, input, context, &mut output)
        .await
        .unwrap_or_else(|_| panic!("tool events should write"));

    let text = String::from_utf8(output).expect("output should be utf-8");
    let events = text
        .lines()
        .map(|line| serde_json::from_str::<RuntimeJournalEvent>(line).expect("line should be JSON"))
        .collect::<Vec<_>>();
    let event_types = events
        .iter()
        .map(|event| {
            let value = serde_json::to_value(event).expect("event should serialize");
            value["payload"]["type"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        event_types,
        [
            "session_started",
            "step_started",
            "tool_call_pending",
            "artifact_recorded",
            "tool_call_resolved",
            "step_started",
            "assistant_output_recorded",
            "step_completed",
        ]
    );

    let resolved = events
        .iter()
        .find_map(|event| match &event.payload {
            merry_core::RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool should be resolved");
    assert_eq!(resolved.status(), ToolCallResultStatus::Succeeded);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools().len(), 1);
    assert_eq!(requests[0].tools()[0].name().as_str(), DEBUG_TOOL_NAME);
    assert!(requests[0].continuations().is_empty());
    assert_eq!(requests[0].generation().max_output_tokens(), Some(16));
    assert!(!requests[0].generation().allow_parallel_tool_calls());

    assert_eq!(requests[1].tools().len(), 1);
    assert_eq!(requests[1].tools()[0].name().as_str(), DEBUG_TOOL_NAME);
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-debug");
    assert_eq!(
        continuation.result().status(),
        ToolCallResultStatus::Succeeded
    );
    assert_eq!(
        continuation.result().content().as_text(),
        Some("debug result")
    );
    assert!(
        requests[1]
            .messages()
            .iter()
            .any(|message| message.content().as_text() == DEBUG_TOOL_CONTINUATION_INPUT)
    );
    assert_eq!(requests[1].generation().max_output_tokens(), Some(16));
    assert!(!requests[1].generation().allow_parallel_tool_calls());
}

#[tokio::test]
async fn tool_helper_errors_when_first_step_calls_wrong_tool() {
    let wrong_tool_name = "wrong_tool";
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-wrong").expect("valid tool call id"),
        ToolName::new(wrong_tool_name).expect("valid tool name"),
        ToolArguments::new(Map::new()),
    );
    let provider = ScriptedProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("should not continue")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-wrong-tool").unwrap())
        .register_tool(echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")))
        .model_provider(
            Arc::new(provider.clone()),
            merry_llm::ModelName::new("debug-model").unwrap(),
        )
        .build()
        .expect("runtime should build");
    let input = StepInput::user_text("please call the tool").expect("valid input");
    let mut output = Vec::new();

    let error = write_tool_events(&runtime, input, StepContext::default(), &mut output)
        .await
        .expect_err("wrong first-step tool call should fail");

    match error {
        CliError::Unexpected(message) => {
            assert!(message.contains(DEBUG_TOOL_NAME));
            assert!(message.contains(wrong_tool_name));
        }
        _ => panic!("expected unexpected error for wrong tool call"),
    }

    let text = String::from_utf8(output).expect("output should be utf-8");
    let events = text
        .lines()
        .map(|line| serde_json::from_str::<RuntimeJournalEvent>(line).expect("line should be JSON"))
        .collect::<Vec<_>>();
    assert!(
        !events.is_empty(),
        "first-step runtime events should be preserved"
    );
    let event_types = events
        .iter()
        .map(|event| {
            let value = serde_json::to_value(event).expect("event should serialize");
            value["payload"]["type"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();

    assert!(event_types.iter().any(|kind| kind == "tool_call_pending"));
    assert!(!event_types.iter().any(|kind| kind == "tool_call_resolved"));
    let pending = events
        .iter()
        .find_map(|event| match &event.payload {
            merry_core::RuntimeJournalPayload::ToolCallPending { call } => Some(call),
            _ => None,
        })
        .expect("wrong tool call should remain pending in first-step events");
    assert_eq!(pending.name().as_str(), wrong_tool_name);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].continuations().is_empty());
}

#[tokio::test]
async fn tool_helper_errors_when_first_step_does_not_call_debug_echo() {
    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("completed without tool")],
            FinishReason::Stop,
            None,
        ),
    })]]);
    let runtime = Runtime::builder(merry_core::SessionId::new("debug-openai-no-tool").unwrap())
        .register_tool(echo_tool("debug result").unwrap_or_else(|_| panic!("valid debug tool")))
        .model_provider(
            Arc::new(provider.clone()),
            merry_llm::ModelName::new("debug-model").unwrap(),
        )
        .build()
        .expect("runtime should build");
    let input = StepInput::user_text("do not call the tool").expect("valid input");
    let mut output = Vec::new();

    let error = write_tool_events(&runtime, input, StepContext::default(), &mut output)
        .await
        .expect_err("missing first-step tool call should fail");

    match error {
        CliError::Unexpected(message) => {
            assert!(message.contains(DEBUG_TOOL_NAME));
            assert!(message.contains("no tool call was pending"));
        }
        _ => panic!("expected unexpected error for missing tool call"),
    }

    let text = String::from_utf8(output).expect("output should be utf-8");
    let events = text
        .lines()
        .map(|line| serde_json::from_str::<RuntimeJournalEvent>(line).expect("line should be JSON"))
        .collect::<Vec<_>>();
    assert!(
        !events.is_empty(),
        "first-step runtime events should be preserved"
    );
    let event_types = events
        .iter()
        .map(|event| {
            let value = serde_json::to_value(event).expect("event should serialize");
            value["payload"]["type"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        event_types,
        [
            "session_started",
            "step_started",
            "assistant_output_recorded",
            "step_completed",
        ]
    );
    assert!(!event_types.iter().any(|kind| kind == "tool_call_pending"));
    assert!(!event_types.iter().any(|kind| kind == "tool_call_resolved"));

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].continuations().is_empty());
}
