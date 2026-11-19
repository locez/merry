use crate::{
    AcceptedLocalWorkspaceProcessAdmission, ProcessEnvPolicy, ProcessPermissionProfileId,
    RegisteredTool, ToolActionKind, ToolExecutionContext,
    runtime::tests::support::{
        common::{accepted_local_workspace_process_admission, capture_traces_for},
        process::{
            FakeProcessRunner, ProcessProposingToolExecutor, StaticPermissionAdmissionSource,
        },
        tool_helpers::{
            assert_sanitized_policy_denial_content, denied_action_content,
            event_kind_names_for_tool_execution, policy_tool_spec,
            register_policy_pending_registered_tool_with_builder, resolved_artifact_json,
            resolved_tool_result,
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
        ["ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Failed
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
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
        ["ToolCallResolved"]
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
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_accepted_local_workspace_process_action_executes_local_workspace_effect() {
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
        ["ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(result.diagnostic().is_none());
    assert!(runtime.pending_tool_calls().await.is_empty());

    let payload = resolved_artifact_json(&runtime, result, "process result").await;
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

    let observed = runner.observed_intents();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].argv(), ["cargo", "test", "-p", "merry-runtime"]);
    assert_eq!(observed[0].cwd(), Some("."));
    assert_eq!(observed[0].env_policy(), ProcessEnvPolicy::Empty);
    assert_eq!(observed[0].stdin_text(), None);
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
        ["ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Failed
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
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
        ["ToolCallResolved"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        merry_core::ToolCallResultStatus::Succeeded
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let observed = runner.observed_intents();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].argv(), ["unknown-readonly-ish", "--version"]);
    assert_eq!(observed[0].stdin_text(), None);
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
        ["ToolCallResolved"]
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
}
