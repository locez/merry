use crate::support::{
    models::{
        RecordingProvider, completed_text_event, completed_tool_call_event, model_name,
        model_tool_call,
    },
    runtime::session_id,
    tools::tool_spec,
};
use merry_core::{InteractiveRunState, PendingToolCall, RuntimeEvent};
use merry_runtime::{
    AgentLoopConfig, InterruptReason, Runtime, StepContext, ToolExecutionContext,
    ToolExecutionError, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct BlockingToolExecutor {
    started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl ToolExecutor for BlockingToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if let Some(started) = self.started.lock().expect("started lock").take() {
                let _ = started.send(());
            }
            context.cancellation_token().cancelled().await;
            Err(ToolExecutionError::Cancelled)
        })
    }
}

#[derive(Clone)]
struct CancelFirstThenSucceedToolExecutor {
    first_started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    calls_started: Arc<Mutex<usize>>,
}

impl ToolExecutor for CancelFirstThenSucceedToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        let first_started = Arc::clone(&self.first_started);
        let calls_started = Arc::clone(&self.calls_started);
        Box::pin(async move {
            let call_index = {
                let mut calls_started = calls_started.lock().expect("calls lock");
                let call_index = *calls_started;
                *calls_started += 1;
                call_index
            };

            if call_index == 0 {
                if let Some(started) = first_started.lock().expect("started lock").take() {
                    let _ = started.send(());
                }
                context.cancellation_token().cancelled().await;
                return Err(ToolExecutionError::Cancelled);
            }

            Ok(ToolExecutionOutcome::succeeded_text("second tool ok"))
        })
    }
}

#[tokio::test]
async fn interrupt_during_tool_execution_closes_pending_tool_call() {
    let (started_tx, started_rx) = oneshot::channel();
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-blocking",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("after cancel"))],
    ]);
    let tool = BlockingToolExecutor {
        started: Arc::new(Mutex::new(Some(started_tx))),
    };
    let runtime = Runtime::builder(session_id("interactive-tool-interrupt"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(tool),
        ))
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
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("start").await.expect("start queued");
    started_rx.await.expect("tool starts");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let mut saw_resolved = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if matches!(event, RuntimeEvent::ToolCallFinished { .. }) {
            saw_resolved = true;
            break;
        }
    }
    assert!(saw_resolved);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test]
async fn new_input_after_interrupt_still_executes_runtime_tool_calls() {
    let (started_tx, started_rx) = oneshot::channel();
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-cancelled",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-after-interrupt",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("after second tool"))],
    ]);
    let tool = CancelFirstThenSucceedToolExecutor {
        first_started: Arc::new(Mutex::new(Some(started_tx))),
        calls_started: Arc::new(Mutex::new(0)),
    };
    let runtime = Runtime::builder(session_id("interactive-tool-after-interrupt"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(tool),
        ))
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
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    started_rx.await.expect("first tool starts");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let mut returned_to_waiting = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            returned_to_waiting = true;
            break;
        }
    }
    assert!(returned_to_waiting);
    assert!(runtime.pending_tool_calls().await.is_empty());

    input
        .submit_next("second")
        .await
        .expect("second queued after interrupt");

    let mut saw_second_tool_result = false;
    while let Some(event) = stream.next_event().await.expect("interactive stream error") {
        if let RuntimeEvent::ToolCallFinished { result, .. } = event
            && result.call_id().as_str() == "call-after-interrupt"
        {
            saw_second_tool_result = true;
            break;
        }
    }
    assert!(saw_second_tool_result);
    assert!(runtime.pending_tool_calls().await.is_empty());
}
