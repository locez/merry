use super::*;

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_process_command_invalid_arguments_resolve_failed_and_continue() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(
            model_tool_call_with_arguments(
                "call-bad-process-argv",
                "run_process",
                json!({ "argv": "cargo test -p merry-runtime" }),
            ),
        ))],
        vec![Ok(completed_text_event("final after bad process argv"))],
    ]);
    let runner = RecordingProcessRunner::succeeding("must not run\n");
    let runtime = Runtime::builder(session_id("agent-loop-process-command-invalid-args"))
        .register_tool(
            process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a local process from argv through runtime policy",
            )
            .expect("process command tool should build"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .allow_low_risk_process_actions(Arc::new(runner.clone()))
        .build()
        .expect("runtime should build");

    let result = run_default_loop(&runtime, "Run process with malformed argv.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(runner.observed_intents(), Vec::<ProcessActionIntent>::new());
    assert!(runtime.pending_tool_calls().await.is_empty());

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    let continuation = &requests[1].continuations()[0];
    assert_eq!(continuation.call().id().as_str(), "call-bad-process-argv");
    assert_eq!(continuation.result().status(), ToolCallResultStatus::Failed);
    assert_eq!(
        continuation
            .result()
            .diagnostic()
            .expect("invalid argv should include diagnostic")
            .code(),
        "tool_input_schema_invalid"
    );
    let content = continuation
        .result()
        .content()
        .as_json()
        .expect("invalid argv result should be JSON");
    let value: Value = serde_json::from_str(content).expect("invalid argv JSON should parse");
    assert_eq!(value["error"]["code"], "tool_input_schema_invalid");
    assert!(
        !value["error"]["violations"]
            .as_array()
            .expect("schema failure should list violations")
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_trace_includes_checkpoint_budget_diagnostics_without_prompt_projection() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("final answer"))]]);
    let runtime = Runtime::builder(session_id("agent-loop-context-budget-trace"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .task_anchor(TaskAnchor::new("Keep budget diagnostics separate.").expect("valid anchor"))
        .build()
        .expect("runtime should build");

    let (result, logs) = capture_traces_for(
        "agent-loop-context-budget-trace",
        runtime.run_agent_loop(
            StepInput::user_text("Use a short request.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(2).expect("valid config"),
        ),
    )
    .await;

    let result = result.expect("agent loop should run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(logs.contains("\"event\":\"runtime.provider.request\""));
    assert!(logs.contains("\"context_window_source\":\"fallback\""));
    assert!(logs.contains("\"context_budget_policy\":\"balanced\""));
    assert!(logs.contains("\"checkpoint_decision\":\"continue\""));
    assert!(logs.contains("\"dynamic_body_estimated_tokens\":"));
    assert!(logs.contains("\"soft_water_tokens\":"));
    assert!(logs.contains("\"hard_water_tokens\":"));

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .messages()
            .iter()
            .all(|message| !message.content().as_text().contains("checkpoint_decision")),
        "checkpoint diagnostics must not be projected into prompt messages"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_is_blocked_when_context_budget_is_unavailable() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("final answer"))]])
        .with_capabilities(
            ModelCapabilities::new(true, true, false, true, Some(100), Some(100))
                .expect("valid capabilities"),
        );
    let runtime = Runtime::builder(session_id("agent-loop-context-budget-unavailable"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    let (result, logs) = capture_traces_for(
        "agent-loop-context-budget-unavailable",
        runtime.run_agent_loop(
            StepInput::user_text("Use a short request.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(2).expect("valid config"),
        ),
    )
    .await;

    let result = result.expect("agent loop should run");
    assert!(matches!(
        result.status(),
        AgentLoopStatus::Failed { diagnostic }
            if diagnostic.code() == "auto_compaction"
                && diagnostic.message().contains(
                    "cannot confirm request budget before automatic context reduction"
                )
    ));
    assert!(logs.contains("\"event\":\"runtime.provider.request.context_budget_unavailable\""));
    assert!(!logs.contains("\"event\":\"runtime.provider.request\""));
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn provider_request_still_runs_when_budget_is_unavailable_and_auto_compaction_is_disabled() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_text_event("final answer"))]])
        .with_capabilities(
            ModelCapabilities::new(true, true, false, true, Some(100), Some(100))
                .expect("valid capabilities"),
        );
    let runtime = Runtime::builder(session_id("agent-loop-disabled-context-budget-unavailable"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime should build");

    let (result, logs) = capture_traces_for(
        "agent-loop-disabled-context-budget-unavailable",
        runtime.run_agent_loop(
            StepInput::user_text("Use a short request.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::new(2).expect("valid config"),
        ),
    )
    .await;

    let result = result.expect("agent loop should run");
    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert!(logs.contains("\"event\":\"runtime.provider.request.context_budget_unavailable\""));
    assert!(logs.contains("\"event\":\"runtime.provider.request\""));

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].messages().iter().all(|message| {
            !message
                .content()
                .as_text()
                .contains("context_budget_unavailable")
        }),
        "budget diagnostics must remain trace-only"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unregistered_tool_resolves_failed_and_continues_once() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-missing-tool",
            "missing_tool",
        )))],
        vec![Ok(completed_text_event("final after missing tool"))],
    ]);
    let runtime = runtime_with_provider("agent-loop-unregistered", provider.clone());

    let result = run_default_loop(&runtime, "Call missing tool.").await;

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    let resolved = result
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool call should resolve");
    assert_eq!(resolved.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        resolved
            .diagnostic()
            .expect("unregistered tool result should have diagnostic")
            .code(),
        "tool_not_registered"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(provider.recorded_requests()[1].continuations().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn denied_registered_tool_resolves_failed_and_agent_loop_continues_once() {
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-policy-denied",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("final after policy denial"))],
    ]);
    let executor = ScriptedToolExecutor::succeeding_text("executor must not run\n");
    let runtime = runtime_with_tool_action(
        "agent-loop-policy-denied",
        provider.clone(),
        executor.clone(),
        ToolActionKind::WorkspaceWrite,
    );

    let (result, logs) = capture_traces_for(
        "agent-loop-policy-denied",
        runtime.run_agent_loop(
            StepInput::user_text("Call denied tool.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        ),
    )
    .await;
    let result = result.expect("agent loop should complete after denied tool");

    assert_eq!(result.status(), &AgentLoopStatus::Completed);
    assert_eq!(
        event_kind_names(result.events()),
        [
            "SessionStarted",
            "StepStarted",
            "ToolCallPending",
            "ArtifactRecorded",
            "ToolCallResolved",
            "StepStarted",
            "AssistantOutputRecorded",
            "StepCompleted",
        ]
    );
    assert_eq!(executor.calls().len(), 0);
    let resolved = result
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool call should resolve");
    assert_eq!(resolved.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        resolved
            .diagnostic()
            .expect("policy denial should include diagnostic")
            .code(),
        "action_policy_denied"
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].continuations().len(), 1);
    assert_eq!(
        requests[1].continuations()[0].result().status(),
        ToolCallResultStatus::Failed
    );
    let content = requests[1].continuations()[0]
        .result()
        .content()
        .as_json()
        .expect("policy denial continuation should carry JSON content");
    let value: Value = serde_json::from_str(content).expect("denial JSON should parse");
    assert_sanitized_policy_denial_json(&value, "search_notes");
    assert_eq!(
        logs.matches("\"event\":\"runtime.tool.execute.finish\"")
            .count(),
        1
    );
    assert!(logs.contains("\"status\":\"denied\""));
    assert!(logs.contains("\"diagnostic_code\":\"action_policy_denied\""));
    assert!(!logs.contains("\"status\":\"failed\""));
}

#[tokio::test(flavor = "current_thread")]
async fn executor_infrastructure_error_preserves_events_and_pending_call() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-infra-error", "search_notes"),
    ))]]);
    let executor = ScriptedToolExecutor::infrastructure_error("temporary executor outage");
    let runtime = runtime_with_tool("agent-loop-infra-error", provider, executor);

    let (result, logs) = capture_traces_for(
        "agent-loop-infra-error",
        runtime.run_agent_loop(
            StepInput::user_text("Search notes.").expect("valid step input"),
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        ),
    )
    .await;
    let err = result.expect_err("infrastructure error should stop the loop as a method error");

    assert_eq!(
        event_kind_names(err.events()),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = pending_tool_call(err.events()).clone();
    assert!(matches!(
        err.runtime_error(),
        RuntimeError::ToolExecutionFailed { call_id, .. } if call_id == pending.id()
    ));
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert!(logs.contains("\"event\":\"runtime.loop.finish\""));
    assert!(logs.contains("\"status\":\"error\""));
    assert!(logs.contains("\"diagnostic_code\":\"tool_execution_failed\""));
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_stream_infrastructure_error_preserves_events_and_pending_call() {
    let provider = ScriptedModelProvider::new(vec![vec![Ok(completed_tool_call_event(
        model_tool_call("call-stream-infra-error", "search_notes"),
    ))]]);
    let executor = ScriptedToolExecutor::infrastructure_error("temporary stream executor outage");
    let runtime = runtime_with_tool("agent-loop-stream-infra-error", provider, executor);

    let (err, logs) = capture_traces_for("agent-loop-stream-infra-error", async {
        let mut stream = runtime
            .run_agent_loop_stream(
                StepInput::user_text("Search notes.").expect("valid step input"),
                StepContext::new(CancellationToken::new()),
                AgentLoopConfig::default(),
            )
            .expect("agent loop stream should start");
        while stream.next().await.is_some() {}
        stream
            .result()
            .await
            .expect_err("stream should preserve the executor infrastructure error")
    })
    .await;

    assert_eq!(
        event_kind_names(err.events()),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = pending_tool_call(err.events()).clone();
    assert!(matches!(
        err.runtime_error(),
        RuntimeError::ToolExecutionFailed {
            call_id,
            message,
            ..
        } if call_id == pending.id() && message == "temporary stream executor outage"
    ));
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert!(logs.contains("\"event\":\"runtime.loop.finish\""));
    assert!(logs.contains("\"status\":\"error\""));
    assert!(logs.contains("\"diagnostic_code\":\"tool_execution_failed\""));
}

#[tokio::test(flavor = "current_thread")]
async fn agent_loop_tool_execution_cancellation_returns_cancelled_and_keeps_pending() {
    let (started_tx, started_rx) = oneshot::channel();
    let (_release_tx, release_rx) = oneshot::channel();
    let provider = ScriptedModelProvider::new(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-loop-cancelled-tool",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("should not continue"))],
    ]);
    let executor = BlockingToolExecutor::new(started_tx, release_rx);
    let runtime = runtime_with_tool(
        "agent-loop-tool-cancellation",
        provider.clone(),
        executor.clone(),
    );
    let token = CancellationToken::new();
    let loop_runtime = runtime.clone();
    let loop_token = token.clone();
    let loop_handle = tokio::spawn(async move {
        loop_runtime
            .run_agent_loop(
                StepInput::user_text("Search notes.").expect("valid step input"),
                StepContext::new(loop_token),
                AgentLoopConfig::default(),
            )
            .await
            .expect("tool cancellation should return a loop status, not a method error")
    });

    started_rx
        .await
        .expect("executor should signal after tool execution starts");
    token.cancel();

    let result = loop_handle
        .await
        .expect("agent loop task should not panic after cancellation");

    assert!(matches!(
        result.status(),
        AgentLoopStatus::Cancelled {
            diagnostic
        } if diagnostic.code() == "tool_execution_cancelled"
            && diagnostic.message().contains("call-loop-cancelled-tool")
    ));
    assert_eq!(result.model_turns_run(), 1);
    assert_eq!(
        event_kind_names(result.events()),
        ["SessionStarted", "StepStarted", "ToolCallPending"]
    );
    let pending = pending_tool_call(result.events()).clone();
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert_eq!(executor.calls().len(), 1);
    assert_eq!(provider.recorded_requests().len(), 1);

    let evidence_err = runtime
        .evidence_ref(
            &artifact_id("tool-result-3"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("cancelled loop tool execution must not record a tool result artifact");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == artifact_id("tool-result-3")
    ));
    assert!(result.events().iter().all(|event| !matches!(
        event.payload,
        RuntimeJournalPayload::ToolCallResolved { .. }
    )));

    let artifact_events = runtime
        .record_artifact(
            ArtifactRef::new(artifact_id("post-cancel-artifact"), ArtifactKind::Text),
            merry_runtime::ArtifactContent::text("runtime permit released\n"),
        )
        .await
        .expect("runtime should release active permit after loop cancellation");
    assert_eq!(event_kind_names(&artifact_events), ["ArtifactRecorded"]);
}
