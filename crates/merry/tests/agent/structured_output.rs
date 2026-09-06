use crate::{
    StrictStructuredAnswer, StructuredAnswer, agent, final_output_call, model_name, session_id,
};
use merry::{
    AgentBuilder, AgentLoopStatus, RuntimeEvent, StructuredOutputRetryPolicy, StructuredRunResult,
};
use merry_core::ToolName;
use merry_llm::{
    FinishReason, ModelEvent, ModelOutput, ModelResponse, ModelToolCall, ModelToolCallId,
    ToolArguments, testing::FakeModelProvider,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn structured_run_builds_schema_and_decodes_recorded_output() {
    let call = ModelToolCall::new(
        ModelToolCallId::new("call-final").expect("test call id should be valid"),
        ToolName::new("merry_final_output").expect("final output tool name should be valid"),
        ToolArguments::try_from(serde_json::json!({"answer": "typed"}))
            .expect("test arguments should be an object"),
    );
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = agent(provider);

    let result: StructuredRunResult<StructuredAnswer> = agent
        .run_structured("return a typed answer")
        .await
        .expect("structured run should complete");

    assert_eq!(result.output().answer, "typed");
    assert_eq!(result.run().status(), &AgentLoopStatus::Completed);
}

#[tokio::test]
async fn structured_run_rejects_non_object_schema_before_starting_provider() {
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text("provider should not be called")],
            FinishReason::Stop,
            None,
        ),
    })]));
    let agent = agent(provider);

    let error = agent
        .run_structured::<String>("return a scalar")
        .await
        .expect_err("scalar structured output should be rejected before the run");

    assert!(matches!(
        error,
        merry::AgentError::FinalOutputContract {
            source: merry_runtime::FinalOutputContractError::RootSchemaMustBeObject,
        }
    ));
}

#[tokio::test]
async fn structured_output_retry_stays_in_one_run_and_recovers() {
    let provider = Arc::new(FakeModelProvider::new_turns(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(final_output_call(
                    "call-final-invalid",
                    json!("not-a-number"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(final_output_call(
                    "call-final-valid",
                    json!(42),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
    ]));
    let agent = AgentBuilder::new(session_id("structured-retry"))
        .model_provider(provider, model_name())
        .structured_output_retry_policy(StructuredOutputRetryPolicy::new(1))
        .build()
        .expect("structured retry agent should build");

    let result: StructuredRunResult<StrictStructuredAnswer> = agent
        .run_structured("return a numeric answer")
        .await
        .expect("structured output should recover after one retry");

    assert_eq!(result.output().answer, 42);
    assert_eq!(result.run().status(), &AgentLoopStatus::Completed);
    assert_eq!(result.run().model_turns_run(), 2);
    assert!(result.run().events().iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == merry_core::ToolCallResultStatus::Failed
    )));
}

#[tokio::test]
async fn structured_output_failure_retains_run_when_retries_are_exhausted() {
    let provider = Arc::new(FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(final_output_call(
                "call-final-exhausted",
                json!("not-a-number"),
            ))],
            FinishReason::ToolCalls,
            None,
        ),
    })]));
    let agent = AgentBuilder::new(session_id("structured-retry-exhausted"))
        .model_provider(provider, model_name())
        .structured_output_retry_policy(StructuredOutputRetryPolicy::disabled())
        .build()
        .expect("structured retry agent should build");

    let error = agent
        .run_structured::<StrictStructuredAnswer>("return a numeric answer")
        .await
        .expect_err("invalid structured output should be reported");
    let merry::AgentError::StructuredOutputNotRecorded { run } = error else {
        panic!("structured failure should retain its run result");
    };

    assert!(matches!(run.status(), AgentLoopStatus::Failed { .. }));
    assert_eq!(run.model_turns_run(), 1);
    assert!(run.events().iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolCallFinished { result, .. }
            if result.status() == merry_core::ToolCallResultStatus::Failed
    )));
}
