use futures_util::StreamExt;
use merry_core::{
    ArtifactId, EvidenceLocator, PendingToolCall, RuntimeEvent, RuntimeEventKind, SessionId,
    ToolCallId, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelEvent, ModelName, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments, testing::FakeModelProvider,
};
use merry_runtime::{
    ArtifactError, LedgerFactKind, LedgerProjection, RegisteredTool, Runtime, RuntimeError,
    StepContext, StepInput, ToolActionKind, ToolExecutionContext, ToolExecutionError,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use schemars::Schema;
use serde_json::json;
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

fn session_id() -> SessionId {
    SessionId::new("cancel-session").expect("valid session id")
}

fn tool_call_id(value: &str) -> ToolCallId {
    ToolCallId::new(value).expect("valid tool call id")
}

fn artifact_id(value: &str) -> ArtifactId {
    ArtifactId::new(value).expect("valid artifact id")
}

fn tool_spec() -> ToolSpec {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("test schema should be a JSON schema");
    ToolSpec::new(
        ToolName::new("wait_tool").expect("valid tool name"),
        "Waits for cancellation",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

fn pending_model_tool_call(call_id: &str) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(call_id).expect("valid model tool call id"),
        ToolName::new("wait_tool").expect("valid tool name"),
        ToolArguments::new(Default::default()),
    )
}

fn pending_tool_response(call_id: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(pending_model_tool_call(call_id))],
            FinishReason::ToolCalls,
            None,
        ),
    }
}

async fn collect_pending_step(runtime: &Runtime, text: &str) -> Vec<RuntimeEvent> {
    runtime
        .step(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start")
        .collect()
        .await
}

fn event_kind_names(events: &[RuntimeEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event.kind {
            RuntimeEventKind::SessionStarted => "SessionStarted",
            RuntimeEventKind::StepStarted => "StepStarted",
            RuntimeEventKind::StepCompleted => "StepCompleted",
            RuntimeEventKind::Cancelled { .. } => "Cancelled",
            RuntimeEventKind::Failed { .. } => "Failed",
            RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
            RuntimeEventKind::EvidenceReferenced { .. } => "EvidenceReferenced",
            RuntimeEventKind::ToolCallPending { .. } => "ToolCallPending",
            RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
            _ => "Unknown",
        })
        .collect()
}

async fn assert_missing_tool_result_artifact(runtime: &Runtime) {
    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("cancelled tool execution must not record runtime-owned tool result artifact");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
}

fn runtime_with_waiting_tool(session: &str, executor: impl ToolExecutor + 'static) -> Runtime {
    Runtime::builder(SessionId::new(session).expect("valid session id"))
        .register_tool(RegisteredTool::read_only(tool_spec(), Arc::new(executor)))
        .model_provider(
            Arc::new(FakeModelProvider::new(vec![Ok(pending_tool_response(
                "cancel-tool-call",
            ))])),
            model_name(),
        )
        .build()
        .expect("runtime should build")
}

fn runtime_with_waiting_tool_action(
    session: &str,
    executor: impl ToolExecutor + 'static,
    action_kind: ToolActionKind,
) -> Runtime {
    Runtime::builder(SessionId::new(session).expect("valid session id"))
        .register_tool(RegisteredTool::new(
            tool_spec(),
            Arc::new(executor),
            action_kind,
        ))
        .model_provider(
            Arc::new(FakeModelProvider::new(vec![Ok(pending_tool_response(
                "cancel-tool-call",
            ))])),
            model_name(),
        )
        .build()
        .expect("runtime should build")
}

#[derive(Clone)]
struct WaitingToolExecutor {
    calls: Arc<AtomicUsize>,
}

impl WaitingToolExecutor {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ToolExecutor for WaitingToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            context.cancellation_token().cancelled().await;
            Err(ToolExecutionError::Cancelled)
        })
    }
}

#[derive(Clone)]
struct CancellingSuccessfulToolExecutor {
    calls: Arc<AtomicUsize>,
}

impl CancellingSuccessfulToolExecutor {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ToolExecutor for CancellingSuccessfulToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            context.cancellation_token().cancel();
            Ok(ToolExecutionOutcome::succeeded_text("ok\n"))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_step_emits_only_cancelled() {
    let runtime = Runtime::builder(session_id())
        .build()
        .expect("runtime should build");
    let token = CancellationToken::new();
    token.cancel();

    let events = runtime
        .step(
            StepInput::user_text("hello").expect("valid step input"),
            StepContext::new(token),
        )
        .expect("pre-cancelled step should return a stream")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sequence, 0);
    match &events[0].kind {
        RuntimeEventKind::Cancelled { diagnostic } => {
            assert_eq!(diagnostic.code(), "cancelled");
        }
        other => panic!("expected Cancelled event, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_step_is_rejected() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");

    let first_stream = runtime
        .step(
            StepInput::user_text("first").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("first step should start");

    let err = match runtime.step(
        StepInput::user_text("second").expect("valid step input"),
        StepContext::new(CancellationToken::new()),
    ) {
        Ok(_) => panic!("second step unexpectedly started"),
        Err(err) => err,
    };

    assert!(matches!(err, RuntimeError::StepAlreadyActive { .. }));
    drop(first_stream);
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_call_is_rejected_while_step_is_active() {
    let executor = WaitingToolExecutor::new();
    let runtime = Runtime::builder(session_id())
        .register_tool(RegisteredTool::read_only(tool_spec(), Arc::new(executor)))
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");
    let stream = runtime
        .step(
            StepInput::user_text("hold active step").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("step should start");
    tokio::task::yield_now().await;

    let err = runtime
        .execute_tool_call(
            &tool_call_id("cancel-tool-call"),
            ToolExecutionContext::default(),
        )
        .await
        .expect_err("tool execution should be rejected while a step is active");

    assert!(matches!(err, RuntimeError::StepAlreadyActive { .. }));
    drop(stream);
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_tool_execution_keeps_pending_and_releases_active_permit() {
    let executor = WaitingToolExecutor::new();
    let runtime = runtime_with_waiting_tool("cancel-tool-pre", executor.clone());
    let events = collect_pending_step(&runtime, "request wait tool").await;
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let projection_before_cancel = runtime.ledger_projection().await;
    assert_eq!(
        projection_before_cancel.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
        ]
    );

    let token = CancellationToken::new();
    token.cancel();
    let err = runtime
        .execute_tool_call(
            &tool_call_id("cancel-tool-call"),
            ToolExecutionContext::new(token),
        )
        .await
        .expect_err("pre-cancelled tool execution should be rejected");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. }
            if call_id == tool_call_id("cancel-tool-call")
    ));
    assert_eq!(executor.call_count(), 0);
    assert_eq!(runtime.pending_tool_calls().await.len(), 1);
    assert_eq!(runtime.ledger_projection().await, projection_before_cancel);
    assert_missing_tool_result_artifact(&runtime).await;
    let follow_up_events = start_step_after_cleanup(&runtime, "after pre-cancel").await;
    assert!(
        follow_up_events
            .iter()
            .any(|event| matches!(event.kind, RuntimeEventKind::Failed { .. }))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_workspace_write_tool_execution_keeps_pending_without_denial_artifact() {
    let executor = WaitingToolExecutor::new();
    let runtime = runtime_with_waiting_tool_action(
        "cancel-tool-pre-policy-denied",
        executor.clone(),
        ToolActionKind::WorkspaceWrite,
    );
    let events = collect_pending_step(&runtime, "request wait tool").await;
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let projection_before_cancel = runtime.ledger_projection().await;

    let token = CancellationToken::new();
    token.cancel();
    let err = runtime
        .execute_tool_call(
            &tool_call_id("cancel-tool-call"),
            ToolExecutionContext::new(token),
        )
        .await
        .expect_err("pre-cancelled policy-denied tool execution should be rejected");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. }
            if call_id == tool_call_id("cancel-tool-call")
    ));
    assert_eq!(executor.call_count(), 0);
    assert_eq!(runtime.pending_tool_calls().await.len(), 1);
    assert_eq!(runtime.ledger_projection().await, projection_before_cancel);
    assert_missing_tool_result_artifact(&runtime).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_during_tool_execution_keeps_pending_and_releases_active_permit() {
    let executor = WaitingToolExecutor::new();
    let runtime = runtime_with_waiting_tool("cancel-tool-during", executor.clone());
    let events = collect_pending_step(&runtime, "request wait tool").await;
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let projection_before_cancel = runtime.ledger_projection().await;
    assert_eq!(
        projection_before_cancel.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
        ]
    );
    let token = CancellationToken::new();
    let execute_runtime = runtime.clone();
    let execute_token = token.clone();

    let handle = tokio::spawn(async move {
        execute_runtime
            .execute_tool_call(
                &tool_call_id("cancel-tool-call"),
                ToolExecutionContext::new(execute_token),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(executor.call_count(), 1);
    token.cancel();

    let err = handle
        .await
        .expect("tool execution task should not panic")
        .expect_err("cancelled tool execution should return an error");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. }
            if call_id == tool_call_id("cancel-tool-call")
    ));
    assert_eq!(runtime.pending_tool_calls().await.len(), 1);
    assert_eq!(runtime.ledger_projection().await, projection_before_cancel);
    assert_missing_tool_result_artifact(&runtime).await;
    let follow_up_events = start_step_after_cleanup(&runtime, "after during-cancel").await;
    assert!(
        follow_up_events
            .iter()
            .any(|event| matches!(event.kind, RuntimeEventKind::Failed { .. }))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_after_successful_tool_execution_keeps_pending_and_releases_active_permit() {
    let executor = CancellingSuccessfulToolExecutor::new();
    let runtime = runtime_with_waiting_tool("cancel-tool-after-success", executor.clone());
    let events = collect_pending_step(&runtime, "request wait tool").await;
    assert_eq!(
        event_kind_names(&events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    let pending_before_cancel = runtime.pending_tool_calls().await;
    assert_eq!(pending_before_cancel.len(), 1);
    let projection_before_cancel = runtime.ledger_projection().await;
    assert_eq!(
        projection_before_cancel.entries(),
        [
            LedgerProjection::Lifecycle {
                sequence: 0,
                order: 0,
                kind: LedgerFactKind::SessionStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 1,
                order: 1,
                kind: LedgerFactKind::StepStarted,
            },
            LedgerProjection::Lifecycle {
                sequence: 2,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
        ]
    );

    let err = runtime
        .execute_tool_call(
            &tool_call_id("cancel-tool-call"),
            ToolExecutionContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("late-cancelled successful tool execution should return an error");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. }
            if call_id == tool_call_id("cancel-tool-call")
    ));
    assert_eq!(executor.call_count(), 1);
    assert_eq!(runtime.pending_tool_calls().await, pending_before_cancel);
    assert_eq!(runtime.ledger_projection().await, projection_before_cancel);
    assert_missing_tool_result_artifact(&runtime).await;
    let follow_up_events = start_step_after_cleanup(&runtime, "after late-cancel").await;
    assert!(
        follow_up_events
            .iter()
            .any(|event| matches!(event.kind, RuntimeEventKind::Failed { .. }))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_full_stream_releases_active_step_after_producer_stops() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");
    let token = CancellationToken::new();

    let mut first_stream = runtime
        .step(
            StepInput::user_text("first").expect("valid step input"),
            StepContext::new(token.clone()),
        )
        .expect("first step should start");
    tokio::task::yield_now().await;

    let err = match runtime.step(
        StepInput::user_text("second").expect("valid step input"),
        StepContext::new(CancellationToken::new()),
    ) {
        Ok(_) => panic!("second step unexpectedly started while first producer is active"),
        Err(err) => err,
    };
    assert!(matches!(err, RuntimeError::StepAlreadyActive { .. }));

    token.cancel();
    let old_events = first_stream.by_ref().collect::<Vec<_>>().await;
    assert_eq!(old_events.len(), 2);
    assert!(matches!(
        old_events[0].kind,
        RuntimeEventKind::SessionStarted
    ));
    assert!(matches!(
        old_events[1].kind,
        RuntimeEventKind::Cancelled { .. }
    ));

    let second_events = runtime
        .step(
            StepInput::user_text("second").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("second step should start after cancellation cleanup")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        second_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(matches!(
        second_events[0].kind,
        RuntimeEventKind::StepStarted
    ));
    assert!(matches!(
        second_events[1].kind,
        RuntimeEventKind::StepCompleted
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_full_stream_eventually_emits_cancelled() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");
    let token = CancellationToken::new();

    let mut stream = runtime
        .step(
            StepInput::user_text("cancel with full stream").expect("valid step input"),
            StepContext::new(token.clone()),
        )
        .expect("step should start");
    tokio::task::yield_now().await;

    token.cancel();

    let events = stream.by_ref().collect::<Vec<_>>().await;

    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(matches!(events[0].kind, RuntimeEventKind::SessionStarted));
    assert!(matches!(events[1].kind, RuntimeEventKind::Cancelled { .. }));
}

async fn start_step_after_cleanup(runtime: &Runtime, text: &str) -> Vec<merry_core::RuntimeEvent> {
    for _ in 0..8 {
        tokio::task::yield_now().await;

        match runtime.step(
            StepInput::user_text(text).expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        ) {
            Ok(stream) => return stream.collect::<Vec<_>>().await,
            Err(RuntimeError::StepAlreadyActive { .. }) => continue,
            Err(err) => panic!("unexpected step error after cleanup: {err}"),
        }
    }

    panic!("producer did not release active step after cancellation cleanup");
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_full_stream_keeps_step_active_until_producer_stops() {
    let runtime = Runtime::builder(session_id())
        .event_buffer_size(NonZeroUsize::new(1).expect("non-zero buffer"))
        .build()
        .expect("runtime should build");

    let first_stream = runtime
        .step(
            StepInput::user_text("first").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
        )
        .expect("first step should start");
    tokio::task::yield_now().await;
    drop(first_stream);

    let err = match runtime.step(
        StepInput::user_text("second").expect("valid step input"),
        StepContext::new(CancellationToken::new()),
    ) {
        Ok(_) => panic!("second step started before dropped producer stopped"),
        Err(err) => err,
    };
    assert!(matches!(err, RuntimeError::StepAlreadyActive { .. }));

    let second_events = start_step_after_cleanup(&runtime, "second").await;

    assert_eq!(
        second_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(matches!(
        second_events[0].kind,
        RuntimeEventKind::StepStarted
    ));
    assert!(matches!(
        second_events[1].kind,
        RuntimeEventKind::StepCompleted
    ));
}
