use crate::{
    PermissionReviewMode,
    action_audit::ActionAuditStatus,
    action_policy::{
        ActionPolicyDecision, ActionPolicyDisposition, ActionRiskTier, DefaultActionPolicy,
    },
    ledger::LedgerFactKind,
    process::ProcessEnvPolicy,
    runtime::{
        APPLY_PATCH_TOOL_NAME, DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        admit_action_to_generic_executor,
        tests::support::{
            process::ProcessProposingToolExecutor,
            tool_executors::{ProposingToolExecutor, SuccessfulToolExecutor},
            tool_helpers::{
                action_audit_records, assert_lifecycle_order,
                assert_sanitized_policy_denial_content, denied_action_content,
                event_kind_names_for_tool_execution, lifecycle_kinds, policy_pending_tool_call,
                policy_tool_spec, register_policy_pending_registered_tool,
                register_policy_pending_registered_tool_with_builder, register_policy_pending_tool,
                resolved_tool_result,
            },
        },
    },
    tool::{
        ActionProposal, ActionProposalEvidence, RegisteredTool, ToolActionKind,
        ToolExecutionContext, WorkspacePatchProposal,
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
                RuntimeJournalPayload::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
                RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
                _ => "Other",
            })
            .collect::<Vec<_>>(),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[test]
fn generic_executor_admission_allows_read_only_and_rejects_mutating_actions() {
    let session_id = SessionId::new("generic-executor-admission").expect("valid session id");
    let pending = policy_pending_tool_call("call-admission", APPLY_PATCH_TOOL_NAME);

    let read_only_decision = DefaultActionPolicy.decide(ToolActionKind::ReadOnly);
    admit_action_to_generic_executor(
        &pending,
        ToolActionKind::ReadOnly,
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
        let err =
            admit_action_to_generic_executor(&pending, action_kind, &decision, None, &session_id)
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
        &allowed_decision,
        Some(&proposal),
        &session_id,
    )
    .expect("low-risk workspace patch proposal may enter generic executor");

    let non_patch_pending = policy_pending_tool_call("call-admission-other", "policy_admission");
    let err = admit_action_to_generic_executor(
        &non_patch_pending,
        ToolActionKind::WorkspaceWrite,
        &allowed_decision,
        Some(&proposal),
        &session_id,
    )
    .expect_err("only apply_patch may enter the low-risk patch lane");
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
        ["ArtifactRecorded", "ToolCallResolved"]
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
                RuntimeJournalPayload::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeJournalPayload::AssistantOutputRecorded { .. } => "AssistantOutputRecorded",
                RuntimeJournalPayload::ToolCallResolved { .. } => "ToolCallResolved",
                _ => "Other",
            })
            .collect::<Vec<_>>(),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert!(matches!(
        &events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &events[1].payload,
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

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 1);
    let audit = &audits[0];
    assert_eq!(audit.id().as_str(), "action-audit-00000000000000000000");
    assert_eq!(audit.order(), 0);
    assert_eq!(audit.tool_call_id(), pending.id());
    assert_eq!(audit.tool_name(), pending.name());
    assert_eq!(audit.action_kind(), ToolActionKind::WorkspaceWrite);
    assert_eq!(audit.status(), ActionAuditStatus::Denied);
    let policy = audit.policy().expect("denied audit should include policy");
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
    assert_eq!(policy.risk_tier(), ActionRiskTier::EditElevated);
    assert_eq!(
        policy.reason(),
        "workspace write tool actions are denied by default policy"
    );

    let projection = runtime.ledger_projection().await;
    let lifecycle = lifecycle_kinds(&projection);
    assert_lifecycle_order(
        &lifecycle,
        LedgerFactKind::ActionAuditRecorded,
        LedgerFactKind::ArtifactRecorded,
    );
    assert_lifecycle_order(
        &lifecycle,
        LedgerFactKind::ActionAuditRecorded,
        LedgerFactKind::ToolCallResolved,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_write_tool_with_proposal_records_proposed_before_denied_and_resolution() {
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
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_sanitized_policy_denial_content(
        &denied_action_content(&runtime, &events).await,
        "policy_write_proposed",
    );

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[0].tool_call_id(), pending.id());
    assert_eq!(audits[0].tool_name(), pending.name());
    assert_eq!(audits[0].action_kind(), ToolActionKind::WorkspaceWrite);
    assert!(audits[0].policy().is_none());
    let proposal = audits[0]
        .proposal()
        .expect("proposed audit should include proposal evidence");
    assert_eq!(proposal.tool_call_id(), pending.id());
    assert_eq!(proposal.tool_name(), pending.name());
    assert_eq!(proposal.action_kind(), ToolActionKind::WorkspaceWrite);
    assert_eq!(proposal.label(), "workspace patch");
    assert_eq!(proposal.subject(), "notes/proposed.txt");
    assert!(proposal.summary().contains("notes/proposed.txt"));
    let ActionProposalEvidence::WorkspacePatch(patch) = proposal.evidence() else {
        panic!("workspace write proposal should record workspace patch evidence");
    };
    assert_eq!(patch.relative_path(), "notes/proposed.txt");
    assert_eq!(patch.preimage_bytes(), 3);
    assert_eq!(patch.replacement_bytes(), 7);
    assert_eq!(patch.file_bytes_before(), 20);
    assert_eq!(patch.file_bytes_after(), 24);
    assert_eq!(patch.file_fingerprint_before(), "fnv1a64:0000000000000001");
    assert_eq!(patch.file_fingerprint_after(), "fnv1a64:0000000000000002");

    assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
    assert_eq!(audits[1].tool_call_id(), pending.id());
    assert_eq!(audits[1].tool_name(), pending.name());
    assert!(audits[1].proposal().is_none());
    let denied_policy = audits[1]
        .policy()
        .expect("denied audit should include policy");
    assert_eq!(denied_policy.risk_tier(), ActionRiskTier::EditLow);
    assert_eq!(denied_policy.disposition(), ActionPolicyDisposition::Deny);

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

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].status(), ActionAuditStatus::Denied);
    assert!(audits[0].proposal().is_none());
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
    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].action_kind(), ToolActionKind::CommandExec);
    assert_eq!(audits[0].status(), ActionAuditStatus::Denied);
    assert_eq!(
        audits[0]
            .policy()
            .expect("denied audit should include policy")
            .disposition(),
        ActionPolicyDisposition::Deny
    );
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
async fn command_exec_with_process_proposal_records_proposed_then_denied_without_execute() {
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
        ["ArtifactRecorded", "ToolCallResolved"]
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

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[0].tool_call_id(), pending.id());
    assert_eq!(audits[0].tool_name(), pending.name());
    assert_eq!(audits[0].action_kind(), ToolActionKind::CommandExec);
    assert!(audits[0].policy().is_none());
    let proposal = audits[0]
        .proposal()
        .expect("proposed audit should include process proposal");
    assert_eq!(proposal.action_kind(), ToolActionKind::CommandExec);
    let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
        panic!("command exec proposal should record process action evidence");
    };
    assert_eq!(intent.argv(), ["cargo", "test", "-p", "merry-runtime"]);
    assert_eq!(intent.cwd(), Some("."));
    assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);

    assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
    assert_eq!(audits[1].tool_call_id(), pending.id());
    assert_eq!(audits[1].tool_name(), pending.name());
    assert_eq!(audits[1].action_kind(), ToolActionKind::CommandExec);
    assert!(audits[1].proposal().is_none());
    let denied_policy = audits[1]
        .policy()
        .expect("denied audit should include policy");
    assert_eq!(
        denied_policy.risk_tier(),
        ActionRiskTier::ProcessLocalWorkspaceEffect
    );
    assert_eq!(denied_policy.disposition(), ActionPolicyDisposition::Deny);
    assert_eq!(
        denied_policy.reason(),
        "command execution tool actions are denied by default policy"
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
    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].action_kind(), ToolActionKind::Network);
    assert_eq!(audits[0].status(), ActionAuditStatus::Denied);
    assert_eq!(
        audits[0]
            .policy()
            .expect("denied audit should include policy")
            .disposition(),
        ActionPolicyDisposition::Deny
    );
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
