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
        vec![0, 1, 3, 4, 5, 7, 8]
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
    assert_eq!(requests[0].messages()[2].role(), ModelMessageRole::User);
    assert_eq!(
        requests[0].messages()[2].content().as_text(),
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
            "ToolCallResolved",
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

#[derive(Clone)]
struct ConcurrencyCountingProcessRunner {
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    max_observed: Arc<std::sync::atomic::AtomicUsize>,
    two_active_barrier: Arc<Barrier>,
    started: Arc<std::sync::atomic::AtomicUsize>,
}

impl merry_runtime::ProcessRunner for ConcurrencyCountingProcessRunner {
    fn run<'a>(
        &'a self,
        intent: merry_runtime::ProcessActionIntent,
        _context: merry_runtime::ProcessRunnerContext,
    ) -> merry_runtime::ProcessRunnerFuture<'a> {
        let in_flight = Arc::clone(&self.in_flight);
        let max_observed = Arc::clone(&self.max_observed);
        let barrier = Arc::clone(&self.two_active_barrier);
        let started = Arc::clone(&self.started);
        Box::pin(async move {
            let current = in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            max_observed.fetch_max(current, std::sync::atomic::Ordering::SeqCst);
            let order = started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if order < 2 {
                barrier.wait().await;
            }
            in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Ok(merry_runtime::ProcessRunnerOutput::new(
                &intent,
                merry_runtime::ProcessExitStatus::Exited(0),
                format!("ran {:?}", intent.argv()),
                false,
                String::new(),
                false,
            )
            .expect("valid process output"))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_runs_run_process_calls_in_parallel_and_queues_above_limit() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            merry_llm::ModelToolCall::new(
                merry_llm::ModelToolCallId::new("proc-1").expect("valid id"),
                merry_core::ToolName::new("run_process").expect("valid tool"),
                merry_llm::ToolArguments::try_from(
                    serde_json::json!({ "command": "echo one", "cwd": null }),
                )
                .expect("valid args"),
            ),
            merry_llm::ModelToolCall::new(
                merry_llm::ModelToolCallId::new("proc-2").expect("valid id"),
                merry_core::ToolName::new("run_process").expect("valid tool"),
                merry_llm::ToolArguments::try_from(
                    serde_json::json!({ "command": "echo two", "cwd": null }),
                )
                .expect("valid args"),
            ),
            merry_llm::ModelToolCall::new(
                merry_llm::ModelToolCallId::new("proc-3").expect("valid id"),
                merry_core::ToolName::new("run_process").expect("valid tool"),
                merry_llm::ToolArguments::try_from(
                    serde_json::json!({ "command": "echo three", "cwd": null }),
                )
                .expect("valid args"),
            ),
        ]))],
        vec![Ok(completed_text_event("all three done"))],
    ]);
    let runner = Arc::new(ConcurrencyCountingProcessRunner {
        in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_observed: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        two_active_barrier: Arc::new(Barrier::new(2)),
        started: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    });
    let process_tool = merry_runtime::process_command_tool(
        merry_core::ToolName::new("run_process").expect("valid name"),
        "Run shell command",
    )
    .expect("process tool should build");
    let runtime = Runtime::builder(session_id("agent-loop-parallel-run-process"))
        .register_tool(process_tool)
        .allow_read_only_shell_process_actions(runner.clone())
        .allow_accepted_local_workspace_process_actions(
            merry_runtime::AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            runner.clone(),
        )
        .max_parallel_tool_calls(NonZeroUsize::new(2).expect("non-zero limit"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        run_default_loop(&runtime, "Run three commands."),
    )
    .await
    .expect("two run_process calls must run concurrently and queue the third");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        runner
            .max_observed
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "concurrency must reach limit (2) and queue the 3rd call"
    );
    assert_eq!(runner.started.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[tokio::test]
async fn parallel_batch_emits_fast_tool_finished_before_slow_sibling_completes() {
    struct FastAndGatedExecutor {
        unblock_slow: Arc<tokio::sync::Notify>,
    }

    impl ToolExecutor for FastAndGatedExecutor {
        fn execute<'a>(
            &'a self,
            call: PendingToolCall,
            _context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            let unblock_slow = Arc::clone(&self.unblock_slow);
            Box::pin(async move {
                let is_slow = call.id().as_str() == "call-slow";
                if is_slow {
                    unblock_slow.notified().await;
                }
                Ok(ToolExecutionOutcome::succeeded_text(if is_slow {
                    "slow-ok"
                } else {
                    "fast-ok"
                }))
            })
        }
    }

    let unblock_slow = Arc::new(tokio::sync::Notify::new());
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-slow", "probe"),
            model_tool_call("call-fast", "probe"),
        ]))],
        vec![Ok(completed_text_event("both finished"))],
    ]);
    let runtime = Runtime::builder(session_id("agent-loop-stream-parallel-finish"))
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("probe"),
                Arc::new(FastAndGatedExecutor {
                    unblock_slow: Arc::clone(&unblock_slow),
                }),
            )
            .with_parallel_safe_execution(),
        )
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");

    let mut stream = runtime
        .run_agent_loop_stream(
            merry_runtime::StepInput::user_text("Run slow and fast calls concurrently.")
                .expect("valid input"),
            merry_runtime::StepContext::new(tokio_util::sync::CancellationToken::new()),
            merry_runtime::AgentLoopConfig::default(),
        )
        .expect("stream should start");

    let mut saw_fast_finished = false;
    let mut saw_slow_finished = false;
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Ok(Some(message)) = stream.next_message().await {
            if let merry_runtime::AgentRunMessage::Event(
                merry_core::RuntimeEvent::ToolCallFinished { result, .. },
            ) = message
            {
                if result.call_id().as_str() == "call-fast" {
                    saw_fast_finished = true;
                    assert!(
                        !saw_slow_finished,
                        "fast call must finish and emit before slow sibling is unblocked"
                    );
                    unblock_slow.notify_one();
                } else if result.call_id().as_str() == "call-slow" {
                    saw_slow_finished = true;
                }
            }
        }
    })
    .await
    .expect("fast tool must emit ToolCallFinished while slow sibling is still running");

    let result = stream.result().await.expect("loop should succeed");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(saw_fast_finished && saw_slow_finished);
}
