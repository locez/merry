use super::*;

#[test]
fn denied_tool_action_records_audit_lifecycle_before_artifact_and_resolution() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("denied-action-call");
    let decision = DefaultActionPolicy.decide(crate::ToolActionKind::WorkspaceWrite);
    let diagnostic =
        ErrorInfo::new("action_policy_denied", "blocked by test policy").expect("valid diagnostic");
    session
        .record_session_started_if_needed()
        .expect("session should start");
    session
        .record_tool_call_pending(call.clone())
        .expect("pending call should record");

    let events = session
        .submit_denied_tool_action(
            &call,
            &decision,
            None,
            ArtifactContent::json(r#"{"ok":false}"#),
            diagnostic,
        )
        .expect("denial should resolve");

    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].kind,
        RuntimeEventKind::ArtifactRecorded { .. }
    ));
    assert!(matches!(
        events[1].kind,
        RuntimeEventKind::ToolCallResolved { .. }
    ));

    let audit_snapshot = session.action_audit_snapshot();
    assert_eq!(audit_snapshot.records().len(), 1);
    let audit = &audit_snapshot.records()[0];
    assert_eq!(audit.id().as_str(), "action-audit-00000000000000000000");
    assert_eq!(audit.order(), 0);
    assert_eq!(audit.tool_call_id(), call.id());
    assert_eq!(audit.tool_name(), call.name());
    assert_eq!(audit.action_kind(), crate::ToolActionKind::WorkspaceWrite);
    assert_eq!(audit.status(), ActionAuditStatus::Denied);
    let policy = audit.policy().expect("denied audit should include policy");
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
    assert_eq!(policy.risk_tier(), decision.risk_tier());
    assert_eq!(policy.reason(), decision.reason());

    let projection = session.ledger_projection();
    let lifecycle_kinds = projection
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            LedgerProjection::Lifecycle { kind, .. } => Some(*kind),
            LedgerProjection::Fact { .. } => None,
        })
        .collect::<Vec<_>>();
    let audit_index = lifecycle_kinds
        .iter()
        .position(|kind| *kind == LedgerFactKind::ActionAuditRecorded)
        .expect("audit lifecycle should be recorded");
    let artifact_index = lifecycle_kinds
        .iter()
        .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
        .expect("artifact lifecycle should be recorded");
    let resolved_index = lifecycle_kinds
        .iter()
        .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
        .expect("resolution lifecycle should be recorded");
    assert!(audit_index < artifact_index);
    assert!(artifact_index < resolved_index);
    assert!(session.pending_tool_calls().is_empty());
}

#[test]
fn proposed_tool_execution_records_executed_audit_before_artifact_and_resolution() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("executed-action-call");
    session
        .record_session_started_if_needed()
        .expect("session should start");
    session
        .record_tool_call_pending(call.clone())
        .expect("pending call should record");

    let proposal_evidence = ActionProposalEvidence::WorkspacePatch(
        WorkspacePatchProposal::new(
            "note.txt",
            3,
            5,
            16,
            18,
            "fnv1a64:0000000000000100",
            "fnv1a64:0000000000000101",
        )
        .expect("valid workspace patch proposal"),
    );
    let proposal = ActionProposal::new(
        &call,
        crate::ToolActionKind::WorkspaceWrite,
        "workspace patch",
        "note.txt",
        "Replace one preimage in note.txt.",
        proposal_evidence,
    )
    .expect("valid action proposal");
    let execution_evidence = ActionExecutionEvidence::WorkspacePatch(
        WorkspacePatchExecutionEvidence::new(
            "note.txt",
            3,
            5,
            16,
            18,
            "fnv1a64:0000000000000100",
            "fnv1a64:0000000000000101",
        )
        .expect("valid execution evidence"),
    );
    let policy = ActionAuditPolicy::new(
        ActionRiskTier::EditLow,
        ActionPolicyDisposition::Allow,
        "test low-risk workspace patch allow",
    );

    let events = session
        .submit_proposed_tool_execution_outcome(
            proposal,
            merry_core::ToolCallResultStatus::Succeeded,
            ArtifactContent::json(r#"{"ok":true}"#),
            None,
            Some(execution_evidence.clone()),
            policy,
        )
        .expect("proposed execution should resolve");

    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].kind,
        RuntimeEventKind::ArtifactRecorded { .. }
    ));
    assert!(matches!(
        events[1].kind,
        RuntimeEventKind::ToolCallResolved { .. }
    ));

    let audit_snapshot = session.action_audit_snapshot();
    assert_eq!(audit_snapshot.records().len(), 2);
    assert_eq!(
        audit_snapshot.records()[0].status(),
        ActionAuditStatus::Proposed
    );
    assert_eq!(
        audit_snapshot.records()[1].status(),
        ActionAuditStatus::Executed
    );
    assert!(audit_snapshot.records()[0].proposal().is_some());
    assert!(audit_snapshot.records()[0].execution_evidence().is_none());
    assert!(audit_snapshot.records()[1].proposal().is_none());
    assert_eq!(
        audit_snapshot.records()[1]
            .execution_evidence()
            .expect("executed audit should include evidence"),
        &execution_evidence
    );

    let lifecycle_kinds = session
        .ledger_projection()
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            LedgerProjection::Lifecycle { kind, .. } => Some(*kind),
            LedgerProjection::Fact { .. } => None,
        })
        .collect::<Vec<_>>();
    let audit_indexes = lifecycle_kinds
        .iter()
        .enumerate()
        .filter_map(|(index, kind)| (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(audit_indexes.len(), 2);
    let artifact_index = lifecycle_kinds
        .iter()
        .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
        .expect("artifact lifecycle should be recorded");
    let resolved_index = lifecycle_kinds
        .iter()
        .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
        .expect("resolution lifecycle should be recorded");
    assert!(audit_indexes[0] < audit_indexes[1]);
    assert!(audit_indexes[1] < artifact_index);
    assert!(artifact_index < resolved_index);
}

#[test]
fn proposed_tool_execution_can_record_observation_after_artifact_before_resolution() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("observed-action-call");
    session
        .record_session_started_if_needed()
        .expect("session should start");
    session
        .record_tool_call_pending(call.clone())
        .expect("pending call should record");

    let proposal_evidence = ActionProposalEvidence::WorkspacePatch(
        WorkspacePatchProposal::new(
            "note.txt",
            3,
            5,
            16,
            18,
            "fnv1a64:0000000000000100",
            "fnv1a64:0000000000000101",
        )
        .expect("valid workspace patch proposal"),
    );
    let proposal = ActionProposal::new(
        &call,
        crate::ToolActionKind::WorkspaceWrite,
        "workspace patch",
        "note.txt",
        "Replace one preimage in note.txt.",
        proposal_evidence,
    )
    .expect("valid action proposal");
    let execution_evidence = ActionExecutionEvidence::WorkspacePatch(
        WorkspacePatchExecutionEvidence::new(
            "note.txt",
            3,
            5,
            16,
            18,
            "fnv1a64:0000000000000100",
            "fnv1a64:0000000000000101",
        )
        .expect("valid execution evidence"),
    );
    let policy = ActionAuditPolicy::new(
        ActionRiskTier::EditLow,
        ActionPolicyDisposition::Allow,
        "test low-risk workspace patch allow",
    );
    let observation = ToolResultLedgerObservation::new(
        LedgerScope::Tool,
        "process action `rustc --version` exit code 0; stdout_bytes=21; stderr_bytes=0",
    )
    .expect("valid compact observation");

    let events = session
        .submit_proposed_tool_execution_outcome_record(
            ProposedToolExecutionOutcome::new(
                proposal,
                merry_core::ToolCallResultStatus::Succeeded,
                ArtifactContent::json(r#"{"ok":true}"#),
                None,
                Some(execution_evidence),
                policy,
            )
            .with_observation(observation),
        )
        .expect("observed proposed execution should resolve");
    let result = events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool result should resolve");

    let projection = session.ledger_projection();
    let artifact_order = projection
        .entries()
        .iter()
        .find_map(|entry| match entry {
            LedgerProjection::Lifecycle {
                kind: LedgerFactKind::ArtifactRecorded,
                order,
                ..
            } => Some(*order),
            LedgerProjection::Lifecycle { .. } | LedgerProjection::Fact { .. } => None,
        })
        .expect("artifact lifecycle should be recorded");
    let resolved_order = projection
        .entries()
        .iter()
        .find_map(|entry| match entry {
            LedgerProjection::Lifecycle {
                kind: LedgerFactKind::ToolCallResolved,
                order,
                ..
            } => Some(*order),
            LedgerProjection::Lifecycle { .. } | LedgerProjection::Fact { .. } => None,
        })
        .expect("resolution lifecycle should be recorded");
    let (observation_order, observation_text) = projection
        .entries()
        .iter()
        .find_map(|entry| match entry {
            LedgerProjection::Fact {
                order,
                scope: LedgerScope::Tool,
                text,
                ..
            } if text.starts_with("process action `rustc --version`") => {
                Some((*order, text.as_str()))
            }
            LedgerProjection::Fact { .. } | LedgerProjection::Lifecycle { .. } => None,
        })
        .expect("tool observation should be projected");

    assert!(artifact_order < observation_order);
    assert!(observation_order < resolved_order);
    assert!(observation_text.contains(&format!("artifact={}", result.artifact().id().as_str())));
}

#[test]
fn guarded_tool_action_records_internal_audit_without_events_or_resolution() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("guarded-action-call");
    session
        .record_session_started_if_needed()
        .expect("session should start");
    session
        .record_tool_call_pending(call.clone())
        .expect("pending call should record");
    let projection_before = session.ledger_projection();
    let next_sequence_before = session.next_sequence();
    let policy = ActionAuditPolicy::new(
        ActionRiskTier::EditElevated,
        ActionPolicyDisposition::Allow,
        "test policy allowed workspace write before commit lifecycle",
    );

    session
        .record_guarded_tool_action(&call, crate::ToolActionKind::WorkspaceWrite, policy)
        .expect("guarded audit should record for pending call");

    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), vec![call.clone()]);
    let audit_snapshot = session.action_audit_snapshot();
    assert_eq!(audit_snapshot.records().len(), 1);
    let audit = &audit_snapshot.records()[0];
    assert_eq!(audit.tool_call_id(), call.id());
    assert_eq!(audit.tool_name(), call.name());
    assert_eq!(audit.action_kind(), crate::ToolActionKind::WorkspaceWrite);
    assert_eq!(audit.status(), ActionAuditStatus::Guarded);
    assert_eq!(
        audit.policy().expect("guarded audit should include policy"),
        policy
    );
    assert!(audit.proposal().is_none());

    let projection_after = session.ledger_projection();
    assert_eq!(
        projection_after.entries().len(),
        projection_before.entries().len() + 1
    );
    assert!(matches!(
        projection_after.entries().last(),
        Some(LedgerProjection::Lifecycle {
            kind: LedgerFactKind::ActionAuditRecorded,
            ..
        })
    ));
    let expected_result_artifact_id = artifact_id("tool-result-2");
    assert!(matches!(
        session.read_artifact_content(&expected_result_artifact_id),
        Err(ArtifactError::MissingArtifact { id }) if id == expected_result_artifact_id
    ));

    let audit_snapshot_after_first = session.action_audit_snapshot();
    let projection_after_first = session.ledger_projection();
    session
        .record_guarded_tool_action(&call, crate::ToolActionKind::WorkspaceWrite, policy)
        .expect("duplicate guarded audit should no-op for pending call");

    assert_eq!(session.next_sequence(), next_sequence_before);
    assert_eq!(session.pending_tool_calls(), vec![call.clone()]);
    assert_eq!(session.action_audit_snapshot(), audit_snapshot_after_first);
    assert_eq!(session.ledger_projection(), projection_after_first);
    assert!(matches!(
        session.read_artifact_content(&expected_result_artifact_id),
        Err(ArtifactError::MissingArtifact { id }) if id == expected_result_artifact_id
    ));
}

#[test]
fn submit_tool_result_starts_session_before_artifact_and_resolution_when_needed() {
    let mut session = SessionState::new(session_id());
    session
        .pending_tool_calls
        .push(pending_tool_call("pre-start-call"));
    let artifact = ArtifactRef::new(artifact_id("pre-start-result"), ArtifactKind::Json);
    let result = ToolCallResult::succeeded(tool_call_id("pre-start-call"), artifact.clone());

    let events = session
        .submit_tool_result(result.clone(), ArtifactContent::json(r#"{"ok":true}"#))
        .expect("tool result should submit");

    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(matches!(events[0].kind, RuntimeEventKind::SessionStarted));
    assert!(matches!(
        &events[1].kind,
        RuntimeEventKind::ArtifactRecorded { artifact: recorded } if recorded == &artifact
    ));
    assert!(matches!(
        &events[2].kind,
        RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == &result
    ));
}

#[test]
fn duplicate_tool_call_pending_is_rejected_without_second_pending_or_sequence() {
    let mut session = SessionState::new(session_id());
    let call = pending_tool_call("duplicate-call");
    let first = session
        .record_tool_call_pending(call.clone())
        .expect("first pending call should record");

    let err = session
        .record_tool_call_pending(call.clone())
        .expect_err("duplicate pending call id should be rejected");

    assert_eq!(err.code(), "tool_call_duplicate");
    assert_eq!(session.pending_tool_calls(), vec![call]);
    assert_eq!(first.sequence, 0);
    assert_eq!(
        session
            .ledger_projection()
            .entries()
            .iter()
            .map(|entry| match entry {
                crate::ledger::LedgerProjection::Lifecycle { sequence, .. } => *sequence,
                crate::ledger::LedgerProjection::Fact { sequence, .. } => *sequence,
            })
            .collect::<Vec<_>>(),
        vec![0]
    );
}
