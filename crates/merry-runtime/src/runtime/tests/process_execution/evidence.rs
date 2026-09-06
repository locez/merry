use crate::action_audit::ActionAuditStatus;
use crate::action_policy::{ActionPolicyDisposition, ActionRiskTier};
use crate::ledger::{LedgerFactKind, LedgerProjection, LedgerScope};
use crate::runtime::tests::{
    FakeProcessRunner, ProcessProposingToolExecutor, action_audit_records,
    event_kind_names_for_tool_execution, lifecycle_kinds, policy_tool_spec,
    register_policy_pending_registered_tool_with_builder, resolved_tool_result,
};
use crate::{
    ActionExecutionEvidence, ActionProposalEvidence, ProcessActionIntent, ProcessEnvPolicy,
    ProcessExecutionEvidence, ProcessExitStatus, ProcessPermissionProfileId, RegisteredTool,
    ToolActionKind, ToolExecutionContext,
};
use merry_core::RuntimeJournalPayload;
use serde_json::json;
use std::sync::Arc;

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
            "permission_profile_id": "process.read_only",
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
        "process.read_only"
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
    assert!(observation_text.contains("permission_profile=process.read_only"));
    assert!(observation_text.contains("stdout_bytes=21"));
    assert!(observation_text.contains("stderr_bytes=0"));
    assert!(observation_text.contains(&format!("artifact={}", result.artifact().id().as_str())));
    assert!(!observation_text.contains("runtime tests passed"));
}

#[tokio::test(flavor = "current_thread")]
async fn process_action_artifact_preserves_non_utf8_output_as_base64() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::succeeding_with_non_utf8_output();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_non_utf8_output"),
        Arc::new(executor),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-non-utf8-output",
        "policy_command_non_utf8_output",
        "call-command-non-utf8-output",
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
        .expect("non-UTF-8 process output should still produce a result");

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

    assert_eq!(payload["stdout"]["utf8"], false);
    assert_eq!(payload["stdout"]["bytes"], 3);
    assert_eq!(payload["stdout"]["bytes_base64"], "/wBh");
    assert_eq!(payload["stderr"]["utf8"], false);
    assert_eq!(payload["stderr"]["bytes"], 2);
    assert_eq!(payload["stderr"]["bytes_base64"], "b/4=");
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
    assert!(message.contains("network"));
    assert!(message.contains("permissions"));
    assert!(message.contains("host integration"));
    assert!(message.contains("exact filesystem path"));
    assert!(!message.contains("stderr"));
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
        ProcessPermissionProfileId::READ_ONLY,
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
