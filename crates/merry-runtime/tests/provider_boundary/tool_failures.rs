use crate::support::{
    events::{event_kind_names, pending_tool_call, resolved_tool_result},
    models::{ScriptedModelProvider, completed_outputs_event, model_name, model_tool_call_with_id},
    runtime::{collect_step, runtime_with_registered_tool, session_id},
    tools::{ScriptedToolExecutor, ToolExecutorResponse, test_tool_spec},
};
use merry_core::{PendingToolCall, ToolCallResult, ToolOutput, ToolOutputKind};
use merry_llm::{FinishReason, ModelOutput};
use merry_runtime::{
    ContextSummary, LedgerFactKind, LedgerProjection, RegisteredTool, Runtime,
    ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ReentrantMutationExecutor {
    runtime: Arc<Mutex<Option<Runtime>>>,
    observations: Arc<Mutex<Vec<ReentrantMutationObservation>>>,
}

impl ReentrantMutationExecutor {
    fn new() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(None)),
            observations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn set_runtime(&self, runtime: Runtime) {
        *self
            .runtime
            .lock()
            .expect("reentrant runtime mutex should not be poisoned") = Some(runtime);
    }

    fn observations(&self) -> Vec<ReentrantMutationObservation> {
        self.observations
            .lock()
            .expect("reentrant observations mutex should not be poisoned")
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReentrantMutationObservation {
    RecordStepAlreadyActive,
    SubmitStepAlreadyActive,
    Other(String),
}

impl ToolExecutor for ReentrantMutationExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let runtime = self
                .runtime
                .lock()
                .expect("reentrant runtime mutex should not be poisoned")
                .clone()
                .expect("runtime should be installed before executor runs");
            let summary =
                ContextSummary::new("executor-inner-summary", "inner summary must not record")
                    .expect("valid summary");
            let record_observation = match runtime.record_context_summary(summary).await {
                Ok(_) => ReentrantMutationObservation::Other(
                    "record_context_summary unexpectedly succeeded".to_owned(),
                ),
                Err(merry_runtime::RuntimeError::StepAlreadyActive { .. }) => {
                    ReentrantMutationObservation::RecordStepAlreadyActive
                }
                Err(error) => ReentrantMutationObservation::Other(error.to_string()),
            };
            self.observations
                .lock()
                .expect("reentrant observations mutex should not be poisoned")
                .push(record_observation);

            let result = ToolCallResult::succeeded(call.id().clone());
            let submit_observation = match runtime
                .submit_tool_result(result, ToolOutput::text("inner result must not resolve\n"))
                .await
            {
                Ok(_) => ReentrantMutationObservation::Other(
                    "submit_tool_result unexpectedly succeeded".to_owned(),
                ),
                Err(merry_runtime::RuntimeError::StepAlreadyActive { .. }) => {
                    ReentrantMutationObservation::SubmitStepAlreadyActive
                }
                Err(error) => ReentrantMutationObservation::Other(error.to_string()),
            };
            self.observations
                .lock()
                .expect("reentrant observations mutex should not be poisoned")
                .push(submit_observation);

            Ok(ToolExecutionOutcome::succeeded_text(
                "outer executor result\n",
            ))
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn executor_infrastructure_error_keeps_pending_without_artifact_or_result() {
    let call = model_tool_call_with_id("call-infra-failure");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::infrastructure_error("temporary executor outage");
    let runtime =
        runtime_with_registered_tool("provider-execute-tool-infra-error", provider, executor);
    let pending_events = collect_step(&runtime, "Search notes.").await;
    assert_eq!(
        event_kind_names(&pending_events),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    assert_eq!(
        pending_events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;
    assert_eq!(
        before.entries(),
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
                sequence: 3,
                order: 2,
                kind: LedgerFactKind::ToolCallPending,
            },
        ]
    );

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("infrastructure failure should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::ToolExecutionFailed { call_id, .. }
            if call_id == *pending.id()
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending.clone()]);
    assert_eq!(
        runtime
            .read_tool_output(pending.id())
            .await
            .expect("read tool output"),
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_blank_text_outcome_keeps_pending_without_output_or_result() {
    let call = model_tool_call_with_id("call-blank-text-outcome");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::succeeding_text(" \n\t ");
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-blank-text-outcome",
        provider,
        executor,
    );
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("blank executor text should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            call_id,
            output_kind: ToolOutputKind::Text
        } if call_id == *pending.id()
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending.clone()]);
    assert_eq!(
        runtime
            .read_tool_output(pending.id())
            .await
            .expect("read tool output"),
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn execute_tool_blank_json_outcome_keeps_pending_without_output_or_result() {
    let call = model_tool_call_with_id("call-blank-json-outcome");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ScriptedToolExecutor::new(ToolExecutorResponse::Outcome(Box::new(
        ToolExecutionOutcome::succeeded_json(" \n\t "),
    )));
    let runtime = runtime_with_registered_tool(
        "provider-execute-tool-blank-json-outcome",
        provider,
        executor,
    );
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();
    let before = runtime.ledger_projection().await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("blank executor JSON should not resolve the pending call");
    let after = runtime.ledger_projection().await;

    assert!(matches!(
        err,
        merry_runtime::RuntimeError::UnsupportedToolResultContent {
            call_id,
            output_kind: ToolOutputKind::Json
        } if call_id == *pending.id()
    ));
    assert_eq!(before, after);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending.clone()]);
    assert_eq!(
        runtime
            .read_tool_output(pending.id())
            .await
            .expect("read tool output"),
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn executor_reentrant_runtime_mutations_are_rejected_while_outer_execution_resolves() {
    let call = model_tool_call_with_id("call-reentrant-executor");
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_outputs_event(
        vec![ModelOutput::tool_call(call)],
        FinishReason::ToolCalls,
    ))]]);
    let executor = ReentrantMutationExecutor::new();
    let runtime = Runtime::builder(session_id("provider-execute-tool-reentrant"))
        .register_tool(RegisteredTool::read_only(
            test_tool_spec("search_notes"),
            Arc::new(executor.clone()),
        ))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime should build");
    executor.set_runtime(runtime.clone());
    let pending_events = collect_step(&runtime, "Search notes.").await;
    let pending = pending_tool_call(&pending_events).clone();

    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("outer registered executor result should resolve");

    assert_eq!(
        executor.observations(),
        [
            ReentrantMutationObservation::RecordStepAlreadyActive,
            ReentrantMutationObservation::SubmitStepAlreadyActive,
        ]
    );
    assert_eq!(event_kind_names(&execution_events), ["ToolCallResolved"]);
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.call_id(), pending.id());
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(
        runtime
            .read_tool_output(result.call_id())
            .await
            .expect("outer executor result output read"),
        Some(ToolOutput::text("outer executor result\n"))
    );
    assert!(
        !merry_runtime::ContextCompiler::new()
            .compile(&runtime.context_snapshot().await)
            .expect("context should compile")
            .to_snapshot()
            .contains("executor-inner-summary")
    );
}
