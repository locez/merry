use crate::support::{
    events::{event_kind_names, public_event_kind_names},
    models::{
        ScriptedModelProvider, completed_text_event, completed_tool_call_batch_event,
        completed_tool_call_event, model_tool_call, model_tool_call_with_arguments,
    },
    runtime::{runtime_with_provider, runtime_with_tool},
    tools::{ScriptedToolExecutor, final_output_contract},
};
use futures_util::StreamExt;
use merry_core::ToolCallResultStatus;
use merry_llm::ModelInputItem;
use merry_runtime::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopConfigError, AgentLoopStatus,
    AgentRunMessage, FINAL_OUTPUT_TOOL_NAME, RuntimeError, StepContext, StepInput,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn run_agent_loop_stream_completes_final_output_without_continuation_budget() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call_with_arguments(
            "call-final",
            FINAL_OUTPUT_TOOL_NAME,
            json!({"summary": "Order A123 shipped."}),
        ),
    ))]]);
    let runtime = runtime_with_provider("agent-loop-stream-final-output-budget", provider);
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(1)
                .expect("valid non-zero budget")
                .with_final_output_contract(final_output_contract()),
        )
        .expect("agent loop stream should start");

    let mut events = Vec::new();
    loop {
        match stream
            .next_message()
            .await
            .expect("agent run message should be readable")
        {
            Some(AgentRunMessage::Event(event)) => events.push(event),
            Some(AgentRunMessage::ToolInvocations { .. }) => {
                panic!("agent run emitted an unexpected host tool batch")
            }
            Some(_) => panic!("agent run emitted an unsupported message"),
            None => break,
        }
    }
    let result = stream.result().await.expect("stream should produce result");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        result
            .final_output_json()
            .expect("structured final output should be recorded")
            .json(),
        r#"{"summary":"Order A123 shipped."}"#
    );
    assert_eq!(
        public_event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallStarted",
            "FinalOutputRecorded",
        ]
    );
}

#[test]
fn structured_output_retries_are_opt_in() {
    assert_eq!(
        AgentLoopConfig::default()
            .structured_output_retry_policy()
            .max_retries(),
        0
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_completes_when_model_calls_final_output_tool() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call_with_arguments(
            "call-final",
            FINAL_OUTPUT_TOOL_NAME,
            json!({"summary": "Order A123 shipped."}),
        ),
    ))]]);
    let runtime = runtime_with_provider("agent-loop-final-output", provider);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        result
            .final_output_json()
            .expect("structured final output should be recorded")
            .json(),
        r#"{"summary":"Order A123 shipped."}"#
    );
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "FinalOutputRecorded",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_imports_final_output_contract_from_step_context() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call_with_arguments(
            "call-context-final",
            FINAL_OUTPUT_TOOL_NAME,
            json!({"summary": "context contract"}),
        ),
    ))]]);
    let runtime = runtime_with_provider("agent-loop-context-final-output", provider);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new())
                .with_final_output_contract(final_output_contract()),
            AgentLoopConfig::default(),
        )
        .await
        .expect("context final-output contract should be applied");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(result.final_output_json().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_rejects_duplicate_final_output_contract_sources() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("unused"))]]);
    let runtime = runtime_with_provider("agent-loop-duplicate-final-output", provider.clone());

    let error = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new())
                .with_final_output_contract(final_output_contract()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect_err("duplicate final-output contracts should be rejected");

    assert!(matches!(
        error.runtime_error(),
        RuntimeError::AgentLoopConfig {
            source: AgentLoopConfigError::FinalOutputContractConfiguredTwice,
        }
    ));
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn final_output_tool_call_is_not_replayed_into_next_step_transcript() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-final",
                FINAL_OUTPUT_TOOL_NAME,
                json!({"summary": "Order A123 shipped."}),
            ),
        ))],
        vec![Ok(completed_text_event("next answer"))],
    ]);
    let runtime = runtime_with_provider("agent-loop-final-output-then-next-step", provider.clone());

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default()
                .with_final_output_contract(final_output_contract())
                .with_structured_output_retry_policy(
                    merry_runtime::StructuredOutputRetryPolicy::new(1),
                ),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let next_step = runtime
        .step(
            StepInput::user_text("Handle the next request.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("next step should start");
    let next_events = next_step.collect::<Vec<_>>().await;

    assert_eq!(
        event_kind_names(&next_events),
        ["StepStarted", "AssistantOutputRecorded", "StepCompleted"]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1]
            .input()
            .iter()
            .all(|item| matches!(item, ModelInputItem::Message(_))),
        "runtime final-output tool calls must not be replayed into later provider input"
    );
    assert!(requests[1].continuations().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_final_output_tool_arguments_retry_as_failed_tool_result() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments("call-final-invalid", FINAL_OUTPUT_TOOL_NAME, json!({})),
        ))],
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-final-valid",
                FINAL_OUTPUT_TOOL_NAME,
                json!({"summary": "Order A123 shipped."}),
            ),
        ))],
    ]);
    let runtime = runtime_with_provider("agent-loop-final-output-invalid-retry", provider.clone());

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default()
                .with_final_output_contract(final_output_contract())
                .with_structured_output_retry_policy(
                    merry_runtime::StructuredOutputRetryPolicy::new(1),
                ),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        result
            .final_output_json()
            .expect("structured final output should be recorded")
            .json(),
        r#"{"summary":"Order A123 shipped."}"#
    );
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "FinalOutputRecorded",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-final-invalid");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("schema failure should carry diagnostic")
            .code(),
        "tool_input_schema_invalid"
    );
    assert!(
        continuation
            .result()
            .content()
            .as_str()
            .contains("tool_input_schema_invalid")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_executes_runtime_tool_before_final_output_tool() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-search",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-final",
                FINAL_OUTPUT_TOOL_NAME,
                json!({"summary": "Order A123 shipped."}),
            ),
        ))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool("agent-loop-tool-then-final-output", provider, executor);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search notes and return structured status.")
                .expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        result
            .final_output_json()
            .expect("structured final output should be recorded")
            .json(),
        r#"{"summary":"Order A123 shipped."}"#
    );
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "FinalOutputRecorded",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_rejects_final_output_mixed_with_other_tool_calls_before_execution() {
    let provider =
        ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-search", "search_notes"),
            model_tool_call_with_arguments(
                "call-final",
                FINAL_OUTPUT_TOOL_NAME,
                json!({"summary": "Order A123 shipped."}),
            ),
        ]))]]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool(
        "agent-loop-mixed-final-output-batch",
        provider,
        executor.clone(),
    );

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search notes and return structured status.")
                .expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("protocol failure should be returned as loop status");

    assert_eq!(
        result.status(),
        &AgentLoopStatus::Failed {
            diagnostic: merry_core::ErrorInfo::new(
                "final_output_tool_batch_mixed",
                "final-output tool calls must be the only call in their model batch",
            )
            .expect("valid diagnostic"),
        }
    );
    assert!(executor.calls().is_empty());
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_blocks_text_completion_when_final_output_contract_is_active() {
    let provider =
        ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("Order A123 shipped."))]]);
    let runtime = runtime_with_provider("agent-loop-final-output-text-blocked", provider);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Return structured order status.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default().with_final_output_contract(final_output_contract()),
        )
        .await
        .expect("agent loop should run");

    assert_eq!(
        result.status(),
        &AgentLoopStatus::Blocked {
            reason: AgentLoopBlockedReason::FinalOutputToolNotCalled,
        }
    );
    assert!(result.final_output().is_none());
    assert!(result.final_output_json().is_none());
}
