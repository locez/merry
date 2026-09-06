use crate::support::{
    events::{event_kind_names, pending_tool_call, public_event_kind_names},
    models::{
        ScriptedModelProvider, completed_text_event, completed_tool_call_batch_event,
        completed_tool_call_event, model_tool_call, model_tool_call_with_arguments,
    },
    runtime::{run_default_loop, runtime_with_bridge_tool, tool_call_id},
};
use merry_core::{RuntimeJournalPayload, ToolCallBatchId, ToolCallResultStatus, ToolName};
use merry_runtime::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopStatus, AgentRunMessage, RuntimeError,
    StepContext, StepInput, ToolExecutionOutcome,
};
use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn run_agent_loop_stream_resumes_same_loop_after_bridge_tool_result() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-bridge",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let runtime = runtime_with_bridge_tool("agent-loop-stream-bridge-resume", provider);
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Search notes.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    let mut events = Vec::new();
    while let Some(message) = stream
        .next_message()
        .await
        .expect("agent run message should be readable")
    {
        match message {
            AgentRunMessage::Event(event) => events.push(event),
            AgentRunMessage::ToolInvocations { batch } => {
                let [call] = batch.calls() else {
                    panic!("one bridge call should be represented as a one-element batch");
                };
                assert_eq!(call.id().as_str(), "call-bridge");
                let batch_id = batch.id().clone();
                let wrong_batch_id =
                    ToolCallBatchId::new("wrong-batch").expect("valid test batch id");
                let wrong_result = stream
                    .submit_bridge_tool_outcomes(
                        &wrong_batch_id,
                        vec![(
                            call.id().clone(),
                            ToolExecutionOutcome::succeeded_json(r#"{"ok":true}"#),
                        )],
                    )
                    .await
                    .expect_err("a stale batch id must be rejected");
                assert!(matches!(
                    wrong_result,
                    RuntimeError::BridgeToolResultBatchIdMismatch {
                        expected_batch_id,
                        received_batch_id,
                        ..
                    } if expected_batch_id == batch_id && received_batch_id == wrong_batch_id
                ));
                assert!(matches!(
                    stream.next_message().await,
                    Err(RuntimeError::AgentRunToolInvocationsPending { batch_id: pending_id, .. })
                        if pending_id == batch_id
                ));
                stream
                    .submit_bridge_tool_outcomes(
                        &batch_id,
                        vec![(
                            tool_call_id("call-bridge"),
                            ToolExecutionOutcome::succeeded_json(r#"{"ok":true}"#),
                        )],
                    )
                    .await
                    .expect("bridge result should submit to the active loop");
            }
            _ => {}
        }
    }

    let result = stream.result().await.expect("stream should produce result");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        public_event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallStarted",
            "ToolCallFinished",
            "StepStarted",
            "AssistantMessage",
            "StepCompleted",
        ]
    );
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "BridgeToolCallRequested",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_agent_loop_stream_resumes_after_multiple_bridge_results_in_model_order() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-bridge-1", "search_notes"),
            model_tool_call("call-bridge-2", "search_notes"),
        ]))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let runtime = runtime_with_bridge_tool("agent-loop-stream-bridge-batch", provider.clone());
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Search two sources.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    let mut requested = Vec::new();
    while let Some(message) = stream
        .next_message()
        .await
        .expect("agent run message should be readable")
    {
        if let AgentRunMessage::ToolInvocations { batch } = message {
            requested.extend(
                batch
                    .calls()
                    .iter()
                    .map(|call| call.id().as_str().to_owned()),
            );
            let outcomes = batch
                .calls()
                .iter()
                .rev()
                .map(|call| {
                    (
                        call.id().clone(),
                        ToolExecutionOutcome::succeeded_json(format!(
                            r#"{{"resolved":"{}"}}"#,
                            call.id().as_str()
                        )),
                    )
                })
                .collect();
            stream
                .submit_bridge_tool_outcomes(batch.id(), outcomes)
                .await
                .expect("bridge result batch should submit to the active loop");
        }
    }

    let result = stream.result().await.expect("stream should produce result");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(requested, ["call-bridge-1", "call-bridge-2"]);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].batch_continuations()[0]
            .results()
            .iter()
            .map(|result| result.call_id().as_str())
            .collect::<Vec<_>>(),
        ["call-bridge-1", "call-bridge-2"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_agent_loop_stream_cancellation_settles_pending_bridge_batch() {
    let provider =
        ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-bridge-cancel-1", "search_notes"),
            model_tool_call("call-bridge-cancel-2", "search_notes"),
        ]))]]);
    let runtime = runtime_with_bridge_tool("agent-loop-stream-bridge-cancel", provider.clone());
    let token = CancellationToken::new();
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Search notes.").expect("valid step input"),
            StepContext::new(token.clone()),
            AgentLoopConfig::default(),
        )
        .expect("agent loop stream should start");

    loop {
        match stream
            .next_message()
            .await
            .expect("agent run message should be readable")
        {
            Some(AgentRunMessage::Event(_)) => {}
            Some(AgentRunMessage::ToolInvocations { batch }) => {
                assert_eq!(batch.calls().len(), 2);
                break;
            }
            Some(_) => panic!("agent run emitted an unsupported message"),
            None => panic!("agent run closed before the bridge batch"),
        }
    }

    token.cancel();
    let cancellation_released_batch = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match stream.next_message().await {
                Err(RuntimeError::AgentRunToolInvocationsPending { .. }) => {
                    tokio::task::yield_now().await;
                }
                Ok(Some(_)) | Ok(None) | Err(_) => break true,
            }
        }
    })
    .await
    .expect("producer cancellation should release the consumer batch");
    assert!(cancellation_released_batch);
    tokio::time::timeout(Duration::from_secs(1), stream.cancel_and_wait())
        .await
        .expect("cancelled bridge run should settle promptly");
    let result = tokio::time::timeout(Duration::from_secs(1), stream.result())
        .await
        .expect("cancelled bridge run should return a result")
        .expect("cancelled bridge run should produce a terminal result");

    assert!(matches!(result.status(), AgentLoopStatus::Cancelled { .. }));
    assert!(runtime.pending_tool_calls().await.is_empty());
    let resolved = result
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().all(|result| {
        result.status() == ToolCallResultStatus::Failed
            && result
                .diagnostic()
                .is_some_and(|diagnostic| diagnostic.code() == "tool_abandoned_by_run_settlement")
    }));
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_blocks_for_bridge_tool_runner_instead_of_executing_runtime_tool() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-bridge", "search_notes"),
    ))]]);
    let runtime = runtime_with_bridge_tool("agent-loop-bridge-blocks", provider);

    let result = run_default_loop(&runtime, "Search notes.").await;

    assert_eq!(
        result.status(),
        &AgentLoopStatus::Blocked {
            reason: AgentLoopBlockedReason::BridgeToolCallRequested {
                call_id: tool_call_id("call-bridge"),
                tool_name: ToolName::new("search_notes").expect("valid tool name"),
            },
        }
    );
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "BridgeToolCallRequested",
        ]
    );
    assert_eq!(
        runtime.pending_tool_calls().await,
        vec![pending_tool_call(result.events()).clone()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_continues_after_invalid_bridge_tool_arguments_are_resolved() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments("call-invalid-bridge", "search_notes", json!({})),
        ))],
        vec![Ok(completed_text_event("final after invalid bridge args"))],
    ]);
    let runtime = runtime_with_bridge_tool("agent-loop-invalid-bridge-continues", provider.clone());

    let result = run_default_loop(&runtime, "Search notes.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-invalid-bridge");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("schema failure should carry diagnostic")
            .code(),
        "tool_input_schema_invalid"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_stream_continues_after_invalid_bridge_tool_arguments_are_resolved() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments("call-invalid-bridge", "search_notes", json!({})),
        ))],
        vec![Ok(completed_text_event("final after invalid bridge args"))],
    ]);
    let runtime = runtime_with_bridge_tool(
        "agent-loop-stream-invalid-bridge-continues",
        provider.clone(),
    );
    let mut stream = runtime
        .run_agent_loop_stream(
            StepInput::user_text("Search notes.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
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
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        public_event_kind_names(&events),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallStarted",
            "ToolCallFinished",
            "StepStarted",
            "AssistantMessage",
            "StepCompleted",
        ]
    );

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].result().status(),
        ToolCallResultStatus::Failed
    );
}
