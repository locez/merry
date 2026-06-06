use super::*;

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
            .map(|event| match event.kind {
                RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
                _ => "Other",
            })
            .collect::<Vec<_>>(),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_control_tool_resolves_even_if_token_is_cancelled_during_execution() {
    let executor = CancelDuringRuntimeControlExecutor::new();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-runtime-control-cancel-race",
        "policy_control",
        "call-runtime-control",
        ToolActionKind::RuntimeControl,
        executor.clone(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("runtime control outcome should survive in-flight cancellation");

    assert_eq!(executor.call_count(), 1);
    assert!(executor.token_seen().is_cancelled());
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[test]
fn generic_executor_admission_allows_read_only_and_rejects_mutating_actions() {
    let session_id = SessionId::new("generic-executor-admission").expect("valid session id");
    let pending = policy_pending_tool_call("call-admission", WORKSPACE_PATCH_TOOL_NAME);

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
    let allowed_decision = ActionPolicyDecision::allow_low_risk_workspace_patch();
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
    .expect_err("only workspace_patch may enter the low-risk patch lane");
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
            .map(|event| match event.kind {
                RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
                _ => "Other",
            })
            .collect::<Vec<_>>(),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert!(matches!(
        &events[0].kind,
        RuntimeEventKind::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &events[1].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == result
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
async fn preflight_outcome_must_be_failed_to_resolve_without_policy_bypass() {
    let executor = ProposingToolExecutor::with_preflight_outcome(
        ToolExecutionOutcome::succeeded_text("must not bypass policy\n"),
    );
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_write_preflight_success"),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool(
        "runtime-policy-preflight-success-rejected",
        "policy_write_preflight_success",
        "call-preflight-success-rejected",
        tool,
    )
    .await;

    let error = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("successful preflight outcome must not bypass action policy");

    match error {
        RuntimeError::Core { source } => assert!(
            source.to_string().contains("preflight tool outcome"),
            "unexpected core error: {source}"
        ),
        other => panic!("expected core validation error, got {other:?}"),
    }
    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec![pending.id().as_str().to_owned()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_workspace_write_patch_proposal_executes_and_records_execution_audit() {
    let executor = ProposingToolExecutor::immediate();
    let tool = RegisteredTool::new(
        policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-proposed-workspace-write-opt-in",
        WORKSPACE_PATCH_TOOL_NAME,
        "call-workspace-write-opt-in",
        tool,
        |builder| builder.allow_low_risk_workspace_patches().build(),
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
        |builder| builder.allow_low_risk_workspace_patches().build(),
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
        policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-workspace-write-opt-in-cancel-after-side-effect",
        WORKSPACE_PATCH_TOOL_NAME,
        "call-workspace-write-opt-in-cancel-after-side-effect",
        tool,
        |builder| builder.allow_low_risk_workspace_patches().build(),
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
        policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-workspace-write-opt-in-missing-evidence",
        WORKSPACE_PATCH_TOOL_NAME,
        "call-workspace-write-opt-in-missing-evidence",
        tool,
        |builder| builder.allow_low_risk_workspace_patches().build(),
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
