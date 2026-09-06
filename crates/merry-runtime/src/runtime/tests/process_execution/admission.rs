use crate::{
    AcceptedLocalWorkspaceProcessAdmission, ActionExecutionEvidence, ActionProposalEvidence,
    ProcessEnvPolicy, ProcessExitStatus, ProcessPermissionProfileId, RegisteredTool,
    ToolActionKind, ToolExecutionContext,
    action_audit::ActionAuditStatus,
    action_policy::{ActionPolicyDisposition, ActionRiskTier},
    ledger::LedgerFactKind,
    runtime::tests::support::{
        common::{accepted_local_workspace_process_admission, capture_traces_for},
        process::{
            FakeProcessRunner, ProcessProposingToolExecutor, StaticPermissionAdmissionSource,
        },
        tool_helpers::{
            action_audit_records, assert_sanitized_policy_denial_content, denied_action_content,
            event_kind_names_for_tool_execution, lifecycle_kinds, policy_tool_spec,
            register_policy_pending_registered_tool_with_builder, resolved_tool_result,
        },
    },
};
use serde_json::json;
use std::sync::Arc;

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
            "permission_profile_id": "process.local_workspace",
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
            },
            "stderr": {
                "text": "",
                "bytes": 0,
                "truncated": false,
                "utf8": true,
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
        "process.local_workspace"
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
            ProcessPermissionProfileId::READ_ONLY,
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
async fn accepted_local_workspace_process_action_executes_unknown_argv_under_local_workspace_profile()
 {
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
