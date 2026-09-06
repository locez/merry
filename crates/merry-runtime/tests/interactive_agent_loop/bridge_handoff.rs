use crate::support::{
    models::{
        RecordingProvider, completed_text_event, completed_tool_call_batch_event,
        completed_tool_call_event, model_name, model_tool_call,
    },
    runtime::{session_id, wait_for_interactive_waiting},
    tools::{BarrierToolExecutor, tool_spec},
};
use merry_core::{InteractiveRunState, RuntimeEvent};
use merry_runtime::{
    AgentLoopConfig, InteractiveError, InteractiveRunMessage, InterruptReason, Runtime,
    StepContext, ToolExecutionOutcome,
};
use std::sync::Arc;
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn interactive_wait_until_closed_preserves_unresolved_bridge_handoff() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "wait-close-bridge",
            "bridge_lookup",
        )))],
        vec![Ok(completed_text_event("bridge resolved"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-wait-close-bridge"))
        .model_provider(Arc::new(provider), model_name())
        .allow_bridge_tools()
        .register_tool(merry_runtime::RegisteredTool::bridge(tool_spec(
            "bridge_lookup",
        )))
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    input
        .submit_next("use the bridge")
        .await
        .expect("input should be accepted");

    let error = timeout(Duration::from_secs(1), stream.wait_until_closed())
        .await
        .expect("wait_until_closed should reach the bridge boundary")
        .expect_err("wait_until_closed must not execute a bridge call");
    assert!(matches!(
        error,
        merry_runtime::InteractiveError::ToolInvocationsRequireMessageProtocol { count: 1, .. }
    ));

    let message = stream
        .next_message()
        .await
        .expect("preserved bridge handoff should be readable")
        .expect("interactive run should remain open");
    let merry_runtime::InteractiveRunMessage::ToolInvocations { batch } = message else {
        panic!("wait_until_closed should preserve the bridge batch");
    };
    let outcome = batch
        .calls()
        .iter()
        .map(|call| {
            (
                call.id().clone(),
                ToolExecutionOutcome::succeeded_text("bridge result"),
            )
        })
        .collect();
    stream
        .submit_tool_invocation_outcomes(batch.id(), outcome)
        .await
        .expect("preserved bridge batch should be resolvable");
    wait_for_interactive_waiting(&mut stream).await;

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream closes after handoff recovery");
}

#[tokio::test]
async fn interactive_bridge_calls_are_one_ordered_batch_and_require_completion() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("bridge-call-1", "bridge_lookup"),
            model_tool_call("bridge-call-2", "bridge_lookup"),
        ]))],
        vec![Ok(completed_text_event("bridge complete"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-bridge-batch"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_bridge_tools()
        .register_tool(merry_runtime::RegisteredTool::bridge(tool_spec(
            "bridge_lookup",
        )))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    assert!(matches!(
        stream
            .next_message()
            .await
            .expect("initial message should be readable"),
        Some(InteractiveRunMessage::Event(
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ))
    ));

    input
        .submit_next("use both bridge calls")
        .await
        .expect("input queued");

    let (batch_id, calls) = timeout(Duration::from_secs(1), async {
        loop {
            match stream
                .next_message()
                .await
                .expect("interactive bridge message should be readable")
            {
                Some(InteractiveRunMessage::Event(_)) => {}
                Some(InteractiveRunMessage::ToolInvocations { batch }) => {
                    break (batch.id().clone(), batch.calls().to_vec());
                }
                None => panic!("interactive run closed before bridge calls"),
                _ => panic!("unsupported interactive message"),
            }
        }
    })
    .await
    .expect("bridge request should be emitted");
    assert_eq!(
        calls
            .iter()
            .map(|call| call.id().as_str())
            .collect::<Vec<_>>(),
        ["bridge-call-1", "bridge-call-2"]
    );
    assert!(matches!(
        stream.next_message().await,
        Err(InteractiveError::ToolInvocationsPending { .. })
    ));

    let outcomes = calls
        .iter()
        .rev()
        .map(|call| {
            (
                call.id().clone(),
                ToolExecutionOutcome::succeeded_text(format!("result for {}", call.id())),
            )
        })
        .collect();
    stream
        .submit_tool_invocation_outcomes(&batch_id, outcomes)
        .await
        .expect("complete bridge result batch should be accepted");

    let observed = timeout(Duration::from_secs(1), async {
        let mut finished = 0;
        let mut saw_answer = false;
        loop {
            let Some(message) = stream
                .next_message()
                .await
                .expect("interactive bridge continuation should be readable")
            else {
                break;
            };
            let InteractiveRunMessage::Event(event) = message else {
                panic!("bridge request was emitted before the previous batch completed");
            };
            match event {
                RuntimeEvent::ToolCallFinished { .. } => finished += 1,
                RuntimeEvent::AssistantMessage { .. } => saw_answer = true,
                RuntimeEvent::InteractiveRunStateChanged {
                    state: InteractiveRunState::WaitingForInput,
                } if saw_answer => return finished,
                _ => {}
            }
        }
        finished
    })
    .await
    .expect("interactive bridge continuation should complete");
    assert_eq!(observed, 2);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].batch_continuations()[0]
            .results()
            .iter()
            .map(|result| result.call_id().as_str())
            .collect::<Vec<_>>(),
        ["bridge-call-1", "bridge-call-2"]
    );

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream closes");
}

#[tokio::test]
async fn interactive_next_event_preserves_bridge_handoff_for_message_protocol() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("bridge-event-1", "bridge_lookup"),
            model_tool_call("bridge-event-2", "bridge_lookup"),
        ]))],
        vec![Ok(completed_text_event("bridge event complete"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-event-bridge-recovery"))
        .model_provider(Arc::new(provider), model_name())
        .allow_bridge_tools()
        .register_tool(merry_runtime::RegisteredTool::bridge(tool_spec(
            "bridge_lookup",
        )))
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();

    assert!(matches!(
        stream
            .next_event()
            .await
            .expect("initial event should be readable"),
        Some(RuntimeEvent::InteractiveRunStateChanged {
            state: merry_core::InteractiveRunState::WaitingForInput
        })
    ));
    input
        .submit_next("use bridge tools through message recovery")
        .await
        .expect("input queued");

    let handoff_error = timeout(Duration::from_secs(1), async {
        loop {
            match stream.next_event().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("interactive run closed before bridge handoff"),
                Err(error) => break error,
            }
        }
    })
    .await
    .expect("next_event should report the bridge handoff promptly");
    assert!(matches!(
        handoff_error,
        InteractiveError::ToolInvocationsRequireMessageProtocol { count: 2, .. }
    ));

    let message = stream
        .next_message()
        .await
        .expect("preserved bridge handoff should be readable")
        .expect("interactive run should remain open");
    let InteractiveRunMessage::ToolInvocations { batch } = message else {
        panic!("message recovery should return the preserved bridge batch");
    };
    assert_eq!(
        batch
            .calls()
            .iter()
            .map(|call| call.id().as_str())
            .collect::<Vec<_>>(),
        ["bridge-event-1", "bridge-event-2"]
    );
    let outcomes = batch
        .calls()
        .iter()
        .map(|call| {
            (
                call.id().clone(),
                ToolExecutionOutcome::succeeded_text(format!("result for {}", call.id())),
            )
        })
        .collect();
    stream
        .submit_tool_invocation_outcomes(batch.id(), outcomes)
        .await
        .expect("recovered bridge batch should be accepted");

    let mut saw_answer = false;
    while let Some(message) = timeout(Duration::from_secs(1), stream.next_message())
        .await
        .expect("bridge continuation should complete")
        .expect("interactive stream should remain healthy")
    {
        let InteractiveRunMessage::Event(event) = message else {
            panic!("a second bridge handoff must not be emitted");
        };
        match event {
            RuntimeEvent::AssistantMessage { .. } => saw_answer = true,
            RuntimeEvent::InteractiveRunStateChanged {
                state: merry_core::InteractiveRunState::WaitingForInput,
            } if saw_answer => break,
            _ => {}
        }
    }
    assert!(saw_answer);

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream closes");
}

#[tokio::test]
async fn interactive_mixed_model_batch_keeps_runtime_and_bridge_ownership_separate() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("native-call", "native_lookup"),
            model_tool_call("bridge-call", "bridge_lookup"),
        ]))],
        vec![Ok(completed_text_event("mixed tools complete"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-mixed-tool-batch"))
        .model_provider(Arc::new(provider), model_name())
        .allow_bridge_tools()
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("native_lookup"),
            Arc::new(BarrierToolExecutor::new(1)),
        ))
        .register_tool(merry_runtime::RegisteredTool::bridge(tool_spec(
            "bridge_lookup",
        )))
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();

    let _ = stream
        .next_message()
        .await
        .expect("initial message should be readable");
    input
        .submit_next("use one native and one bridge tool")
        .await
        .expect("input queued");

    let mut native_finished = false;
    let (batch_id, calls) = loop {
        let message = timeout(Duration::from_secs(1), stream.next_message())
            .await
            .expect("mixed tool run should not deadlock")
            .expect("interactive stream should remain healthy")
            .expect("interactive stream should remain open");
        match message {
            InteractiveRunMessage::Event(event) => {
                if let RuntimeEvent::ToolCallFinished { result, .. } = event
                    && result.call_id().as_str() == "native-call"
                {
                    native_finished = true;
                }
            }
            InteractiveRunMessage::ToolInvocations { batch } => {
                assert!(native_finished, "native runtime tool must settle first");
                break (batch.id().clone(), batch.calls().to_vec());
            }
            _ => panic!("unexpected future interactive message variant"),
        }
    };
    assert_eq!(
        calls
            .iter()
            .map(|call| call.id().as_str())
            .collect::<Vec<_>>(),
        ["bridge-call"]
    );
    stream
        .submit_tool_invocation_outcomes(
            &batch_id,
            vec![(
                calls[0].id().clone(),
                ToolExecutionOutcome::succeeded_text("bridge result"),
            )],
        )
        .await
        .expect("bridge result should be accepted");

    let mut bridge_finished = false;
    let mut saw_answer = false;
    while let Some(message) = timeout(Duration::from_secs(1), stream.next_message())
        .await
        .expect("mixed tool continuation should not deadlock")
        .expect("interactive stream should remain healthy")
    {
        let InteractiveRunMessage::Event(event) = message else {
            panic!("mixed tool run emitted a second bridge handoff");
        };
        match event {
            RuntimeEvent::ToolCallFinished { result, .. }
                if result.call_id().as_str() == "bridge-call" =>
            {
                bridge_finished = true
            }
            RuntimeEvent::AssistantMessage { text, .. } if text == "mixed tools complete" => {
                saw_answer = true;
            }
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if saw_answer => break,
            _ => {}
        }
    }
    assert!(native_finished);
    assert!(bridge_finished);
    assert!(saw_answer);

    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream closes");
}

#[tokio::test]
async fn interrupt_settles_pending_interactive_bridge_calls() {
    let provider = RecordingProvider::new_with_steps(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("bridge-call-interrupt", "bridge_lookup"),
    ))]]);
    let runtime = Runtime::builder(session_id("interactive-bridge-interrupt"))
        .model_provider(Arc::new(provider), model_name())
        .allow_bridge_tools()
        .register_tool(merry_runtime::RegisteredTool::bridge(tool_spec(
            "bridge_lookup",
        )))
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_message()
        .await
        .expect("initial state should be readable");
    input
        .submit_next("interrupt bridge")
        .await
        .expect("input queued");

    loop {
        match stream
            .next_message()
            .await
            .expect("bridge request should be readable")
        {
            Some(InteractiveRunMessage::ToolInvocations { .. }) => break,
            Some(InteractiveRunMessage::Event(_)) => {}
            None => panic!("interactive run closed before bridge request"),
            _ => panic!("unsupported interactive message"),
        }
    }
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt should be accepted");

    let mut saw_waiting = false;
    while let Some(message) = timeout(Duration::from_secs(1), stream.next_message())
        .await
        .expect("interrupt settlement should complete")
        .expect("interactive stream should remain healthy")
    {
        let InteractiveRunMessage::Event(event) = message else {
            panic!("interrupt settlement must not emit another bridge request");
        };
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            saw_waiting = true;
            break;
        }
    }
    assert!(saw_waiting);
    assert!(runtime.pending_tool_calls().await.is_empty());
    let after_interrupt = timeout(Duration::from_millis(100), stream.next_message()).await;
    assert!(!matches!(
        after_interrupt,
        Ok(Err(InteractiveError::ToolInvocationsPending { .. }))
    ));
    control.close().await.expect("interactive run closes");
    stream
        .wait_until_closed()
        .await
        .expect("interactive stream closes");
}
