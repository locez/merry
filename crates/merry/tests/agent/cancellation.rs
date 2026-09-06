use crate::{PendingProvider, agent, bridge_tool, model_name, session_id};
use merry::{AgentBuilder, AgentLoopStatus, rust::AgentRunMessage};
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn cancelling_a_stream_waits_for_a_cancelled_terminal_result() {
    let agent = agent(Arc::new(PendingProvider::new()));
    let mut stream = agent
        .stream("wait until cancelled")
        .expect("stream should start");

    let result = tokio::time::timeout(std::time::Duration::from_secs(1), stream.cancel())
        .await
        .expect("cancellation should settle");
    let result = result.expect("cancel should return a runtime result");

    assert!(matches!(result.status(), AgentLoopStatus::Cancelled { .. }));
}

#[tokio::test]
async fn cancelling_a_stream_with_pending_bridge_request_returns_cancelled_result() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-cancel").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = AgentBuilder::new(session_id("bridge-cancel"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut driver = agent
        .stream_with_tool_handoff("cancel the pending bridge")
        .expect("driver should start");

    let result = loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), driver.next())
            .await
            .expect("tool invocation batch should be emitted")
            .expect("driver should advance")
            .expect("driver should emit a tool invocation batch");
        if let AgentRunMessage::ToolInvocations { batch } = message {
            break tokio::time::timeout(std::time::Duration::from_secs(1), batch.cancel())
                .await
                .expect("bridge cancellation should settle")
                .expect("cancel should return a runtime result");
        }
    };

    assert!(matches!(result.status(), AgentLoopStatus::Cancelled { .. }));
}

#[tokio::test]
async fn dropping_an_unresolved_invocation_batch_requests_cancellation() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-drop").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = AgentBuilder::new(session_id("bridge-drop"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut driver = agent
        .stream_with_tool_handoff("drop the pending bridge")
        .expect("driver should start");

    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), driver.next())
            .await
            .expect("tool invocation batch should be emitted")
            .expect("driver should advance")
            .expect("driver should emit a message");
        match message {
            AgentRunMessage::Event(_) => {}
            AgentRunMessage::ToolInvocations { batch } => {
                drop(batch);
                break;
            }
            _ => panic!("unexpected future run message variant"),
        }
    }

    let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while driver
            .next()
            .await
            .expect("driver should settle after dropped batch")
            .is_some()
        {}
        driver.result().await
    })
    .await
    .expect("dropped batch should not leave the producer waiting");
    let result = result.expect("cancelled run should return a result");
    assert!(matches!(result.status(), AgentLoopStatus::Cancelled { .. }));
}
