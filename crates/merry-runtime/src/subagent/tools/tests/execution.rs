use super::{CapturingChildFactory, manager, outcome_json, pending_call};
use crate::subagent::tools::{SUBAGENT_INVALID_ARGUMENTS_CODE, SUBAGENT_SPAWN_REJECTED_CODE};
use crate::{
    DEFAULT_MAX_MODEL_TURNS, SubagentConfig, SubagentManager, SubagentStatusLabel,
    SubagentTaskSpec, ToolExecutionContext, ToolExecutionError, ToolExecutor,
    subagent::tools::{CancelSubagentsExecutor, SpawnSubagentsExecutor, WaitSubagentsExecutor},
    subagent::{CANCEL_SUBAGENTS_TOOL_NAME, SPAWN_SUBAGENTS_TOOL_NAME, WAIT_SUBAGENTS_TOOL_NAME},
};
use merry_core::{SessionId, ToolCallResultStatus, ToolName};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn spawn_tool_returns_structured_output_and_preserves_task_input() {
    let factory = Arc::new(CapturingChildFactory::default());
    let executor = SpawnSubagentsExecutor::new(manager(factory.clone()));
    let call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({
            "max_concurrency": 1,
            "tasks": [{
                "task": "Review the runtime module.",
                "display_name": "Runtime review",
                "max_model_turns": 3,
                "allowed_tools": ["read_text"],
                "read_scope": ["crates/merry-runtime/src"],
                "write_scope": ["tmp/subagent-output"],
                "forbidden_paths": ["target", ".git"],
                "expected_output": "Return a compact findings list.",
                "reasoning_effort": "low"
            }]
        }),
    );

    let outcome = executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("spawn execution should succeed");
    let output: crate::subagent::SpawnSubagentsOutput =
        serde_json::from_value(outcome_json(&outcome)).expect("spawn output is structured");
    let captured = factory.inputs();

    assert_eq!(output.spawned.len(), 1);
    assert!(output.rejected.is_empty());
    assert_eq!(
        output.spawned[0].display_name.as_deref(),
        Some("Runtime review")
    );
    assert_eq!(
        output.spawned[0].read_scope,
        vec!["crates/merry-runtime/src".to_owned()]
    );
    assert_eq!(
        output.spawned[0].write_scope,
        vec!["tmp/subagent-output".to_owned()]
    );
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].task.display_name(), Some("Runtime review"));
    assert_eq!(captured[0].task.max_model_turns(), 3);
    assert_eq!(
        captured[0].task.allowed_tools(),
        &[ToolName::new("read_text").expect("valid tool name")]
    );
    assert_eq!(
        captured[0].allowed_tools,
        vec![ToolName::new("read_text").expect("valid tool name")]
    );
    assert_eq!(
        captured[0].task.read_scope(),
        &[PathBuf::from("crates/merry-runtime/src")]
    );
    assert_eq!(
        captured[0].workspace_scope.read_scope(),
        &[PathBuf::from("crates/merry-runtime/src")]
    );
    assert_eq!(
        captured[0].task.write_scope(),
        &[PathBuf::from("tmp/subagent-output")]
    );
    assert_eq!(
        captured[0].workspace_scope.write_scope(),
        &[PathBuf::from("tmp/subagent-output")]
    );
    assert_eq!(
        captured[0].task.forbidden_paths(),
        &[PathBuf::from(".git"), PathBuf::from("target")]
    );
    assert_eq!(
        captured[0].workspace_scope.forbidden_paths(),
        &[PathBuf::from(".git"), PathBuf::from("target")]
    );
    assert_eq!(
        captured[0].task.expected_output(),
        Some("Return a compact findings list.")
    );
    assert_eq!(
        captured[0]
            .task
            .reasoning_effort()
            .map(|effort| effort.as_str()),
        Some("low")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_tool_returns_failed_outcome_when_all_tasks_are_rejected() {
    let factory = Arc::new(CapturingChildFactory::default());
    let manager = SubagentManager::runtime_controlled(
        SessionId::new("spawn-all-rejected").expect("valid session id"),
        SubagentConfig::default(),
        factory.clone(),
        false,
    );
    let executor = SpawnSubagentsExecutor::new(manager);
    let call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({"tasks": [{"task": "This must not start."}]}),
    );

    let outcome = executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("rejected spawn should resolve as a failed tool outcome");

    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        outcome
            .diagnostic()
            .expect("rejected spawn should include a diagnostic")
            .code(),
        SUBAGENT_SPAWN_REJECTED_CODE
    );
    let payload = outcome_json(&outcome);
    assert!(
        payload["spawned"]
            .as_array()
            .expect("spawned array")
            .is_empty()
    );
    assert_eq!(payload["rejected"].as_array().map(Vec::len), Some(1));
    assert!(factory.inputs().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_tool_defaults_max_model_turns_when_omitted() {
    let factory = Arc::new(CapturingChildFactory::default());
    let executor = SpawnSubagentsExecutor::new(manager(factory.clone()));
    let call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({
            "tasks": [{
                "task": "Use the default model-turn limit."
            }]
        }),
    );

    executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("spawn execution should succeed");
    let captured = factory.inputs();

    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].task.max_model_turns(), DEFAULT_MAX_MODEL_TURNS);
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_tool_uses_manager_configured_default_max_model_turns() {
    let factory = Arc::new(CapturingChildFactory::default());
    let config = SubagentConfig::default()
        .with_max_model_turns(2048)
        .expect("positive child model-turn limit");
    let manager = SubagentManager::new(
        SessionId::new("configured-default").expect("valid session id"),
        config,
        factory.clone(),
    );
    let executor = SpawnSubagentsExecutor::new(manager);
    let call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({
            "tasks": [{"task": "Use the configured model-turn limit."}]
        }),
    );

    executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("spawn execution should succeed");

    assert_eq!(factory.inputs()[0].task.max_model_turns(), 2048);
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_tool_uses_random_child_session_ids() {
    let factory = Arc::new(CapturingChildFactory::default());
    let executor = SpawnSubagentsExecutor::new(manager(factory.clone()));
    let call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({
            "max_concurrency": 2,
            "tasks": [
                { "task": "Inspect one file." },
                { "task": "Inspect another file." }
            ]
        }),
    );

    executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("spawn execution should succeed");
    let captured = factory.inputs();

    assert_eq!(captured.len(), 2);
    let first = captured[0].session_id.as_str();
    let second = captured[1].session_id.as_str();
    assert_ne!(first, second);
    assert_eq!(first.len(), 36);
    assert_eq!(second.len(), 36);
    assert!(!first.starts_with("parent-agent-"));
    assert!(!second.starts_with("parent-agent-"));
}

#[tokio::test(flavor = "current_thread")]
async fn wait_tool_returns_status_output() {
    let manager = manager(Arc::new(CapturingChildFactory::default()));
    let spawn = manager
        .spawn(
            vec![SubagentTaskSpec::new("Stay queued for wait.", 2).expect("valid task")],
            Some(0),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let executor = WaitSubagentsExecutor::new(manager);
    let call = pending_call(
        WAIT_SUBAGENTS_TOOL_NAME,
        json!({
            "agent_ids": [spawn.spawned[0].agent_id],
            "timeout_ms": 0
        }),
    );

    let outcome = executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("wait execution should succeed");
    let output: crate::subagent::WaitSubagentsOutput =
        serde_json::from_value(outcome_json(&outcome)).expect("wait output is structured");

    assert_eq!(output.agents.len(), 1);
    assert_eq!(output.agents[0].status, SubagentStatusLabel::Queued);
    assert_eq!(output.agents[0].summary, "child queued");
    let payload = outcome_json(&outcome);
    assert_eq!(payload["timed_out"], true);
    assert_eq!(payload["terminal"], false);
    assert_eq!(
        payload["pending_agent_ids"].as_array().map(Vec::len),
        Some(1)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_tool_returns_cancelled_status_output() {
    let manager = manager(Arc::new(CapturingChildFactory::default()));
    let spawn = manager
        .spawn(
            vec![SubagentTaskSpec::new("Cancel queued child.", 2).expect("valid task")],
            Some(0),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let executor = CancelSubagentsExecutor::new(manager);
    let call = pending_call(
        CANCEL_SUBAGENTS_TOOL_NAME,
        json!({
            "agent_ids": [spawn.spawned[0].agent_id]
        }),
    );

    let outcome = executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("cancel execution should succeed");
    let output: crate::subagent::WaitSubagentsOutput =
        serde_json::from_value(outcome_json(&outcome)).expect("cancel output is structured");

    assert_eq!(output.agents.len(), 1);
    assert_eq!(output.agents[0].status, SubagentStatusLabel::Cancelled);
    assert_eq!(output.agents[0].summary, "child cancelled by parent");
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_tool_invalid_task_or_path_returns_failed_outcome() {
    let executor = SpawnSubagentsExecutor::new(manager(Arc::new(CapturingChildFactory::default())));

    for arguments in [
        json!({ "tasks": [{ "task": " " }] }),
        json!({ "tasks": [{ "task": "Bad path.", "read_scope": ["../secret"] }] }),
        json!({ "tasks": [{ "task": "Bad control path.", "read_scope": ["bad\npath"] }] }),
        json!({ "tasks": [{ "task": "Bad effort.", "reasoning_effort": "bad\neffort" }] }),
    ] {
        let call = pending_call(SPAWN_SUBAGENTS_TOOL_NAME, arguments);
        let outcome = executor
            .execute(call, ToolExecutionContext::default())
            .await
            .expect("invalid input should resolve as failed tool outcome");
        let diagnostic = outcome
            .diagnostic()
            .expect("failed subagent arguments should include diagnostic");
        let payload = outcome_json(&outcome);

        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(diagnostic.code(), SUBAGENT_INVALID_ARGUMENTS_CODE);
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["error"]["code"], SUBAGENT_INVALID_ARGUMENTS_CODE);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_tool_recovery_rejects_provider_namespaced_allowed_tools() {
    let executor = SpawnSubagentsExecutor::new(manager(Arc::new(CapturingChildFactory::default())));
    let call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({
            "tasks": [{
                "task": "Inspect the runtime.",
                "allowed_tools": ["functions.run_process"]
            }]
        }),
    );

    let outcome = executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("invalid input should resolve as a failed tool outcome");
    let payload = outcome_json(&outcome);
    let recovery = payload["recovery"]["tool_name_contract"]
        .as_str()
        .expect("tool-name recovery should be present");

    assert!(recovery.contains("exact registered Merry tool names"));
    assert!(recovery.contains("run_process"));
    assert!(recovery.contains("functions.run_process"));
}

#[tokio::test(flavor = "current_thread")]
async fn provider_input_shape_errors_return_failed_outcome() {
    let executor = WaitSubagentsExecutor::new(manager(Arc::new(CapturingChildFactory::default())));
    let call = pending_call(
        WAIT_SUBAGENTS_TOOL_NAME,
        json!({
            "agent_ids": [],
            "unexpected": true
        }),
    );

    let outcome = executor
        .execute(call, ToolExecutionContext::default())
        .await
        .expect("invalid provider-visible input should resolve as failed outcome");
    let diagnostic = outcome
        .diagnostic()
        .expect("failed input should include diagnostic");
    let payload = outcome_json(&outcome);

    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_eq!(diagnostic.code(), SUBAGENT_INVALID_ARGUMENTS_CODE);
    assert_eq!(payload["tool"], WAIT_SUBAGENTS_TOOL_NAME);
}

#[tokio::test(flavor = "current_thread")]
async fn wait_tool_without_timeout_honors_cancellation() {
    let manager = manager(Arc::new(CapturingChildFactory::default()));
    let spawn = manager
        .spawn(
            vec![SubagentTaskSpec::new("Stay queued until cancellation.", 2).expect("valid task")],
            Some(0),
            CancellationToken::new(),
        )
        .await
        .expect("spawn should succeed");
    let executor = WaitSubagentsExecutor::new(manager);
    let call = pending_call(
        WAIT_SUBAGENTS_TOOL_NAME,
        json!({
            "agent_ids": [spawn.spawned[0].agent_id],
            "mode": "all"
        }),
    );
    let token = CancellationToken::new();
    token.cancel();

    let error = executor
        .execute(call, ToolExecutionContext::new(token))
        .await
        .expect_err("pre-cancelled wait should not resolve");

    assert!(matches!(error, ToolExecutionError::Cancelled));
}
