use crate::{agent, bridge_tool, model_name, session_id, text_provider};
use merry::{
    AgentBuilder, RuntimeEvent,
    rust::{InteractiveMessage, ToolInvocationContent, ToolInvocationResult},
};
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn interactive_run_exposes_shared_handles_and_public_events() {
    let agent = agent(text_provider("interactive"));
    let run = agent
        .start_interactive()
        .expect("interactive run should start");
    let (mut events, input, control) = run.split();

    assert_eq!(input.run_id(), control.run_id());
    let first = tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
        .await
        .expect("interactive run should announce its initial state")
        .expect("interactive stream should remain healthy")
        .expect("interactive stream should remain open");
    assert!(matches!(
        first,
        RuntimeEvent::InteractiveRunStateChanged { .. }
    ));

    input
        .submit_next("say hello")
        .await
        .expect("interactive input should be accepted");
    let saw_output = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(event) = events
            .next()
            .await
            .expect("interactive stream should remain healthy")
        {
            if matches!(
                event,
                RuntimeEvent::AssistantMessage { ref text, .. } if text == "interactive"
            ) {
                return true;
            }
        }
        false
    })
    .await
    .expect("interactive run should emit model output");
    assert!(saw_output);

    control
        .close()
        .await
        .expect("interactive run should close cleanly");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        events.wait_until_closed(),
    )
    .await
    .expect("interactive stream should reach its closed state")
    .expect("interactive stream should close cleanly");
}

#[tokio::test]
async fn interactive_facade_uses_the_same_ordered_bridge_batch_contract() {
    let first_call = ModelToolCall::new(
        ModelToolCallId::new("interactive-call-1").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({"key": "first"}))
            .expect("bridge arguments should be an object"),
    );
    let second_call = ModelToolCall::new(
        ModelToolCallId::new("interactive-call-2").expect("test call id should be valid"),
        ToolName::new("bridge_lookup").expect("bridge tool name should be valid"),
        ToolArguments::try_from(json!({"key": "second"}))
            .expect("bridge arguments should be an object"),
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
                vec![ModelOutput::text("interactive bridge complete")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("interactive-bridge-facade"))
        .model_provider(provider, model_name())
        .allow_bridge_tools()
        .register_tool(bridge_tool())
        .build()
        .expect("interactive bridge agent should build");
    let run = agent
        .start_interactive()
        .expect("interactive bridge run should start");
    let (mut stream, input, control) = run.split();

    let _ = stream
        .next()
        .await
        .expect("initial interactive message should be readable")
        .expect("interactive stream should remain open");
    input
        .submit_next("resolve both bridge calls")
        .await
        .expect("interactive input should be accepted");

    let mut batch = loop {
        let message =
            tokio::time::timeout(std::time::Duration::from_secs(1), stream.next_message())
                .await
                .expect("interactive bridge batch should be emitted")
                .expect("interactive stream should remain healthy")
                .expect("interactive stream should remain open");
        match message {
            InteractiveMessage::Event(_) => {}
            InteractiveMessage::ToolInvocations { batch } => break batch,
            _ => panic!("unexpected future interactive message variant"),
        }
    };
    assert_eq!(
        batch
            .invocations()
            .iter()
            .map(|invocation| invocation.id().as_str())
            .collect::<Vec<_>>(),
        ["interactive-call-1", "interactive-call-2"]
    );

    let results = batch
        .invocations()
        .iter()
        .rev()
        .map(|invocation| {
            ToolInvocationResult::succeeded(
                invocation.id().clone(),
                ToolInvocationContent::text(format!("result for {}", invocation.name())),
            )
        })
        .collect();
    batch
        .submit(results)
        .await
        .expect("complete interactive bridge batch should be accepted");
    drop(batch);

    let mut finished = 0;
    let mut saw_final_output = false;
    loop {
        let Some(message) =
            tokio::time::timeout(std::time::Duration::from_secs(1), stream.next_message())
                .await
                .expect("interactive continuation should not deadlock")
                .expect("interactive stream should remain healthy")
        else {
            break;
        };
        match message {
            InteractiveMessage::Event(event) => match event.as_ref() {
                RuntimeEvent::ToolCallFinished { .. } => finished += 1,
                RuntimeEvent::AssistantMessage { text, .. }
                    if text == "interactive bridge complete" =>
                {
                    saw_final_output = true
                }
                RuntimeEvent::InteractiveRunStateChanged { .. } if saw_final_output => break,
                _ => {}
            },
            InteractiveMessage::ToolInvocations { .. } => {
                panic!("interactive emitted a second handoff before continuation completed")
            }
            _ => panic!("unexpected future interactive message variant"),
        }
    }
    assert_eq!(finished, 2);
    assert!(saw_final_output);

    control
        .close()
        .await
        .expect("interactive bridge run should close");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        stream.wait_until_closed(),
    )
    .await
    .expect("interactive stream should close")
    .expect("interactive stream should close cleanly");
}
