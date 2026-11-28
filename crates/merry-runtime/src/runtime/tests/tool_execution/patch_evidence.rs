use crate::{
    RuntimeError,
    runtime::tests::support::{
        process::CancellingOptInPatchExecutor,
        tool_executors::ProposingToolExecutor,
        tool_helpers::{
            assert_sanitized_policy_denial_content, denied_action_content,
            event_kind_names_for_tool_execution, policy_tool_spec,
            register_policy_pending_registered_tool_with_builder, resolved_tool_result,
        },
    },
    tool::{RegisteredTool, ToolActionKind, ToolExecutionContext},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const TEST_PATCH_TOOL_NAME: &str = "apply_patch";

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_proposal_executes_independently_of_tool_name() {
    for (session_name, tool_name, call_id) in [
        (
            "runtime-policy-proposed-workspace-write-opt-in",
            TEST_PATCH_TOOL_NAME,
            "call-workspace-write-opt-in",
        ),
        (
            "runtime-policy-proposed-workspace-write-custom-name",
            "custom_workspace_patch",
            "call-workspace-write-custom-name",
        ),
    ] {
        let executor = ProposingToolExecutor::immediate();
        let tool = RegisteredTool::new(
            policy_tool_spec(tool_name),
            Arc::new(executor.clone()),
            ToolActionKind::WorkspaceWrite,
        )
        .with_low_risk_workspace_patch_lane();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            session_name,
            tool_name,
            call_id,
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
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_proposal_rejects_tool_without_patch_lane_opt_in() {
    let executor = ProposingToolExecutor::immediate();
    // Even if a tool uses the `apply_patch` name, missing the typed lane opt-in must deny it.
    let tool = RegisteredTool::new(
        policy_tool_spec(TEST_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-proposed-workspace-write-opt-in-missing-lane",
        TEST_PATCH_TOOL_NAME,
        "call-workspace-write-opt-in-missing-lane",
        tool,
        |builder| builder.allow_low_risk_apply_patches().build(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("tool without patch lane opt-in should resolve as policy denial");

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
        TEST_PATCH_TOOL_NAME,
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_records_outcome_when_cancelled_after_side_effect() {
    let executor = CancellingOptInPatchExecutor::new();
    let tool = RegisteredTool::new(
        policy_tool_spec(TEST_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_low_risk_workspace_patch_lane();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-workspace-write-opt-in-cancel-after-side-effect",
        TEST_PATCH_TOOL_NAME,
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
        policy_tool_spec(TEST_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_low_risk_workspace_patch_lane();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-workspace-write-opt-in-missing-evidence",
        TEST_PATCH_TOOL_NAME,
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
