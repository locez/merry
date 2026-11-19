use crate::{
    RuntimeError,
    runtime::{
        APPLY_PATCH_TOOL_NAME,
        tests::support::{
            process::CancellingOptInPatchExecutor,
            tool_executors::ProposingToolExecutor,
            tool_helpers::{
                assert_sanitized_policy_denial_content, denied_action_content,
                event_kind_names_for_tool_execution, policy_tool_spec,
                register_policy_pending_registered_tool_with_builder, resolved_tool_result,
            },
        },
    },
    tool::{RegisteredTool, ToolActionKind, ToolExecutionContext},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_proposal_executes() {
    let executor = ProposingToolExecutor::immediate();
    let tool = RegisteredTool::new(
        policy_tool_spec(APPLY_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-proposed-workspace-write-opt-in",
        APPLY_PATCH_TOOL_NAME,
        "call-workspace-write-opt-in",
        tool,
        |builder| builder.allow_low_risk_apply_patches().build(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("opted-in low-risk workspace patch should execute");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 1);
    assert_eq!(executor.approved_proposal_seen(), vec![true]);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Succeeded
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_proposal_rejects_non_patch_tool_name() {
    let executor = ProposingToolExecutor::immediate();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_write_opt_in"),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-proposed-workspace-write-opt-in-wrong-tool",
        "policy_write_opt_in",
        "call-workspace-write-opt-in-wrong-tool",
        tool,
        |builder| builder.allow_low_risk_apply_patches().build(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("non-patch-file low-risk proposal should resolve as policy denial");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Failed
    );
    assert_sanitized_policy_denial_content(
        &denied_action_content(&runtime, &events).await,
        "policy_write_opt_in",
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_records_outcome_when_cancelled_after_side_effect() {
    let executor = CancellingOptInPatchExecutor::new();
    let tool = RegisteredTool::new(
        policy_tool_spec(APPLY_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-workspace-write-opt-in-cancel-after-side-effect",
        APPLY_PATCH_TOOL_NAME,
        "call-workspace-write-opt-in-cancel-after-side-effect",
        tool,
        |builder| builder.allow_low_risk_apply_patches().build(),
    )
    .await;
    let token = CancellationToken::new();

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::new(token))
        .await
        .expect("successful opt-in patch execution must be durably recorded");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 1);
    assert_eq!(executor.approved_proposal_seen(), vec![true]);
    assert!(executor.side_effect_happened());
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Succeeded
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_missing_execution_evidence_fails_closed() {
    let executor = ProposingToolExecutor::missing_execution_evidence();
    let tool = RegisteredTool::new(
        policy_tool_spec(APPLY_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-workspace-write-opt-in-missing-evidence",
        APPLY_PATCH_TOOL_NAME,
        "call-workspace-write-opt-in-missing-evidence",
        tool,
        |builder| builder.allow_low_risk_apply_patches().build(),
    )
    .await;
    let sequence_before = runtime.inner.session.lock().await.next_sequence();

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("successful admitted patch without evidence must fail closed");

    assert!(matches!(
        err,
        RuntimeError::MissingActionExecutionEvidence { call_id, action_kind, .. }
            if call_id == *pending.id() && action_kind == ToolActionKind::WorkspaceWrite
    ));
    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 1);
    assert_eq!(executor.approved_proposal_seen(), vec![true]);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert_eq!(
        runtime.inner.session.lock().await.next_sequence(),
        sequence_before
    );
}
