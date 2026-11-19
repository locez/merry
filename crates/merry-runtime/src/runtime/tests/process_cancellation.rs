use crate::{
    RuntimeError,
    runtime::tests::support::{
        process::{FakeProcessRunner, ProcessProposingToolExecutor},
        tool_helpers::{
            event_kind_names_for_tool_execution, policy_tool_spec,
            register_policy_pending_registered_tool_with_builder, resolved_artifact_json,
            resolved_tool_result,
        },
    },
    tool::{RegisteredTool, ToolActionKind, ToolExecutionContext},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_commits_output_after_runner_cancels_token() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::succeeding_then_cancelling_token();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_post_output_cancel"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-post-output-cancel",
        "policy_command_post_output_cancel",
        "call-command-exec-post-output-cancel",
        tool,
        |builder| {
            builder
                .allow_low_risk_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("runner output should commit even if token is cancelled afterward");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(runtime.pending_tool_calls().await.is_empty());

    let payload = resolved_artifact_json(&runtime, result, "process result").await;
    assert_eq!(
        payload
            .pointer("/stdout/text")
            .expect("process stdout text should be present"),
        "runtime tests passed after token cancellation\n"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_pre_cancel_keeps_pending_without_result_output() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_pre_cancel"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-pre-cancel",
        "policy_command_pre_cancel",
        "call-command-exec-pre-cancel",
        tool,
        |builder| {
            builder
                .allow_low_risk_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;
    let sequence_before = runtime.inner.session.lock().await.next_sequence();
    let token = CancellationToken::new();
    token.cancel();

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::new(token))
        .await
        .expect_err("pre-cancelled process action should not resolve");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. } if call_id == *pending.id()
    ));
    assert_eq!(executor.propose_count(), 0);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 0);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending.clone()]);
    assert_eq!(
        runtime.inner.session.lock().await.next_sequence(),
        sequence_before
    );
    assert!(
        runtime
            .read_tool_output(pending.id())
            .await
            .expect("tool output query succeeds")
            .is_none(),
        "pre-cancelled process action must not record tool output"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_runner_cancel_keeps_pending_without_result_output() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::cancelling();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_runner_cancel"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-runner-cancel",
        "policy_command_runner_cancel",
        "call-command-exec-runner-cancel",
        tool,
        |builder| {
            builder
                .allow_low_risk_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;
    let sequence_before = runtime.inner.session.lock().await.next_sequence();

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("runner-cancelled process action should not resolve");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. } if call_id == *pending.id()
    ));
    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending.clone()]);
    assert_eq!(
        runtime.inner.session.lock().await.next_sequence(),
        sequence_before
    );
    assert!(
        runtime
            .read_tool_output(pending.id())
            .await
            .expect("tool output query succeeds")
            .is_none(),
        "runner-cancelled process action must not record tool output"
    );
}
