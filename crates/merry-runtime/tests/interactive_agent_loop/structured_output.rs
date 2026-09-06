use crate::support::{
    models::{RecordingProvider, completed_tool_call_event, model_name},
    runtime::session_id,
    tools::{final_output_call, final_output_contract},
};
use merry_core::{InteractiveRunState, RuntimeEvent, ToolInputSchema};
use merry_runtime::{
    AgentLoopConfig, FinalOutputContract, Runtime, StepContext, StructuredOutputRetryPolicy,
};
use schemars::{JsonSchema, Schema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
struct InteractiveStructuredAnswer {
    #[schemars(description = "Numeric final answer.")]
    answer: u64,
}

fn structured_final_output_contract() -> FinalOutputContract {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "answer": {
                "type": "integer",
                "description": "Numeric final answer."
            }
        },
        "required": ["answer"],
        "additionalProperties": false
    }))
    .expect("structured output schema should be valid");

    FinalOutputContract::new(ToolInputSchema::new(schema).expect("schema should be an object"))
        .expect("final output contract should be valid")
        .with_output_decoder::<InteractiveStructuredAnswer>()
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_structured_output_is_recorded_as_a_runtime_event() {
    let provider = RecordingProvider::new_with_steps(vec![vec![Ok(completed_tool_call_event(
        final_output_call(
            "interactive-final-output",
            json!({"summary": "interactive result"}),
        ),
    ))]]);
    let runtime = Runtime::builder(session_id("interactive-structured-output"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();

    let _ = stream
        .next_event()
        .await
        .expect("initial event should be readable")
        .expect("interactive run should remain open");
    input
        .submit_next("return a structured answer")
        .await
        .expect("input should be accepted");

    let mut saw_final_output = false;
    loop {
        let event = timeout(Duration::from_secs(1), stream.next_event())
            .await
            .expect("structured interactive output should not deadlock")
            .expect("interactive stream should remain healthy")
            .expect("interactive stream should remain open");
        match event {
            RuntimeEvent::FinalOutputRecorded { .. } => saw_final_output = true,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if saw_final_output => break,
            _ => {}
        }
    }

    assert!(saw_final_output);
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(provider.recorded_requests().len(), 1);

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream closes");
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_structured_output_retries_after_validation_failure() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(final_output_call(
            "interactive-final-invalid",
            json!({"answer": "not-a-number"}),
        )))],
        vec![Ok(completed_tool_call_event(final_output_call(
            "interactive-final-valid",
            json!({"answer": 42}),
        )))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-structured-retry"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default()
                .with_final_output_contract(structured_final_output_contract())
                .with_structured_output_retry_policy(StructuredOutputRetryPolicy::new(1)),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();

    let _ = stream
        .next_event()
        .await
        .expect("initial event should be readable")
        .expect("interactive run should remain open");
    input
        .submit_next("return a numeric structured answer")
        .await
        .expect("input should be accepted");

    let mut saw_failed_output = false;
    let mut saw_final_output = false;
    loop {
        let event = timeout(Duration::from_secs(1), stream.next_event())
            .await
            .expect("structured retry should not deadlock")
            .expect("interactive stream should remain healthy")
            .expect("interactive stream should remain open");
        match event {
            RuntimeEvent::ToolCallFinished { result, .. }
                if result
                    .diagnostic()
                    .is_some_and(|diagnostic| diagnostic.code() == "tool_input_schema_invalid") =>
            {
                saw_failed_output = true;
            }
            RuntimeEvent::FinalOutputRecorded { .. } => saw_final_output = true,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if saw_final_output => break,
            _ => {}
        }
    }

    assert!(saw_failed_output);
    assert!(saw_final_output);
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(provider.recorded_requests().len(), 2);

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream closes");
}
