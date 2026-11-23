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
                "allowed_tools": ["read_file"],
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
        &[ToolName::new("read_file").expect("valid tool name")]
    );
    assert_eq!(
        captured[0].allowed_tools,
        vec![ToolName::new("read_file").expect("valid tool name")]
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
async fn spawn_tool_uses_distinct_agent_scoped_child_session_ids() {
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
    assert_eq!(
        Some(first),
        captured[0].agent_id.as_ref().map(|id| id.as_str())
    );
    assert_eq!(
        Some(second),
        captured[1].agent_id.as_ref().map(|id| id.as_str())
    );
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

#[tokio::test(flavor = "current_thread")]
async fn tolerant_deserialization_handles_sentinels_single_task_and_singular_agent_id() {
    let factory = Arc::new(CapturingChildFactory::default());
    let mgr = manager(factory.clone());
    let spawn_executor = SpawnSubagentsExecutor::new(mgr.clone());

    // Single-task shorthand + sentinel strings ("null", "none", "") + quoted path
    let spawn_call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({
            "task": "Inspect single task shorthand.",
            "display_name": "null",
            "role": "explore",
            "expected_output": "none",
            "read_scope": ["\"crates/merry-runtime\""],
            "write_scope": "undefined"
        }),
    );
    let outcome = spawn_executor
        .execute(spawn_call, ToolExecutionContext::default())
        .await
        .expect("tolerant spawn should succeed");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let output: crate::subagent::SpawnSubagentsOutput =
        serde_json::from_value(outcome_json(&outcome)).expect("spawn output parses");
    assert_eq!(output.spawned.len(), 1);
    assert_eq!(output.spawned[0].display_name, None);
    assert_eq!(output.spawned[0].role.as_deref(), Some("explore"));
    assert_eq!(output.spawned[0].read_scope, vec!["crates/merry-runtime"]);
    let agent_id = output.spawned[0].agent_id.clone();

    // Singular `agent_id` in wait_subagents
    let wait_executor = WaitSubagentsExecutor::new(mgr.clone());
    let wait_call = pending_call(
        WAIT_SUBAGENTS_TOOL_NAME,
        json!({ "agent_id": agent_id.as_str(), "timeout_ms": 0 }),
    );
    let wait_outcome = wait_executor
        .execute(wait_call, ToolExecutionContext::default())
        .await
        .expect("singular wait should succeed");
    assert_eq!(wait_outcome.status(), ToolCallResultStatus::Succeeded);

    // Singular `agent_id` in cancel_subagents
    let cancel_executor = CancelSubagentsExecutor::new(mgr);
    let cancel_call = pending_call(
        CANCEL_SUBAGENTS_TOOL_NAME,
        json!({ "agent_id": agent_id.as_str() }),
    );
    let cancel_outcome = cancel_executor
        .execute(cancel_call, ToolExecutionContext::default())
        .await
        .expect("singular cancel should succeed");
    assert_eq!(cancel_outcome.status(), ToolCallResultStatus::Succeeded);
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_wait_hands_off_to_background_on_interrupt_without_cancelling_child() {
    use crate::{
        ChildRuntimeFactory, ChildRuntimeInput, Runtime, RuntimeError,
        subagent::tools::SendSubagentMessageExecutor,
    };
    use futures_util::stream;
    use merry_core::ProviderName;
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
        ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
        ModelStreamContext,
    };
    use std::sync::Mutex;

    #[derive(Clone)]
    struct GatedChildProvider {
        release: CancellationToken,
        replies: Arc<Mutex<Vec<String>>>,
    }

    impl ModelProvider for GatedChildProvider {
        fn name(&self) -> &ProviderName {
            static NAME: std::sync::OnceLock<ProviderName> = std::sync::OnceLock::new();
            NAME.get_or_init(|| ProviderName::new("gated-test").expect("valid"))
        }

        fn capabilities(&self) -> &ModelCapabilities {
            static CAPS: std::sync::OnceLock<ModelCapabilities> = std::sync::OnceLock::new();
            CAPS.get_or_init(|| {
                ModelCapabilities::new(true, true, false, true, None, None).expect("valid")
            })
        }

        fn stream_model<'a>(
            &'a self,
            _request: ModelRequest,
            context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            let release = self.release.clone();
            let reply = self.replies.lock().expect("replies lock").remove(0);
            Box::pin(async move {
                let event_stream = stream::once(async move {
                    tokio::select! {
                        () = context.cancellation_token().cancelled() => Err(ModelError::Cancelled),
                        () = release.cancelled() => Ok(ModelEvent::Completed {
                            response: ModelResponse::new(
                                vec![ModelOutput::text(&reply)],
                                FinishReason::Stop,
                                None,
                            ),
                        }),
                    }
                });
                Ok(Box::pin(event_stream) as ModelEventStream)
            })
        }
    }

    struct GatedFactory {
        release: CancellationToken,
        replies: Arc<Mutex<Vec<String>>>,
    }

    impl ChildRuntimeFactory for GatedFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
            Runtime::builder(input.session_id)
                .model_provider(
                    Arc::new(GatedChildProvider {
                        release: self.release.clone(),
                        replies: Arc::clone(&self.replies),
                    }),
                    ModelName::new("fake/child").expect("valid"),
                )
                .build()
        }
    }

    let release = CancellationToken::new();
    let replies = Arc::new(Mutex::new(vec![
        "first pass complete".to_owned(),
        "second pass after wake".to_owned(),
    ]));
    let mgr = manager(Arc::new(GatedFactory {
        release: release.clone(),
        replies,
    }));
    let spawn_executor = SpawnSubagentsExecutor::new(mgr.clone());

    let parent_turn_token = CancellationToken::new();
    let interrupt_task = tokio::spawn({
        let token = parent_turn_token.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            token.cancel();
        }
    });

    let call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({
            "wait": "all",
            "tasks": [{ "task": "Run gated work.", "role": "explore" }]
        }),
    );
    let outcome = spawn_executor
        .execute(call, ToolExecutionContext::new(parent_turn_token))
        .await
        .expect("interrupted foreground wait must hand off instead of failing");
    interrupt_task.await.expect("interrupt task");

    let output: crate::subagent::SpawnSubagentsOutput =
        serde_json::from_value(outcome_json(&outcome)).expect("spawn output parses");
    let handoff = output
        .background_handoff
        .expect("foreground wait interrupt must produce background_handoff");
    assert_eq!(handoff.reason, "parent_turn_interrupted");
    assert!(handoff.message.contains("NOT cancelled"));
    let agent_id = output.spawned[0].agent_id.clone();

    // Release the gate; child must still finish cleanly in the background.
    release.cancel();
    let waited = mgr
        .wait(std::slice::from_ref(&agent_id), crate::WaitMode::All, None)
        .await
        .expect("child should complete in background");
    assert_eq!(waited.agents[0].status, SubagentStatusLabel::Completed);
    assert_eq!(
        waited.agents[0]
            .result
            .as_ref()
            .map(|r| r.conclusion.as_str()),
        Some("first pass complete")
    );
    assert!(waited.agents[0].raw_output_bytes > 0);

    // High-signal notification must inline conclusion and send_subagent_message hint.
    let notification = crate::subagent::completion_notification_text(&waited.agents);
    assert!(notification.contains("<subagent-notification>"));
    assert!(notification.contains("first pass complete"));
    assert!(notification.contains("send_subagent_message"));

    // Wake the completed child with send_subagent_message and wait inline.
    let send_executor = SendSubagentMessageExecutor::new(mgr);
    let wake_call = pending_call(
        crate::subagent::SEND_SUBAGENT_MESSAGE_TOOL_NAME,
        json!({
            "agent_id": agent_id.as_str(),
            "message": "Now do pass two.",
            "wait": true
        }),
    );
    let wake_outcome = send_executor
        .execute(wake_call, ToolExecutionContext::default())
        .await
        .expect("send_subagent_message wake should succeed");
    let wake_output: crate::subagent::SendSubagentMessageOutput =
        serde_json::from_value(outcome_json(&wake_outcome)).expect("wake output parses");
    assert_eq!(
        wake_output.outcome,
        crate::subagent::SendSubagentMessageOutcome::Woken
    );
    assert_eq!(wake_output.status, Some(SubagentStatusLabel::Completed));
    let wake_wait = wake_output.wait_result.expect("wait_result present");
    assert_eq!(
        wake_wait.agents[0]
            .result
            .as_ref()
            .map(|r| r.conclusion.as_str()),
        Some("second pass after wake")
    );
    assert_eq!(wake_wait.agents[0].turns_used, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn send_subagent_message_wakes_blocked_max_turns_child_and_settles_dangling_tool_calls() {
    use crate::{
        ChildRuntimeFactory, ChildRuntimeInput, RegisteredTool, Runtime, RuntimeError,
        ToolExecutionOutcome, subagent::tools::SendSubagentMessageExecutor,
    };
    use futures_util::stream;
    use merry_core::{ProviderName, ToolInputSchema, ToolSpec};
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
        ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
        ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
    };
    use std::sync::Mutex;

    struct NoopReadExecutor;
    impl ToolExecutor for NoopReadExecutor {
        fn execute<'a>(
            &'a self,
            _call: merry_core::PendingToolCall,
            _context: ToolExecutionContext,
        ) -> crate::ToolExecutorFuture<'a> {
            Box::pin(async move { Ok(ToolExecutionOutcome::succeeded_text("ok")) })
        }
    }

    #[derive(Clone)]
    struct BlockedThenCompleteProvider {
        steps: Arc<Mutex<Vec<ModelResponse>>>,
    }

    impl ModelProvider for BlockedThenCompleteProvider {
        fn name(&self) -> &ProviderName {
            static NAME: std::sync::OnceLock<ProviderName> = std::sync::OnceLock::new();
            NAME.get_or_init(|| ProviderName::new("blocked-wake-test").expect("valid"))
        }

        fn capabilities(&self) -> &ModelCapabilities {
            static CAPS: std::sync::OnceLock<ModelCapabilities> = std::sync::OnceLock::new();
            CAPS.get_or_init(|| {
                ModelCapabilities::new(true, true, false, true, None, None).expect("valid")
            })
        }

        fn stream_model<'a>(
            &'a self,
            _request: ModelRequest,
            _context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            let response = self.steps.lock().expect("steps lock").remove(0);
            Box::pin(async move {
                let event_stream =
                    stream::once(async move { Ok(ModelEvent::Completed { response }) });
                Ok(Box::pin(event_stream) as ModelEventStream)
            })
        }
    }

    struct BlockedChildFactory {
        steps: Arc<Mutex<Vec<ModelResponse>>>,
    }

    impl ChildRuntimeFactory for BlockedChildFactory {
        fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, RuntimeError> {
            let tool_spec = ToolSpec::new(
                ToolName::new("read_file").expect("valid"),
                "Read text",
                ToolInputSchema::new(
                    schemars::Schema::try_from(json!({
                        "type": "object",
                        "properties": { "path": { "type": "string" } }
                    }))
                    .expect("valid schema"),
                )
                .expect("valid schema"),
            )
            .expect("valid spec");
            Runtime::builder(input.session_id)
                .register_tool(RegisteredTool::read_only(
                    tool_spec,
                    Arc::new(NoopReadExecutor),
                ))
                .model_provider(
                    Arc::new(BlockedThenCompleteProvider {
                        steps: Arc::clone(&self.steps),
                    }),
                    ModelName::new("fake/child").expect("valid"),
                )
                .build()
        }
    }

    // Turn 1 asks for a tool call, hitting max_model_turns=1 and leaving a pending tool call!
    // Turn 2 (after wake with extra_model_turns=4) must settle the dangling call and finish cleanly.
    let steps = Arc::new(Mutex::new(vec![
        ModelResponse::new(
            vec![ModelOutput::tool_call(ModelToolCall::new(
                ModelToolCallId::new("call-dangling").expect("valid"),
                ToolName::new("read_file").expect("valid"),
                ToolArguments::try_from(json!({ "path": "src/lib.rs" })).expect("valid"),
            ))],
            FinishReason::ToolCalls,
            None,
        ),
        ModelResponse::new(
            vec![ModelOutput::text("recovered after extra turns")],
            FinishReason::Stop,
            None,
        ),
    ]));
    let mgr = manager(Arc::new(BlockedChildFactory { steps }));
    let spawn_executor = SpawnSubagentsExecutor::new(mgr.clone());
    let spawn_call = pending_call(
        SPAWN_SUBAGENTS_TOOL_NAME,
        json!({
            "wait": "all",
            "tasks": [{
                "task": "Work with 1 turn budget.",
                "max_model_turns": 1
            }]
        }),
    );
    let spawn_outcome = spawn_executor
        .execute(spawn_call, ToolExecutionContext::default())
        .await
        .expect("spawn should succeed");
    let spawn_output: crate::subagent::SpawnSubagentsOutput =
        serde_json::from_value(outcome_json(&spawn_outcome)).expect("spawn output parses");
    let wait_res = spawn_output.wait_result.expect("wait_result present");
    assert_eq!(wait_res.agents[0].status, SubagentStatusLabel::Blocked);
    let agent_id = spawn_output.spawned[0].agent_id.clone();

    let send_executor = SendSubagentMessageExecutor::new(mgr);
    let wake_call = pending_call(
        crate::subagent::SEND_SUBAGENT_MESSAGE_TOOL_NAME,
        json!({
            "agent_id": agent_id.as_str(),
            "message": "Continue with more turns.",
            "extra_model_turns": 4,
            "wait": true
        }),
    );
    let wake_outcome = send_executor
        .execute(wake_call, ToolExecutionContext::default())
        .await
        .expect("wake blocked child should succeed");
    let wake_output: crate::subagent::SendSubagentMessageOutput =
        serde_json::from_value(outcome_json(&wake_outcome)).expect("wake output parses");
    assert_eq!(
        wake_output.outcome,
        crate::subagent::SendSubagentMessageOutcome::Woken
    );
    assert_eq!(wake_output.status, Some(SubagentStatusLabel::Completed));
    assert_eq!(
        wake_output.wait_result.expect("wait_result").agents[0]
            .result
            .as_ref()
            .map(|r| r.conclusion.as_str()),
        Some("recovered after extra turns")
    );
}
