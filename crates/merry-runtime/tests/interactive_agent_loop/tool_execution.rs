use crate::support::{
    models::{
        RecordingProvider, completed_text_event, completed_tool_call_batch_event,
        completed_tool_call_event, model_name, model_tool_call,
    },
    runtime::{session_id, wait_for_interactive_waiting},
    tools::{BarrierToolExecutor, tool_spec},
};
use merry_core::{InteractiveRunState, PendingToolCall, RuntimeEvent};
use merry_runtime::{
    AgentLoopConfig, Runtime, StepContext, ToolExecutionContext, ToolExecutionError, ToolExecutor,
    ToolExecutorFuture,
};
use std::{num::NonZeroUsize, sync::Arc};
use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct InfrastructureFailingToolExecutor;

impl ToolExecutor for InfrastructureFailingToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async {
            Err(ToolExecutionError::infrastructure(
                "test executor backend is unavailable\nretry later",
            ))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_run_executes_parallel_safe_tool_batch_before_waiting() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-1", "search_notes"),
            model_tool_call("call-2", "search_notes"),
        ]))],
        vec![Ok(completed_text_event("done"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-parallel-tool-batch"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("search_notes"),
                Arc::new(BarrierToolExecutor::new(2)),
            )
            .with_parallel_safe_execution(),
        )
        .max_parallel_tool_calls(NonZeroUsize::new(2).expect("non-zero limit"))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    assert!(matches!(
        stream
            .next_event()
            .await
            .expect("stream error")
            .expect("initial waiting state"),
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));

    input
        .submit_next("search twice")
        .await
        .expect("input queued");
    let observed = timeout(Duration::from_secs(1), async {
        let mut finished = 0;
        let mut saw_answer = false;
        while let Some(event) = stream.next_event().await.expect("interactive stream error") {
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
    .expect("interactive batch should complete without serial barrier deadlock");

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
        ["call-1", "call-2"]
    );
}

#[tokio::test]
async fn interactive_tool_infrastructure_failure_returns_to_waiting() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-infrastructure-failure",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("continued after tool failure"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-tool-infrastructure-failure"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(InfrastructureFailingToolExecutor),
        ))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("start").await.expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(provider.recorded_requests().len(), 2);
}

#[tokio::test]
async fn interactive_tool_batch_infrastructure_failure_resolves_all_pending_calls() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-batch-infrastructure-1", "search_notes"),
            model_tool_call("call-batch-infrastructure-2", "search_notes"),
        ]))],
        vec![Ok(completed_text_event("continued after batch failure"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-tool-batch-infrastructure-failure"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(InfrastructureFailingToolExecutor),
        ))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input.submit_next("start").await.expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(provider.recorded_requests().len(), 2);
}
