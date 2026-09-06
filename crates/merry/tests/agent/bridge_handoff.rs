use crate::{bridge_tool, model_name, session_id};
use merry::{
    AgentBuilder, AgentLoopStatus, RuntimeEvent,
    binding::OwnedAgentRunMessage,
    rust::{
        AgentRunMessage, ToolInvocationContent, ToolInvocationResult, ToolInvocationSubmission,
    },
};
use merry_core::{ErrorInfo, ToolName};
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn bridge_requests_are_explicit_driver_messages() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("bridge complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("bridge-driver"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut stream = agent
        .stream_with_tool_handoff("use the bridge")
        .expect("stream should start");

    let mut saw_started = false;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(message) = stream.next().await.expect("driver should advance") {
            match message {
                AgentRunMessage::Event(event) => {
                    saw_started |= matches!(
                        event.as_ref(),
                        RuntimeEvent::ToolCallStarted { call, .. }
                            if call.id().as_str() == "call-bridge"
                    );
                }
                AgentRunMessage::ToolInvocations { mut batch } => {
                    assert!(saw_started);
                    assert_eq!(batch.len(), 1);
                    assert_eq!(batch.invocations()[0].name().as_str(), "bridge_lookup");
                    let call_id = batch.invocations()[0].id().clone();
                    batch
                        .submit(vec![ToolInvocationResult::succeeded(
                            call_id,
                            ToolInvocationContent::json(r#"{"found":true}"#)
                                .expect("bridge result JSON should be valid"),
                        )])
                        .await
                        .expect("host result should be accepted");
                    break;
                }
                _ => panic!("unexpected future run message variant"),
            }
        }
    })
    .await
    .expect("bridge request should be emitted");

    let mut saw_finished = false;
    let mut saw_final_output = false;
    while let Some(message) = stream.next().await.expect("driver should advance") {
        match message {
            AgentRunMessage::Event(event) => {
                saw_finished |= matches!(
                    event.as_ref(),
                    RuntimeEvent::ToolCallFinished { result, .. }
                    if result.status() == merry_core::ToolCallResultStatus::Succeeded
                );
                if let RuntimeEvent::ToolCallFinished { result, .. } = event.as_ref() {
                    assert!(result.artifact().id().as_str().starts_with("tool-result-"));
                }
                saw_final_output |= matches!(
                    event.as_ref(),
                    RuntimeEvent::AssistantMessage { text, .. } if text == "bridge complete"
                );
            }
            AgentRunMessage::ToolInvocations { .. } => {
                panic!("the test provider should issue only one tool invocation batch")
            }
            _ => panic!("unexpected future run message variant"),
        }
    }
    let result = stream.result().await.expect("bridge run should complete");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("bridge complete"));
    assert!(saw_finished);
    assert!(saw_final_output);
}

#[tokio::test]
async fn owned_binding_run_preserves_the_host_batch_contract() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-owned-bridge").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({"key": "value"}))
            .expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("owned bridge complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("owned-bridge-driver"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut run = agent
        .stream_with_owned_tool_handoff("use the owned bridge")
        .expect("owned stream should start");

    loop {
        let message = run
            .next()
            .await
            .expect("owned driver should advance")
            .expect("owned driver should emit a message");
        match message {
            OwnedAgentRunMessage::Event(_) => {}
            OwnedAgentRunMessage::ToolInvocations { batch } => {
                assert_eq!(batch.len(), 1);
                assert_eq!(batch.invocations()[0].name().as_str(), "bridge_lookup");
                let batch_id = batch.id().clone();
                let call_id = batch.invocations()[0].id().clone();
                assert!(matches!(
                    run.next().await,
                    Err(merry::AgentError::ToolInvocationBatchPending)
                ));
                let submission = run
                    .submit_tool_invocation_results(
                        &batch_id,
                        vec![ToolInvocationResult::succeeded(
                            call_id,
                            ToolInvocationContent::text("resolved"),
                        )],
                    )
                    .await
                    .expect("owned bridge result should be accepted");
                assert_eq!(submission, ToolInvocationSubmission::Accepted);
                break;
            }
            _ => panic!("unexpected future owned run message variant"),
        }
    }

    while run
        .next()
        .await
        .expect("owned driver should continue")
        .is_some()
    {}
    let result = run.result().await.expect("owned run should complete");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("owned bridge complete"));
}

#[tokio::test]
async fn bridge_invocations_are_delivered_as_one_ordered_batch() {
    let first_call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-1").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let second_call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-2").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![
                    ModelOutput::tool_call(first_call),
                    ModelOutput::tool_call(second_call),
                ],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("batch complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("bridge-batch-driver"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut stream = agent
        .stream_with_tool_handoff("use both bridge calls")
        .expect("stream should start");

    loop {
        let message = stream
            .next()
            .await
            .expect("driver should advance")
            .expect("driver should emit a message");
        match message {
            AgentRunMessage::Event(_) => {}
            AgentRunMessage::ToolInvocations { mut batch } => {
                assert_eq!(
                    batch
                        .invocations()
                        .iter()
                        .map(|invocation| invocation.id().as_str())
                        .collect::<Vec<_>>(),
                    ["call-bridge-1", "call-bridge-2"]
                );

                let incomplete = vec![ToolInvocationResult::succeeded(
                    batch.invocations()[0].id().clone(),
                    ToolInvocationContent::text("first"),
                )];
                assert!(matches!(
                    batch.submit(incomplete).await,
                    Err(merry::AgentError::ToolInvocationBatchMismatch { .. })
                ));

                let results = batch
                    .invocations()
                    .iter()
                    .rev()
                    .map(|invocation| {
                        ToolInvocationResult::succeeded(
                            invocation.id().clone(),
                            ToolInvocationContent::json(format!(
                                r#"{{"resolved":"{}"}}"#,
                                invocation.id().as_str()
                            ))
                            .expect("bridge result JSON should be valid"),
                        )
                    })
                    .collect();
                batch
                    .submit(results)
                    .await
                    .expect("complete bridge result batch should be accepted");
                break;
            }
            _ => panic!("unexpected future run message variant"),
        }
    }

    let mut finished_ids = Vec::new();
    while let Some(message) = stream.next().await.expect("driver should advance") {
        match message {
            AgentRunMessage::Event(event) => {
                if let RuntimeEvent::ToolCallFinished { result, .. } = event.as_ref() {
                    finished_ids.push(result.call_id().as_str().to_owned());
                }
            }
            AgentRunMessage::ToolInvocations { .. } => {
                panic!("the test provider should issue only one invocation batch")
            }
            _ => panic!("unexpected future run message variant"),
        }
    }

    let result = stream.result().await.expect("bridge run should complete");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("batch complete"));
    assert_eq!(finished_ids, ["call-bridge-1", "call-bridge-2"]);
}

#[tokio::test]
async fn bridge_submission_validation_error_keeps_batch_open_for_retry() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-retry").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({})).expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("bridge retry complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("bridge-retry"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut driver = agent
        .stream_with_tool_handoff("retry the bridge result")
        .expect("stream should start");

    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(1), driver.next())
            .await
            .expect("bridge retry should not deadlock")
            .expect("driver should advance")
            .expect("driver should emit a message");
        if let AgentRunMessage::ToolInvocations { mut batch } = message {
            let call_id = batch.invocations()[0].id().clone();
            let invalid = batch
                .submit(vec![ToolInvocationResult::succeeded(
                    call_id.clone(),
                    ToolInvocationContent::text(""),
                )])
                .await;
            assert!(matches!(
                invalid,
                Err(merry::AgentError::Runtime {
                    source: merry_runtime::RuntimeError::UnsupportedToolResultContent { .. }
                })
            ));

            batch
                .submit(vec![ToolInvocationResult::succeeded(
                    call_id,
                    ToolInvocationContent::text("resolved"),
                )])
                .await
                .expect("corrected bridge result should be accepted");
            break;
        }
    }

    while driver
        .next()
        .await
        .expect("driver should continue after corrected result")
        .is_some()
    {}
    let result = driver.result().await.expect("bridge retry should complete");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("bridge retry complete"));
}

#[tokio::test]
async fn bridge_domain_failure_is_recorded_and_loop_continues() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-bridge-failure").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({"key": "missing"}))
            .expect("bridge arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("failure handled")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("bridge-failure"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("bridge agent should build");
    let mut driver = agent
        .stream_with_tool_handoff("handle the missing lookup")
        .expect("driver should start");

    loop {
        let message = driver
            .next()
            .await
            .expect("driver should advance")
            .expect("driver should emit a tool invocation batch");
        if let AgentRunMessage::ToolInvocations { mut batch } = message {
            let call_id = batch.invocations()[0].id().clone();
            let diagnostic =
                ErrorInfo::new("lookup_not_found", "the requested lookup was not found")
                    .expect("test diagnostic should be valid");
            let submission = batch
                .submit(vec![ToolInvocationResult::failed(
                    call_id,
                    ToolInvocationContent::json(r#"{"found":false}"#)
                        .expect("bridge result JSON should be valid"),
                    diagnostic,
                )])
                .await
                .expect("failed host result should be accepted");
            assert_eq!(submission, ToolInvocationSubmission::Accepted);
            break;
        }
    }

    let mut saw_failed_result = false;
    while let Some(message) = driver.next().await.expect("driver should advance") {
        if let AgentRunMessage::Event(event) = message {
            saw_failed_result |= matches!(
                event.as_ref(),
                RuntimeEvent::ToolCallFinished { result, .. }
                    if result.status() == merry_core::ToolCallResultStatus::Failed
                        && result.diagnostic().is_some_and(|info| info.code() == "lookup_not_found")
            );
        }
    }
    let result = driver
        .result()
        .await
        .expect("failed tool run should continue");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("failure handled"));
    assert!(saw_failed_result);
}
