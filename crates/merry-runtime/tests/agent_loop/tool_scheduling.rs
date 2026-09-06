use crate::support::{
    events::{assert_continuation_request_body, event_kind_names},
    models::{
        ScriptedModelProvider, completed_text_event, completed_tool_call_batch_event,
        completed_tool_call_event, model_name, model_tool_call,
    },
    runtime::{run_default_loop, runtime_with_tool, session_id},
    tools::{ScriptedToolExecutor, tool_spec},
};
use merry_core::{PendingToolCall, ToolCallResultStatus};
use merry_llm::ModelMessageRole;
use merry_runtime::{
    AgentLoopStatus, Runtime, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Barrier;

#[derive(Clone)]
struct BarrierToolExecutor {
    barrier: Arc<Barrier>,
    calls: Arc<Mutex<Vec<PendingToolCall>>>,
    markers: Arc<Mutex<Vec<String>>>,
}

impl BarrierToolExecutor {
    fn new(parties: usize) -> Self {
        Self::with_markers(parties, Arc::new(Mutex::new(Vec::new())))
    }

    fn with_markers(parties: usize, markers: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            barrier: Arc::new(Barrier::new(parties)),
            calls: Arc::new(Mutex::new(Vec::new())),
            markers,
        }
    }

    fn calls(&self) -> Vec<PendingToolCall> {
        self.calls
            .lock()
            .expect("tool calls mutex should not be poisoned")
            .clone()
    }
}

impl ToolExecutor for BarrierToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .expect("tool calls mutex should not be poisoned")
                .push(call.clone());
            self.markers
                .lock()
                .expect("markers mutex should not be poisoned")
                .push(format!("start:{}", call.id()));
            self.barrier.wait().await;
            self.markers
                .lock()
                .expect("markers mutex should not be poisoned")
                .push(format!("end:{}", call.id()));
            Ok(ToolExecutionOutcome::succeeded_text(format!(
                "result for {}",
                call.id()
            )))
        })
    }
}

#[derive(Clone)]
struct MarkerToolExecutor {
    marker: &'static str,
    markers: Arc<Mutex<Vec<String>>>,
}

impl MarkerToolExecutor {
    fn new(marker: &'static str, markers: Arc<Mutex<Vec<String>>>) -> Self {
        Self { marker, markers }
    }
}

impl ToolExecutor for MarkerToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.markers
                .lock()
                .expect("markers mutex should not be poisoned")
                .push(self.marker.to_owned());
            Ok(ToolExecutionOutcome::succeeded_text(self.marker))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_executes_one_tool_and_continues_to_final_completion() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-success",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("search result\n");
    let runtime = runtime_with_tool("agent-loop-happy", provider.clone(), executor.clone());

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
    assert_eq!(
        result
            .events()
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5, 6, 7]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(executor.calls().len(), 1);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::System);
    assert!(
        requests[0].messages()[0]
            .content()
            .as_text()
            .contains("You are Merry, a software engineering agent")
    );
    assert_eq!(requests[0].messages()[1].role(), ModelMessageRole::User);
    assert_eq!(
        requests[0].messages()[1].content().as_text(),
        "Search notes."
    );
    assert!(requests[0].continuations().is_empty());
    assert_continuation_request_body(&requests[1], "Search notes.");
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].call().id().as_str(),
        "call-success"
    );
    assert_eq!(
        requests[1].continuations()[0].result().status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_executes_parallel_safe_batch_and_continues_in_model_order() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-1", "search_notes"),
            model_tool_call("call-2", "search_notes"),
        ]))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let executor = BarrierToolExecutor::new(2);
    let runtime = Runtime::builder(session_id("agent-loop-parallel-safe-batch"))
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("search_notes"),
                Arc::new(executor.clone()),
            )
            .with_parallel_safe_execution(),
        )
        .max_parallel_tool_calls(NonZeroUsize::new(2).expect("non-zero limit"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run_default_loop(&runtime, "Search notes twice."),
    )
    .await
    .expect("parallel-safe calls should reach the barrier together");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 2);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallBatchPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(executor.calls().len(), 2);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].batch_continuations().len(), 1);
    assert_eq!(
        requests[1].batch_continuations()[0]
            .results()
            .iter()
            .map(|result| result.call_id().as_str())
            .collect::<Vec<_>>(),
        ["call-1", "call-2"]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_treats_exclusive_tool_as_barrier_between_parallel_waves() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-1", "parallel_before"),
            model_tool_call("call-2", "parallel_before"),
            model_tool_call("call-3", "exclusive"),
            model_tool_call("call-4", "parallel_after"),
        ]))],
        vec![Ok(completed_text_event("final answer"))],
    ]);
    let markers = Arc::new(Mutex::new(Vec::new()));
    let before = BarrierToolExecutor::with_markers(2, Arc::clone(&markers));
    let runtime = Runtime::builder(session_id("agent-loop-exclusive-batch-barrier"))
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("parallel_before"),
                Arc::new(before),
            )
            .with_parallel_safe_execution(),
        )
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("exclusive"),
            Arc::new(MarkerToolExecutor::new("exclusive", Arc::clone(&markers))),
        ))
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("parallel_after"),
                Arc::new(MarkerToolExecutor::new("after", Arc::clone(&markers))),
            )
            .with_parallel_safe_execution(),
        )
        .max_parallel_tool_calls(NonZeroUsize::new(2).expect("non-zero limit"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run_default_loop(&runtime, "Run the mixed batch."),
    )
    .await
    .expect("parallel wave should complete before the exclusive barrier");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    let markers = markers
        .lock()
        .expect("markers mutex should not be poisoned")
        .clone();
    let exclusive_index = markers
        .iter()
        .position(|marker| marker == "exclusive")
        .expect("exclusive marker should be present");
    let after_index = markers
        .iter()
        .position(|marker| marker == "after")
        .expect("after marker should be present");
    assert_eq!(
        markers[..exclusive_index]
            .iter()
            .filter(|marker| marker.starts_with("end:"))
            .count(),
        2
    );
    assert!(after_index > exclusive_index);
}
