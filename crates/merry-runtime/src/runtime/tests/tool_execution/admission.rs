use crate::{
    PermissionReviewMode,
    action_policy::{ActionPolicyDecision, ActionRiskTier, DefaultActionPolicy},
    runtime::{
        DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED, admit_action_to_generic_executor,
        tests::support::{
            common::RuntimeSessionStateTestExt,
            process::ProcessProposingToolExecutor,
            tool_executors::{ProposingToolExecutor, SuccessfulToolExecutor},
            tool_helpers::{
                assert_sanitized_policy_denial_content, denied_action_content,
                event_kind_names_for_tool_execution, policy_pending_tool_call, policy_tool_spec,
                register_policy_pending_registered_tool,
                register_policy_pending_registered_tool_with_builder, register_policy_pending_tool,
                resolved_tool_result,
            },
        },
    },
    tool::{
        ActionProposal, ActionProposalEvidence, RegisteredTool, ToolActionKind,
        ToolActionProposalMode, ToolExecutionContext, WorkspacePatchProposal,
    },
};
use merry_core::{RuntimeJournalPayload, SessionId, ToolCallResultStatus};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn read_only_registered_tool_executes_under_default_policy() {
    let executor = SuccessfulToolExecutor::new();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-read-only",
        "policy_read",
        "call-read-only",
        ToolActionKind::ReadOnly,
        executor.clone(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("read-only tool execution should be allowed");

    assert_eq!(executor.call_count(), 1);
    assert_eq!(
        events
            .iter()
            .map(|event| match event.payload {
                RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
                RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
                _ => "Other",
            })
            .collect::<Vec<_>>(),
        ["ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[test]
fn generic_executor_admission_allows_read_only_and_rejects_mutating_actions() {
    let session_id = SessionId::new("generic-executor-admission").expect("valid session id");
    let pending = policy_pending_tool_call("call-admission", "apply_patch");

    let read_only_decision = DefaultActionPolicy.decide(ToolActionKind::ReadOnly);
    admit_action_to_generic_executor(
        &pending,
        ToolActionKind::ReadOnly,
        ToolActionProposalMode::Disabled,
        &read_only_decision,
        None,
        &session_id,
    )
    .expect("read-only action may enter generic executor");

    for action_kind in [
        ToolActionKind::WorkspaceWrite,
        ToolActionKind::CommandExec,
        ToolActionKind::Network,
    ] {
        let decision = DefaultActionPolicy.decide(action_kind);
        let err = admit_action_to_generic_executor(
            &pending,
            action_kind,
            ToolActionProposalMode::Disabled,
            &decision,
            None,
            &session_id,
        )
        .expect_err("mutating action must require commit lifecycle");
        assert!(matches!(
            err,
            crate::RuntimeError::MutatingActionCommitLifecycleRequired {
                session_id: ref guarded_session,
                call_id: ref guarded_call,
                action_kind: guarded_kind,
            } if guarded_session == &session_id
                && guarded_call == pending.id()
                && guarded_kind == action_kind
        ));
        assert!(
            err.to_string()
                .contains("requires an explicit commit lifecycle")
        );
    }

    let patch = WorkspacePatchProposal::new(
        "notes/proposed.txt",
        3,
        7,
        20,
        24,
        "fnv1a64:0000000000000001",
        "fnv1a64:0000000000000002",
    )
    .expect("test proposal metadata is valid");
    let proposal = ActionProposal::new(
        &pending,
        ToolActionKind::WorkspaceWrite,
        "workspace patch",
        "notes/proposed.txt",
        "Replace one matched preimage in notes/proposed.txt",
        ActionProposalEvidence::WorkspacePatch(patch),
    )
    .expect("test action proposal is valid");
    let allowed_decision = ActionPolicyDecision::allow_low_risk_apply_patch();
    admit_action_to_generic_executor(
        &pending,
        ToolActionKind::WorkspaceWrite,
        ToolActionProposalMode::LowRiskWorkspacePatch,
        &allowed_decision,
        Some(&proposal),
        &session_id,
    )
    .expect("low-risk workspace patch proposal may enter generic executor");

    let non_patch_pending = policy_pending_tool_call("call-admission-other", "policy_admission");
    let err = admit_action_to_generic_executor(
        &non_patch_pending,
        ToolActionKind::WorkspaceWrite,
        ToolActionProposalMode::Enabled,
        &allowed_decision,
        Some(&proposal),
        &session_id,
    )
    .expect_err("only tools opted into the low-risk patch lane may enter");
    assert!(matches!(
        err,
        crate::RuntimeError::MutatingActionCommitLifecycleRequired {
            action_kind: ToolActionKind::WorkspaceWrite,
            ..
        }
    ));

    for action_kind in [ToolActionKind::CommandExec, ToolActionKind::Network] {
        let err = admit_action_to_generic_executor(
            &pending,
            action_kind,
            ToolActionProposalMode::LowRiskWorkspacePatch,
            &allowed_decision,
            Some(&proposal),
            &session_id,
        )
        .expect_err("only workspace patch proposals may enter generic executor");
        assert!(matches!(
            err,
            crate::RuntimeError::MutatingActionCommitLifecycleRequired {
                action_kind: guarded_kind,
                ..
            } if guarded_kind == action_kind
        ));
    }

    let elevated_decision = DefaultActionPolicy
        .decide(ToolActionKind::WorkspaceWrite)
        .with_risk_tier(ActionRiskTier::EditElevated);
    let err = admit_action_to_generic_executor(
        &pending,
        ToolActionKind::WorkspaceWrite,
        ToolActionProposalMode::LowRiskWorkspacePatch,
        &elevated_decision,
        Some(&proposal),
        &session_id,
    )
    .expect_err("workspace write requires low-risk allow decision");
    assert!(matches!(
        err,
        crate::RuntimeError::MutatingActionCommitLifecycleRequired {
            action_kind: ToolActionKind::WorkspaceWrite,
            ..
        }
    ));

    let trusted_decision =
        ActionPolicyDecision::allow_fully_trusted_action(ToolActionKind::CommandExec);
    admit_action_to_generic_executor(
        &pending,
        ToolActionKind::CommandExec,
        ToolActionProposalMode::Disabled,
        &trusted_decision,
        None,
        &session_id,
    )
    .expect("explicit trusted mode may enter the generic executor without a proposal");
}

#[tokio::test(flavor = "current_thread")]
async fn trusted_external_tools_are_allowed_without_commit_lifecycle() {
    let executor = SuccessfulToolExecutor::new();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-trusted-external",
        "trusted_external_tool",
        "call-trusted-external",
        ToolActionKind::TrustedExternal,
        executor.clone(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("trusted external tool execution should be allowed");

    assert_eq!(executor.call_count(), 1);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn fully_trusted_mode_executes_configured_mutating_tools_without_proposals() {
    for (index, action_kind) in [
        ToolActionKind::WorkspaceWrite,
        ToolActionKind::CommandExec,
        ToolActionKind::Network,
    ]
    .into_iter()
    .enumerate()
    {
        let tool_name = format!("trusted_mutating_{index}");
        let executor = SuccessfulToolExecutor::new();
        let tool = RegisteredTool::new(
            policy_tool_spec(&tool_name),
            Arc::new(executor.clone()),
            action_kind,
        );
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            &format!("runtime-policy-trusted-mutating-{index}"),
            &tool_name,
            &format!("call-trusted-mutating-{index}"),
            tool,
            |builder| {
                builder
                    .permission_review_mode(PermissionReviewMode::FullyTrusted)
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("explicit trusted mode should execute configured tools");

        assert_eq!(executor.call_count(), 1);
        assert_eq!(
            resolved_tool_result(&events).status(),
            ToolCallResultStatus::Succeeded
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_write_tool_is_denied_before_executor_and_records_sanitized_failure_artifact() {
    let executor = SuccessfulToolExecutor::new();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-workspace-write",
        "policy_write",
        "call-workspace-write",
        ToolActionKind::WorkspaceWrite,
        executor.clone(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("policy denial should durably resolve the pending call");

    assert_eq!(executor.call_count(), 0);
    assert_eq!(
        events
            .iter()
            .map(|event| match event.payload {
                RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
                RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
                _ => "Other",
            })
            .collect::<Vec<_>>(),
        ["ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert!(matches!(
        &events[0].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let content = denied_action_content(&runtime, &events).await;
    assert_sanitized_policy_denial_content(&content, "policy_write");
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_write_tool_with_proposal_is_denied_without_execute() {
    let executor = ProposingToolExecutor::immediate();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_write_proposed"),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool(
        "runtime-policy-proposed-workspace-write",
        "policy_write_proposed",
        "call-workspace-write-proposed",
        tool,
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("policy denial should durably resolve proposed action");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ToolCallResolved"]
    );
    assert_sanitized_policy_denial_content(
        &denied_action_content(&runtime, &events).await,
        "policy_write_proposed",
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_write_tool_without_proposal_opt_in_does_not_call_propose() {
    let executor = ProposingToolExecutor::immediate();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-proposal-disabled",
        "policy_write_proposal_disabled",
        "call-workspace-write-proposal-disabled",
        ToolActionKind::WorkspaceWrite,
        executor.clone(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("policy denial should durably resolve without proposal hook");

    assert_eq!(executor.propose_count(), 0);
    assert_eq!(executor.execute_count(), 0);
    assert_sanitized_policy_denial_content(
        &denied_action_content(&runtime, &events).await,
        "policy_write_proposal_disabled",
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn command_exec_tool_is_denied_before_executor() {
    let executor = ProposingToolExecutor::immediate();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-command-exec",
        "policy_command",
        "call-command-exec",
        ToolActionKind::CommandExec,
        executor.clone(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("policy denial should durably resolve the pending call");

    assert_eq!(executor.propose_count(), 0);
    assert_eq!(executor.execute_count(), 0);
    let content = denied_action_content(&runtime, &events).await;
    assert_sanitized_policy_denial_content(&content, "policy_command");
    assert_eq!(
        resolved_tool_result(&events)
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn command_exec_with_process_proposal_is_denied_without_execute() {
    let executor =
        ProcessProposingToolExecutor::with_argv(["cargo", "test", "-p", "merry-runtime"]);
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_proposed"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool(
        "runtime-policy-proposed-command-exec",
        "policy_command_proposed",
        "call-command-exec-proposed",
        tool,
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("policy denial should durably resolve proposed command exec");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ToolCallResolved"]
    );
    assert_sanitized_policy_denial_content(
        &denied_action_content(&runtime, &events).await,
        "policy_command_proposed",
    );
    assert_eq!(
        resolved_tool_result(&events)
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn network_tool_is_denied_before_executor() {
    let executor = ProposingToolExecutor::immediate();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-network",
        "policy_network",
        "call-network",
        ToolActionKind::Network,
        executor.clone(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("policy denial should durably resolve the pending call");

    assert_eq!(executor.propose_count(), 0);
    assert_eq!(executor.execute_count(), 0);
    let content = denied_action_content(&runtime, &events).await;
    assert_sanitized_policy_denial_content(&content, "policy_network");
    assert_eq!(
        resolved_tool_result(&events)
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn unregistered_plan_tool_resolves_as_tool_not_registered() {
    let runtime = crate::Runtime::builder(
        SessionId::new("runtime-unregistered-plan-tool").expect("valid id"),
    )
    .build()
    .expect("runtime builds");
    let pending = policy_pending_tool_call("call-unregistered-plan", "enter_plan_mode");
    {
        let mut session = runtime.inner.session.lock().await;
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending recorded");
    }

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("unregistered plan tool should resolve through normal registry lookup");

    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result.diagnostic().map(|d| d.code()),
        Some("tool_not_registered")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn custom_tool_named_request_permissions_does_not_hijack_intrinsic_dispatch() {
    let executor = SuccessfulToolExecutor::new();
    let tool = RegisteredTool::new(
        policy_tool_spec("request_permissions"),
        Arc::new(executor.clone()),
        ToolActionKind::RuntimeControl,
    );
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-custom-request-permissions-name",
        "request_permissions",
        "call-custom-request-permissions",
        tool,
        |builder| builder.build(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("custom RuntimeControl tool should run its own executor");

    assert_eq!(executor.call_count(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}
