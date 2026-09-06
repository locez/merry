use crate::{
    RuntimeError,
    action_audit::ActionAuditStatus,
    action_policy::{ActionPolicyDisposition, ActionRiskTier},
    ledger::LedgerFactKind,
    runtime::{
        APPLY_PATCH_TOOL_NAME,
        tests::support::{
            process::CancellingOptInPatchExecutor,
            tool_executors::ProposingToolExecutor,
            tool_helpers::{
                action_audit_records, assert_sanitized_policy_denial_content,
                denied_action_content, event_kind_names_for_tool_execution, lifecycle_kinds,
                policy_tool_spec, register_policy_pending_registered_tool_with_builder,
                resolved_tool_result,
            },
        },
    },
    tool::{ActionExecutionEvidence, RegisteredTool, ToolActionKind, ToolExecutionContext},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_proposal_executes_and_records_execution_audit() {
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
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Succeeded
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[0].tool_call_id(), pending.id());
    assert_eq!(audits[0].tool_name(), pending.name());
    assert_eq!(audits[0].action_kind(), ToolActionKind::WorkspaceWrite);
    assert!(audits[0].policy().is_none());
    assert!(audits[0].proposal().is_some());
    assert!(audits[0].execution_evidence().is_none());

    assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
    assert_eq!(audits[1].tool_call_id(), pending.id());
    assert_eq!(audits[1].tool_name(), pending.name());
    assert_eq!(audits[1].action_kind(), ToolActionKind::WorkspaceWrite);
    assert!(audits[1].proposal().is_none());
    let policy = audits[1]
        .policy()
        .expect("executed audit should include allow policy");
    assert_eq!(policy.risk_tier(), ActionRiskTier::EditLow);
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
    let ActionExecutionEvidence::WorkspacePatch(evidence) = audits[1]
        .execution_evidence()
        .expect("executed audit should include actual evidence")
    else {
        panic!("workspace patch execution should record workspace patch evidence");
    };
    assert_eq!(evidence.relative_path(), "notes/proposed.txt");
    assert_eq!(evidence.preimage_bytes(), 3);
    assert_eq!(evidence.replacement_bytes(), 7);
    assert_eq!(evidence.file_bytes_before(), 20);
    assert_eq!(evidence.file_bytes_after(), 24);
    assert_eq!(
        evidence.file_fingerprint_before(),
        "fnv1a64:0000000000000001"
    );
    assert_eq!(
        evidence.file_fingerprint_after(),
        "fnv1a64:0000000000000002"
    );

    let projection = runtime.ledger_projection().await;
    let lifecycle = lifecycle_kinds(&projection);
    let audit_indexes = lifecycle
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(audit_indexes.len(), 2);
    let artifact_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
        .expect("artifact lifecycle should be recorded");
    let resolved_index = lifecycle
        .iter()
        .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
        .expect("resolution lifecycle should be recorded");
    assert!(audit_indexes[0] < audit_indexes[1]);
    assert!(audit_indexes[1] < artifact_index);
    assert!(artifact_index < resolved_index);
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
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Failed
    );
    assert_sanitized_policy_denial_content(
        &denied_action_content(&runtime, &events).await,
        "policy_write_opt_in",
    );

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[0].tool_call_id(), pending.id());
    assert_eq!(audits[0].tool_name(), pending.name());
    assert!(audits[0].proposal().is_some());
    assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
    assert_eq!(audits[1].tool_call_id(), pending.id());
    assert_eq!(audits[1].tool_name(), pending.name());
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
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Succeeded
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[0].tool_call_id(), pending.id());
    assert_eq!(audits[0].tool_name(), pending.name());
    assert_eq!(audits[0].action_kind(), ToolActionKind::WorkspaceWrite);
    assert!(audits[0].policy().is_none());
    assert!(audits[0].proposal().is_some());
    assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
    assert_eq!(audits[1].tool_call_id(), pending.id());
    assert_eq!(audits[1].tool_name(), pending.name());
    assert_eq!(audits[1].action_kind(), ToolActionKind::WorkspaceWrite);
    assert!(audits[1].proposal().is_none());
    assert!(audits[1].execution_evidence().is_some());
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
    let projection_before = runtime.ledger_projection().await;

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
    assert_eq!(runtime.ledger_projection().await, projection_before);
    assert!(action_audit_records(&runtime).await.is_empty());
}
