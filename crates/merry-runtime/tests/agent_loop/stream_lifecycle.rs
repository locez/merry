use crate::support::{
    events::{event_kind_names, next_agent_run_event, pending_tool_call, public_event_kind_names},
    models::{
        BlockingModelProvider, ScriptedModelProvider, completed_text_event,
        completed_text_event_with_usage, completed_tool_call_event, model_name, model_tool_call,
    },
    runtime::{run_default_loop, runtime_with_provider, runtime_with_tool, session_id},
    tools::ScriptedToolExecutor,
};
use merry_core::ModelUsage;
use merry_runtime::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopConfigError, AgentLoopStatus,
    AgentRunMessage, Runtime, StepContext, StepInput,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn run_agent_loop_result_includes_session_usage_snapshot() {
    let usage = ModelUsage::with_details(21, Some(13), 8, Some(2), 29);
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event_with_usage(
        "usage final",
        usage,
    ))]]);
    let runtime = Runtime::builder(session_id("agent-loop-result-usage"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Report usage.").await;

    let result_usage = result
        .session_usage()
        .cloned()
        .expect("agent loop result should include usage");
    assert_eq!(result_usage.last, usage);
    assert_eq!(result_usage.total, usage);
    assert_eq!(runtime.usage().await, Some(result_usage));
}

#[tokio::test]
async fn run_agent_loop_stream_result_includes_session_usage_snapshot() {
    let usage = ModelUsage::with_details(31, Some(19), 11, None, 42);
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event_with_usage(
        "stream usage final",
        usage,
    ))]]);
    let runtime = Runtime::builder(session_id("agent-loop-stream-result-usage"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Report stream usage.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    let result = stream.result().await.expect("stream should produce result");

    let result_usage = result
        .session_usage()
        .cloned()
        .expect("stream result should include usage");
    assert_eq!(result_usage.last, usage);
    assert_eq!(result_usage.total, usage);
    assert_eq!(runtime.usage().await, Some(result_usage));
}

#[tokio::test]
async fn run_agent_loop_stream_yields_step_events_before_provider_finishes() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let runtime = Runtime::builder(session_id("agent-loop-live-stream"))
        .model_provider(
            Arc::new(BlockingModelProvider::new(started_tx, release_rx)),
            model_name(),
        )
        .build()
        .expect("runtime should build");

    let mut events = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Wait for the provider.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    started_rx
        .await
        .expect("provider should start and wait for release");
    let first = tokio::time::timeout(
        Duration::from_millis(100),
        next_agent_run_event(&mut events),
    )
    .await
    .expect("stream should yield before provider finishes");
    let second = tokio::time::timeout(
        Duration::from_millis(100),
        next_agent_run_event(&mut events),
    )
    .await
    .expect("stream should yield before provider finishes");

    assert_eq!(
        public_event_kind_names(&[first, second]),
        ["SessionStarted", "StepStarted"]
    );

    release_tx
        .send(())
        .expect("provider release receiver should still be waiting");
    let mut remaining = Vec::new();
    loop {
        match events
            .next_message()
            .await
            .expect("remaining agent run messages should be readable")
        {
            Some(AgentRunMessage::Event(event)) => remaining.push(event),
            Some(AgentRunMessage::ToolInvocations { .. }) => {
                panic!("agent run emitted an unexpected host tool batch")
            }
            Some(_) => panic!("agent run emitted an unsupported message"),
            None => break,
        }
    }
    assert_eq!(
        public_event_kind_names(&remaining),
        ["AssistantMessage", "StepCompleted"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn max_model_turns_blocks_before_infinite_tool_loop_and_leaves_pending() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-loop", "search_notes"),
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text("should not run\n");
    let runtime = runtime_with_tool("agent-loop-max-model-turns", provider, executor);

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Search forever.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(1).expect("valid non-zero budget"),
        )
        .await
        .expect("agent loop should return blocked status");

    assert_eq!(
        result.status(),
        &AgentLoopStatus::Blocked {
            reason: AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns: 1 },
        }
    );
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        event_kind_names(result.events()),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        runtime.pending_tool_calls().await,
        vec![pending_tool_call(result.events()).clone()]
    );
}

#[test]
fn agent_loop_config_rejects_zero_max_model_turns() {
    let err = AgentLoopConfig::new(0).expect_err("zero budget should be rejected");

    assert_eq!(err, AgentLoopConfigError::MaxModelTurnsMustBeNonZero);
}

#[tokio::test(flavor = "current_thread")]
async fn no_provider_loop_completes_like_skeleton_step() {
    let runtime = Runtime::builder(session_id("agent-loop-no-provider"))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "No provider.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        event_kind_names(result.events()),
        ["SessionStarted", "StepStarted", "StepCompleted"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_loop_returns_cancelled_status() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event(
        "should not be requested",
    ))]]);
    let runtime = runtime_with_provider("agent-loop-pre-cancelled", provider);
    let token = CancellationToken::new();
    token.cancel();

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Cancelled.").expect("valid step input"),
            StepContext::new(token),
            AgentLoopConfig::default(),
        )
        .await
        .expect("pre-cancelled loop should return cancelled status");

    assert!(matches!(
        result.status(),
        AgentLoopStatus::Cancelled {
            diagnostic
        } if diagnostic.code() == "cancelled"
    ));
    assert_eq!(event_kind_names(result.events()), ["Cancelled"]);
}
