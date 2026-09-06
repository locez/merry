use crate::action_audit::ActionAuditStatus;
use crate::action_policy::{ActionPolicyDisposition, ActionRiskTier};
use crate::runtime::tests::{
    FakeProcessRunner, ProcessProposingToolExecutor, RecordingModelProvider,
    ScriptedModelProviderResponse, StaticPermissionAdmissionSource, action_audit_records,
    named_model, permission_review_completed_event, policy_tool_spec,
    register_policy_pending_registered_tool_with_builder, resolved_tool_result,
};
use crate::{
    AcceptedLocalWorkspaceProcessAdmission, PermissionReviewMode, RegisteredTool, RuntimeModelRole,
    ToolActionKind, ToolExecutionContext,
};
use merry_core::ToolCallResultStatus;
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn fully_trusted_host_process_allows_high_risk_argv_without_review() {
    let executor = ProcessProposingToolExecutor::with_argv(["sudo", "su"]);
    let runner = FakeProcessRunner::succeeding();
    let admission = StaticPermissionAdmissionSource::denying();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_trusted_host_high_risk"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-fully-trusted-high-risk",
        "policy_command_trusted_host_high_risk",
        "call-command-fully-trusted-high-risk",
        tool,
        |builder| {
            builder
                .permission_review_mode(PermissionReviewMode::FullyTrusted)
                .permission_admission_source(Arc::new(admission.clone()))
                .allow_accepted_local_workspace_process_actions(
                    AcceptedLocalWorkspaceProcessAdmission::accept_host(),
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("trusted host process should execute without a permission review");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(admission.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
    let policy = audits[1]
        .policy()
        .expect("trusted host execution should keep an action policy audit");
    assert_eq!(policy.risk_tier(), ActionRiskTier::ProcessHigh);
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
}

#[tokio::test(flavor = "current_thread")]
async fn host_process_requires_review_without_fully_trusted_mode() {
    let executor = ProcessProposingToolExecutor::with_argv(["sudo", "su"]);
    let runner = FakeProcessRunner::succeeding();
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event(
                "approve",
                "The explicit test task authorizes this host action.",
            ),
        )])]);
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_host_reviewed"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-host-review-required",
        "policy_command_host_reviewed",
        "call-command-host-review-required",
        tool,
        |builder| {
            builder
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(review_provider.clone()),
                    named_model("fake/approval-review"),
                )
                .allow_accepted_local_workspace_process_actions(
                    AcceptedLocalWorkspaceProcessAdmission::accept_host(),
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("reviewed host process should execute after approval");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(review_provider.recorded_requests().len(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_host_process_does_not_require_review_without_external_path() {
    let executor = ProcessProposingToolExecutor::with_argv(["git", "status", "--short"]);
    let runner = FakeProcessRunner::succeeding();
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event(
                "deny",
                "This ordinary workspace action should not reach review.",
            ),
        )])]);
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_host_workspace_git"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-host-workspace-git",
        "policy_command_host_workspace_git",
        "call-command-host-workspace-git",
        tool,
        |builder| {
            builder
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(review_provider.clone()),
                    named_model("fake/approval-review"),
                )
                .allow_accepted_local_workspace_process_actions(
                    AcceptedLocalWorkspaceProcessAdmission::accept_host(),
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("ordinary host workspace process should execute without review");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(runner.call_count(), 1);
    assert!(review_provider.recorded_requests().is_empty());
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn host_git_metadata_write_requires_path_review_without_fully_trusted_mode() {
    let executor = ProcessProposingToolExecutor::with_argv(["git", "checkout", "--", "README.md"]);
    let runner = FakeProcessRunner::succeeding();
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event(
                "approve",
                "The task authorizes the one-action .git metadata write.",
            ),
        )])]);
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_host_git_metadata"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-host-git-metadata",
        "policy_command_host_git_metadata",
        "call-command-host-git-metadata",
        tool,
        |builder| {
            builder
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(review_provider.clone()),
                    named_model("fake/approval-review"),
                )
                .allow_accepted_local_workspace_process_actions(
                    AcceptedLocalWorkspaceProcessAdmission::accept_host(),
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("approved host git metadata action should execute");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(review_provider.recorded_requests().len(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn host_process_with_external_path_requires_review_without_fully_trusted_mode() {
    let executor = ProcessProposingToolExecutor::with_argv(["cat", "/tmp/output"]);
    let runner = FakeProcessRunner::succeeding();
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event(
                "approve",
                "The task authorizes the explicit external path.",
            ),
        )])]);
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_host_external_path"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-host-external-path",
        "policy_command_host_external_path",
        "call-command-host-external-path",
        tool,
        |builder| {
            builder
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(review_provider.clone()),
                    named_model("fake/approval-review"),
                )
                .allow_accepted_local_workspace_process_actions(
                    AcceptedLocalWorkspaceProcessAdmission::accept_host(),
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("approved host external path process should execute");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(review_provider.recorded_requests().len(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}
