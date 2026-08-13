use super::*;

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_uses_runner_and_records_execution_audit() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_opt_in"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-opt-in",
        "policy_command_opt_in",
        "call-command-exec-opt-in",
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
        .expect("opted-in low-risk process action should execute through runner");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(result.diagnostic().is_none());
    assert!(matches!(
        &events[0].payload,
        RuntimeJournalPayload::ArtifactRecorded { artifact } if artifact == result.artifact()
    ));
    assert!(matches!(
        &events[1].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert!(runtime.pending_tool_calls().await.is_empty());

    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("process result artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("process result artifact should be textual JSON"),
    )
    .expect("process result artifact should parse as JSON");
    assert_eq!(
        payload,
        json!({
            "ok": true,
            "kind": "process_action",
            "permission_profile_id": "process.read_only.v1",
            "status": {
                "kind": "exited",
                "code": 0,
            },
            "intent": {
                "summary": "process argv[0]=rustc; argc=2; cwd=.",
                "argv": ["rustc", "--version"],
                "cwd": ".",
            },
            "stdout": {
                "text": "runtime tests passed\n",
                "bytes": "runtime tests passed\n".len(),
                "truncated": false,
                "utf8": true,
                "bytes_base64": "cnVudGltZSB0ZXN0cyBwYXNzZWQK",
            },
            "stderr": {
                "text": "",
                "bytes": 0,
                "truncated": false,
                "utf8": true,
                "bytes_base64": "",
            }
        })
    );
    assert!(payload.get("provider").is_none());
    assert!(payload.get("wire").is_none());

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
    let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
        panic!("proposed audit should record process action intent");
    };
    assert_eq!(runner.observed_intents(), vec![intent.clone()]);

    assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
    assert_eq!(audits[1].tool_call_id(), pending.id());
    assert_eq!(audits[1].tool_name(), pending.name());
    assert_eq!(audits[1].action_kind(), ToolActionKind::CommandExec);
    assert!(audits[1].proposal().is_none());
    let policy = audits[1]
        .policy()
        .expect("executed audit should include process allow policy");
    assert_eq!(policy.risk_tier(), ActionRiskTier::ProcessLow);
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
    let ActionExecutionEvidence::ProcessAction(evidence) = audits[1]
        .execution_evidence()
        .expect("executed audit should include process evidence")
    else {
        panic!("process action should record process execution evidence");
    };
    assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
    assert_eq!(evidence.stdout_bytes(), "runtime tests passed\n".len());
    assert!(!evidence.stdout_truncated());
    assert_eq!(evidence.stderr_bytes(), 0);
    assert!(!evidence.stderr_truncated());
    assert_eq!(
        evidence.permission_profile_id().as_str(),
        "process.read_only.v1"
    );
    assert!(evidence.matches_intent(intent));

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
        .expect("artifact lifecycle should be projected");
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
        .expect("resolution lifecycle should be projected");
    let (observation_order, observation_scope, observation_text) = projection
        .entries()
        .iter()
        .find_map(|entry| match entry {
            LedgerProjection::Fact {
                order, scope, text, ..
            } if text.starts_with("process action `rustc --version`") => {
                Some((*order, *scope, text.as_str()))
            }
            LedgerProjection::Fact { .. } | LedgerProjection::Lifecycle { .. } => None,
        })
        .expect("process result should be reduced into a compact ledger observation");
    assert_eq!(observation_scope, LedgerScope::Tool);
    assert!(artifact_order < observation_order);
    assert!(observation_order < resolved_order);
    assert!(observation_text.contains("exit code 0"));
    assert!(observation_text.contains("permission_profile=process.read_only.v1"));
    assert!(observation_text.contains("stdout_bytes=21"));
    assert!(observation_text.contains("stderr_bytes=0"));
    assert!(observation_text.contains(&format!("artifact={}", result.artifact().id().as_str())));
    assert!(!observation_text.contains("runtime tests passed"));
}

#[tokio::test(flavor = "current_thread")]
async fn process_action_artifact_guides_model_when_output_is_truncated() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::succeeding_with_truncated_stdout();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_truncated_output"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-truncated-output",
        "policy_command_truncated_output",
        "call-command-truncated-output",
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
        .expect("process action should execute");

    let result = resolved_tool_result(&events);
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("process result artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("process result artifact should be textual JSON"),
    )
    .expect("process result artifact should parse as JSON");

    assert_eq!(payload["stdout"]["truncated"], true);
    assert_eq!(payload["stderr"]["truncated"], false);
    assert_eq!(payload["guidance"]["kind"], "process_output_truncated");
    assert_eq!(payload["guidance"]["stdout_truncated"], true);
    assert_eq!(payload["guidance"]["stderr_truncated"], false);
    assert!(
        payload["guidance"]["message"]
            .as_str()
            .expect("guidance message should be text")
            .contains("rerun with a narrower command")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failed_process_action_artifact_explains_capability_recovery() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::failing();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_failed_recovery"),
        Arc::new(executor),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-failed-recovery",
        "policy_command_failed_recovery",
        "call-command-failed-recovery",
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
        .expect("failed process action should still produce a durable result");
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Failed);

    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("failed process result artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("process result artifact should be textual JSON"),
    )
    .expect("process result artifact should parse as JSON");
    assert_eq!(payload["guidance"]["kind"], "process_action_recovery");
    let message = payload["guidance"]["message"]
        .as_str()
        .expect("recovery guidance should be text");
    assert!(message.contains("unavailable network"));
    assert!(message.contains("host integration"));
    assert!(message.contains("exact filesystem path"));
    assert!(!message.contains("stderr"));
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_denies_dangerous_argv_without_runner_call() {
    let executor = ProcessProposingToolExecutor::with_argv(["sh", "-c", "rm -rf target"]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_dangerous_argv"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-dangerous-argv",
        "policy_command_dangerous_argv",
        "call-command-exec-dangerous-argv",
        tool,
        |builder| {
            builder
                .allow_low_risk_process_actions(Arc::new(runner.clone()))
                .allow_accepted_local_workspace_process_actions(
                    accepted_local_workspace_process_admission(),
                    Arc::new(runner.clone()),
                )
                .permission_admission_source(Arc::new(StaticPermissionAdmissionSource::denying()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("dangerous process proposal should be denied durably");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Failed
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    let proposal = audits[0]
        .proposal()
        .expect("proposed audit should include dangerous argv identity");
    let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
        panic!("proposal should include process action intent");
    };
    assert_eq!(intent.argv(), ["sh", "-c", "rm -rf target"]);
    assert_eq!(intent.stdin_text(), None);
    assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
    let policy = audits[1]
        .policy()
        .expect("denied audit should include policy");
    assert_eq!(policy.risk_tier(), ActionRiskTier::ProcessHigh);
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
}

#[tokio::test(flavor = "current_thread")]
async fn noninteractive_trusted_host_process_allows_high_risk_argv_without_review() {
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
        "runtime-policy-command-trusted-host-high-risk",
        "policy_command_trusted_host_high_risk",
        "call-command-trusted-host-high-risk",
        tool,
        |builder| {
            builder
                .permission_review_mode(PermissionReviewMode::NonInteractiveTrusted)
                .permission_admission_source(Arc::new(admission.clone()))
                .allow_accepted_local_workspace_process_actions(
                    AcceptedLocalWorkspaceProcessAdmission::accept_host_v1(),
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
async fn denied_process_action_traces_denied_tool_finish_without_process_execution() {
    let executor = ProcessProposingToolExecutor::with_argv(["sh", "-c", "rm -rf target"]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_dangerous_trace"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-dangerous-trace",
        "policy_command_dangerous_trace",
        "call-command-exec-dangerous-trace",
        tool,
        |builder| {
            builder
                .allow_low_risk_process_actions(Arc::new(runner.clone()))
                .allow_accepted_local_workspace_process_actions(
                    accepted_local_workspace_process_admission(),
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let (events, logs) = capture_traces_for(
        "runtime-policy-command-exec-dangerous-trace",
        runtime.execute_tool_call(pending.id(), ToolExecutionContext::default()),
    )
    .await;
    let events = events.expect("dangerous process proposal should be denied durably");

    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(runner.call_count(), 0);
    assert!(logs.contains("\"event\":\"runtime.tool.execute.finish\""));
    assert!(logs.contains("\"status\":\"review_failed\""));
    assert!(logs.contains("\"diagnostic_code\":\"permission_review_failed\""));
    assert!(logs.contains("\"tool_name\":\"policy_command_dangerous_trace\""));
    assert!(logs.contains("\"tool_call_id\":\"call-command-exec-dangerous-trace\""));
    assert!(!logs.contains("runtime.process.execute.start"));
    assert!(!logs.contains("runtime.process.execute.finish"));
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_denies_local_workspace_effect_without_accepted_risk_opt_in() {
    let executor =
        ProcessProposingToolExecutor::with_argv(["cargo", "test", "-p", "merry-runtime"]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_local_effect"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-local-effect",
        "policy_command_local_effect",
        "call-command-exec-local-effect",
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
        .expect("local workspace effect process proposal should be denied durably");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Failed
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    let proposal = audits[0]
        .proposal()
        .expect("proposed audit should include local effect argv identity");
    let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
        panic!("proposal should include process action intent");
    };
    assert_eq!(intent.argv(), ["cargo", "test", "-p", "merry-runtime"]);
    assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
    let policy = audits[1]
        .policy()
        .expect("denied audit should include policy");
    assert_eq!(
        policy.risk_tier(),
        ActionRiskTier::ProcessLocalWorkspaceEffect
    );
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_accepted_local_workspace_process_action_executes_local_workspace_effect_and_records_policy()
 {
    let executor =
        ProcessProposingToolExecutor::with_argv(["cargo", "test", "-p", "merry-runtime"]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_accepted_local_effect"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-accepted-local-effect",
        "policy_command_accepted_local_effect",
        "call-command-exec-accepted-local-effect",
        tool,
        |builder| {
            builder
                .allow_accepted_local_workspace_process_actions(
                    accepted_local_workspace_process_admission(),
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("accepted local workspace process action should execute through runner");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(result.diagnostic().is_none());
    assert!(runtime.pending_tool_calls().await.is_empty());

    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("process result artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("process result artifact should be textual JSON"),
    )
    .expect("process result artifact should parse as JSON");
    assert_eq!(
        payload,
        json!({
            "ok": true,
            "kind": "process_action",
            "permission_profile_id": "process.local_workspace.bwrap.v1",
            "status": {
                "kind": "exited",
                "code": 0,
            },
            "intent": {
                "summary": "process argv[0]=cargo; argc=4; cwd=.",
                "argv": ["cargo", "test", "-p", "merry-runtime"],
                "cwd": ".",
            },
            "stdout": {
                "text": "runtime tests passed\n",
                "bytes": "runtime tests passed\n".len(),
                "truncated": false,
                "utf8": true,
                "bytes_base64": "cnVudGltZSB0ZXN0cyBwYXNzZWQK",
            },
            "stderr": {
                "text": "",
                "bytes": 0,
                "truncated": false,
                "utf8": true,
                "bytes_base64": "",
            }
        })
    );
    assert!(payload.get("provider").is_none());
    assert!(payload.get("wire").is_none());

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
    let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
        panic!("proposed audit should record process action intent");
    };
    assert_eq!(intent.argv(), ["cargo", "test", "-p", "merry-runtime"]);
    assert_eq!(intent.cwd(), Some("."));
    assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);
    assert_eq!(intent.stdin_text(), None);
    assert_eq!(runner.observed_intents(), vec![intent.clone()]);

    assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
    assert_eq!(audits[1].tool_call_id(), pending.id());
    assert_eq!(audits[1].tool_name(), pending.name());
    assert_eq!(audits[1].action_kind(), ToolActionKind::CommandExec);
    assert!(audits[1].proposal().is_none());
    let policy = audits[1]
        .policy()
        .expect("executed audit should include process allow policy");
    assert_eq!(
        policy.risk_tier(),
        ActionRiskTier::ProcessLocalWorkspaceEffect
    );
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
    assert_eq!(
        policy.reason(),
        "local workspace effect process actions are allowed only by explicit runtime opt-in for accepted local workspace process risk"
    );
    let ActionExecutionEvidence::ProcessAction(evidence) = audits[1]
        .execution_evidence()
        .expect("executed audit should include process evidence")
    else {
        panic!("process action should record process execution evidence");
    };
    assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
    assert_eq!(evidence.stdout_bytes(), "runtime tests passed\n".len());
    assert!(!evidence.stdout_truncated());
    assert_eq!(evidence.stderr_bytes(), 0);
    assert!(!evidence.stderr_truncated());
    assert_eq!(
        evidence.permission_profile_id().as_str(),
        "process.local_workspace.bwrap.v1"
    );
    assert!(evidence.matches_intent(intent));

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
async fn accepted_local_workspace_process_action_denies_when_admission_profile_mismatches() {
    let executor =
        ProcessProposingToolExecutor::with_argv(["cargo", "test", "-p", "merry-runtime"]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_mismatched_local_effect_profile"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let mismatched_admission =
        AcceptedLocalWorkspaceProcessAdmission::for_test_permission_profile_id(
            ProcessPermissionProfileId::READ_ONLY_V1,
        );
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-mismatched-local-effect-profile",
        "policy_command_mismatched_local_effect_profile",
        "call-command-exec-mismatched-local-effect-profile",
        tool,
        |builder| {
            builder
                .allow_accepted_local_workspace_process_actions(
                    mismatched_admission,
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("mismatched local workspace process profile should be denied durably");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Failed
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
    let policy = audits[1]
        .policy()
        .expect("denied audit should include policy");
    assert_eq!(
        policy.risk_tier(),
        ActionRiskTier::ProcessLocalWorkspaceEffect
    );
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_local_workspace_process_action_executes_unknown_argv_under_bwrap_profile() {
    let executor = ProcessProposingToolExecutor::with_argv(["unknown-readonly-ish", "--version"]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_unknown_argv"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-unknown-argv",
        "policy_command_unknown_argv",
        "call-command-exec-unknown-argv",
        tool,
        |builder| {
            builder
                .allow_low_risk_process_actions(Arc::new(runner.clone()))
                .allow_accepted_local_workspace_process_actions(
                    accepted_local_workspace_process_admission(),
                    Arc::new(runner.clone()),
                )
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("accepted unknown process proposal should execute through runner");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
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
    let proposal = audits[0]
        .proposal()
        .expect("proposed audit should include unknown argv identity");
    let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
        panic!("proposal should include process action intent");
    };
    assert_eq!(intent.argv(), ["unknown-readonly-ish", "--version"]);
    assert_eq!(intent.stdin_text(), None);
    assert_eq!(runner.observed_intents(), vec![intent.clone()]);
    assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
    let policy = audits[1]
        .policy()
        .expect("executed audit should include policy");
    assert_eq!(
        policy.risk_tier(),
        ActionRiskTier::ProcessLocalWorkspaceEffect
    );
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_with_stdin_is_denied_without_runner_call() {
    let executor = ProcessProposingToolExecutor::with_stdin_text("stdin is not admitted\n");
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_stdin"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-stdin",
        "policy_command_stdin",
        "call-command-exec-stdin",
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
        .expect("stdin process proposal should be denied durably");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 0);
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
        "policy_command_stdin",
    );
    assert!(runtime.pending_tool_calls().await.is_empty());

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[0].action_kind(), ToolActionKind::CommandExec);
    let proposal = audits[0]
        .proposal()
        .expect("proposed audit should include stdin process proposal");
    let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
        panic!("proposal should include process action intent");
    };
    assert_eq!(intent.argv(), ["cargo", "test", "-p", "merry-runtime"]);
    assert_eq!(intent.cwd(), Some("."));
    assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);
    assert_eq!(intent.stdin_text(), None);
    assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
    assert_eq!(audits[1].action_kind(), ToolActionKind::CommandExec);
    let policy = audits[1]
        .policy()
        .expect("denied audit should include policy");
    assert_eq!(
        policy.risk_tier(),
        ActionRiskTier::ProcessLocalWorkspaceEffect
    );
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
}

#[test]
fn process_execution_evidence_matches_process_action_kind() {
    let intent = ProcessActionIntent::new(
        vec!["rustc".to_owned(), "--version".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        4096,
        4096,
    )
    .expect("valid process intent");
    let evidence = ProcessExecutionEvidence::new(
        &intent,
        ProcessPermissionProfileId::READ_ONLY_V1,
        ProcessExitStatus::Exited(0),
        64,
        false,
        0,
        false,
    )
    .expect("valid process execution evidence");
    let execution_evidence = ActionExecutionEvidence::ProcessAction(evidence);

    assert!(execution_evidence.matches_action_kind(ToolActionKind::CommandExec));
    assert!(!execution_evidence.matches_action_kind(ToolActionKind::WorkspaceWrite));
}
