use crate::{LookupOrderInput, LookupOrderOutput, model_name, session_id};
use merry::{AgentBuilder, AgentLoopStatus, RuntimeEvent};
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn typed_profile_tool_is_executed_by_run_without_host_handoff() {
    let root = tempfile::tempdir().expect("profile workspace should be created");
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_counter = Arc::clone(&executions);
    let lookup_order = merry::Tool::new(
        "lookup_order",
        "Look up the current order status.",
        move |input: LookupOrderInput| {
            let execution_counter = Arc::clone(&execution_counter);
            async move {
                execution_counter.fetch_add(1, Ordering::SeqCst);
                Ok::<LookupOrderOutput, std::convert::Infallible>(LookupOrderOutput {
                    status: format!("order {} is ready", input.order_id),
                })
            }
        },
    )
    .expect("typed tool should build");
    let profile = merry::profiles::coding_agent(root.path())
        .tool(lookup_order)
        .build()
        .expect("coding profile should build");
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-typed-tool").expect("test call id should be valid"),
        ToolName::new("lookup_order").expect("tool name should be valid"),
        ToolArguments::try_from(json!({"order_id": "A-42"}))
            .expect("tool arguments should be an object"),
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
                vec![ModelOutput::text("order checked")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("typed-tool-run"))
        .model_provider(provider, model_name())
        .profile(profile)
        .expect("coding profile should apply")
        .build()
        .expect("agent should build");

    let result = agent
        .run("Check order A-42")
        .await
        .expect("run should complete");

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("order checked"));
    assert!(result.events().iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == merry_core::ToolCallResultStatus::Succeeded
    )));
}

#[tokio::test]
async fn typed_profile_tool_is_executed_inside_event_only_stream() {
    let root = tempfile::tempdir().expect("profile workspace should be created");
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_counter = Arc::clone(&executions);
    let lookup_order = merry::Tool::new(
        "lookup_order_stream",
        "Look up the current order status.",
        move |input: LookupOrderInput| {
            let execution_counter = Arc::clone(&execution_counter);
            async move {
                execution_counter.fetch_add(1, Ordering::SeqCst);
                Ok::<LookupOrderOutput, std::convert::Infallible>(LookupOrderOutput {
                    status: format!("order {} is ready", input.order_id),
                })
            }
        },
    )
    .expect("typed tool should build");
    let profile = merry::profiles::coding_agent(root.path())
        .tool(lookup_order)
        .build()
        .expect("coding profile should build");
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-typed-stream-tool").expect("test call id should be valid"),
        ToolName::new("lookup_order_stream").expect("tool name should be valid"),
        ToolArguments::try_from(json!({"order_id": "A-42"}))
            .expect("tool arguments should be an object"),
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
                vec![ModelOutput::text("order streamed")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("typed-tool-stream"))
        .model_provider(provider, model_name())
        .profile(profile)
        .expect("coding profile should apply")
        .build()
        .expect("agent should build");

    let mut stream = agent
        .stream("Check order A-42")
        .expect("event-only stream should start");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await.expect("event stream should advance") {
        events.push(event);
    }
    let result = stream
        .result()
        .await
        .expect("event-only stream should complete");

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("order streamed"));
    assert_eq!(events, result.events());
}

#[tokio::test]
async fn typed_tool_domain_error_is_recorded_and_model_loop_continues() {
    let root = tempfile::tempdir().expect("profile workspace should be created");
    let failing_tool = merry::Tool::new(
        "lookup_order_failure",
        "Look up an order that may be unavailable.",
        |_input: LookupOrderInput| async {
            Err::<LookupOrderOutput, &'static str>("order service unavailable")
        },
    )
    .expect("typed tool should build");
    let profile = merry::profiles::coding_agent(root.path())
        .tool(failing_tool)
        .build()
        .expect("coding profile should build");
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-typed-tool-failure").expect("test call id should be valid"),
        ToolName::new("lookup_order_failure").expect("tool name should be valid"),
        ToolArguments::try_from(json!({"order_id": "A-42"}))
            .expect("tool arguments should be an object"),
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
    let agent = AgentBuilder::new(session_id("typed-tool-failure"))
        .model_provider(provider, model_name())
        .profile(profile)
        .expect("coding profile should apply")
        .build()
        .expect("agent should build");

    let result = agent
        .run("Check order A-42")
        .await
        .expect("run should complete");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.final_output(), Some("failure handled"));
    assert!(result.events().iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == merry_core::ToolCallResultStatus::Failed
                && result.diagnostic().is_some_and(|info| info.code() == "tool_handler_failed")
    )));
}
