use crate::{
    ActionExecutionEvidence, ProcessActionIntent, ProcessEnvPolicy, ProcessExecutionEvidence,
    ProcessExitStatus, ProcessPermissionProfileId, RegisteredTool, ToolActionKind,
    ToolExecutionContext,
    runtime::tests::support::{
        process::{FakeProcessRunner, ProcessProposingToolExecutor},
        tool_helpers::{
            event_kind_names_for_tool_execution, policy_tool_spec,
            register_policy_pending_registered_tool_with_builder, resolved_artifact_json,
            resolved_tool_result,
        },
    },
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
        ["ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(result.diagnostic().is_none());
    assert!(matches!(
        &events[0].payload,
        RuntimeJournalPayload::ToolCallResolved { result: resolved } if resolved == result
    ));
    assert!(runtime.pending_tool_calls().await.is_empty());

    let payload = resolved_artifact_json(&runtime, result, "process result").await;
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
    assert_eq!(runner.observed_intents().len(), 1);
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
    let payload = resolved_artifact_json(&runtime, result, "process result").await;

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
    let payload = resolved_artifact_json(&runtime, result, "process result").await;

    assert_eq!(payload["stdout"]["truncated"], true);
    assert_eq!(payload["stderr"]["truncated"], false);
    assert_eq!(payload["guidance"]["kind"], "process_output_truncated");
    assert_eq!(payload["guidance"]["stdout_truncated"], true);
    assert_eq!(payload["guidance"]["stderr_truncated"], false);
    assert!(
        payload["guidance"]["message"]
            .as_str()
            .expect("guidance message should be text")
            .contains("Do not repeat a side-effecting command")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failed_process_action_preserves_the_error_without_guessing_required_capabilities() {
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

    let payload = resolved_artifact_json(&runtime, result, "failed process result").await;
    assert_eq!(payload["status"], json!({ "kind": "exited", "code": 1 }));
    assert_eq!(payload["stderr"]["text"], "permission denied\n");
    assert!(payload.get("guidance").is_none());
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
