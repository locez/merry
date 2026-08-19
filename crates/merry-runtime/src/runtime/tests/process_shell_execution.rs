use super::*;

#[tokio::test(flavor = "current_thread")]
async fn read_only_shell_process_requires_explicit_shell_runner_opt_in() {
    let executor =
        ProcessProposingToolExecutor::with_argv(["bash", "-lc", "rg ProcessRunner | wc -l"]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_shell_read_only_without_shell_opt_in"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-shell-read-only-without-shell-opt-in",
        "policy_command_shell_read_only_without_shell_opt_in",
        "call-command-exec-shell-read-only-without-shell-opt-in",
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
        .expect("shell process proposal should be denied without shell opt-in");

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

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
    let policy = audits[1]
        .policy()
        .expect("denied audit should include shell read-only policy");
    assert_eq!(policy.risk_tier(), ActionRiskTier::ProcessShellReadOnly);
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_shell_process_executes_under_shell_profile_when_opted_in() {
    let executor =
        ProcessProposingToolExecutor::with_argv(["bash", "-lc", "rg ProcessRunner | wc -l"]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_shell_read_only_opt_in"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-shell-read-only-opt-in",
        "policy_command_shell_read_only_opt_in",
        "call-command-exec-shell-read-only-opt-in",
        tool,
        |builder| {
            builder
                .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("opted-in read-only shell process action should execute through shell runner");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    let RuntimeJournalPayload::ArtifactRecorded {
        artifact: input_artifact,
    } = &events[0].payload
    else {
        panic!("shell process input artifact should be recorded first");
    };
    assert_eq!(input_artifact.id().as_str(), "process-input-2");
    let input_content = runtime
        .read_artifact_content(input_artifact.id())
        .await
        .expect("shell process input artifact should be readable");
    let input_payload: serde_json::Value = serde_json::from_str(
        input_content
            .as_text()
            .expect("shell process input artifact should be textual JSON"),
    )
    .expect("shell process input artifact should parse as JSON");
    assert_eq!(
        input_payload,
        json!({
            "kind": "shell_command_input",
            "permission_profile_id": "process.shell.read_only",
            "tool_call_id": "call-command-exec-shell-read-only-opt-in",
            "tool_name": "policy_command_shell_read_only_opt_in",
            "intent": {
                "summary": "process argv[0]=bash; argc=3; cwd=.",
                "cwd": ".",
            },
            "input_evidence": {
                "kind": "shell_command_script",
                "shell": "bash",
                "flag": "-lc",
                "script": "rg ProcessRunner | wc -l",
                "script_bytes": "rg ProcessRunner | wc -l".len(),
                "script_fingerprint": stable_process_input_fingerprint(
                    "rg ProcessRunner | wc -l".as_bytes()
                ),
            },
        })
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);

    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("shell process result artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("shell process result artifact should be textual JSON"),
    )
    .expect("shell process result artifact should parse as JSON");
    assert_eq!(payload["permission_profile_id"], "process.shell.read_only");
    assert!(payload["intent"].get("argv").is_none());
    assert_eq!(
        payload["input_artifact"],
        json!({
            "id": input_artifact.id().as_str(),
            "kind": "json",
        })
    );
    assert!(payload.get("input_evidence").is_none());
    assert!(payload.get("provider").is_none());
    assert!(payload.get("wire").is_none());

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
    let policy = audits[1]
        .policy()
        .expect("executed audit should include shell allow policy");
    assert_eq!(policy.risk_tier(), ActionRiskTier::ProcessShellReadOnly);
    assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
    let ActionExecutionEvidence::ProcessAction(evidence) = audits[1]
        .execution_evidence()
        .expect("executed audit should include shell process evidence")
    else {
        panic!("shell process action should record process execution evidence");
    };
    assert_eq!(
        evidence.permission_profile_id(),
        ProcessPermissionProfileId::SHELL_READ_ONLY
    );
    let ActionProposalEvidence::ProcessAction(intent) = audits[0]
        .proposal()
        .expect("proposed audit should include shell process proposal")
        .evidence()
    else {
        panic!("proposed audit should record shell process intent");
    };
    assert_eq!(runner.observed_intents(), vec![intent.clone()]);
    assert!(evidence.matches_intent(intent));

    let projection = runtime.ledger_projection().await;
    let observation_text = projection
        .entries()
        .iter()
        .find_map(|entry| match entry {
            LedgerProjection::Fact { text, .. } if text.starts_with("shell process action ") => {
                Some(text.as_str())
            }
            LedgerProjection::Fact { .. } | LedgerProjection::Lifecycle { .. } => None,
        })
        .expect("shell process result should reduce into a compact ledger observation");
    assert!(observation_text.contains("permission_profile=process.shell.read_only"));
    assert!(observation_text.contains("shell=bash"));
    assert!(observation_text.contains("shell_flag=-lc"));
    assert!(observation_text.contains("shell_script_bytes=24"));
    assert!(observation_text.contains(&format!(
        "shell_script_fingerprint={}",
        stable_process_input_fingerprint("rg ProcessRunner | wc -l".as_bytes())
    )));
    assert!(observation_text.contains(&format!("artifact={}", result.artifact().id().as_str())));
    assert!(observation_text.contains("input_artifact=process-input-2"));
    assert!(!observation_text.contains("rg ProcessRunner | wc -l"));
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_shell_process_traces_payload_free_input_metadata_when_opted_in() {
    let script = "rg ProcessRunner | wc -l";
    let executor = ProcessProposingToolExecutor::with_argv(["bash", "-lc", script]);
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_shell_read_only_trace"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-shell-read-only-trace",
        "policy_command_shell_read_only_trace",
        "call-command-exec-shell-read-only-trace",
        tool,
        |builder| {
            builder
                .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let (events, logs) = capture_traces_for(
        "runtime-policy-command-exec-shell-read-only-trace",
        runtime.execute_tool_call(pending.id(), ToolExecutionContext::default()),
    )
    .await;
    let events = events.expect("opted-in read-only shell process action should execute");

    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(runner.call_count(), 1);
    assert!(logs.contains("\"event\":\"runtime.process.execute.start\""));
    assert!(logs.contains("\"event\":\"runtime.process.execute.finish\""));
    assert!(logs.contains("\"permission_profile_id\":\"process.shell.read_only\""));
    assert!(logs.contains("\"shell\":\"bash\""));
    assert!(logs.contains("\"shell_flag\":\"-lc\""));
    assert!(logs.contains("\"shell_script_bytes\":24"));
    assert!(logs.contains(&format!(
        "\"shell_script_fingerprint\":\"{}\"",
        stable_process_input_fingerprint(script.as_bytes())
    )));
    assert!(!logs.contains("\"argv\""));
    assert!(!logs.contains(script));
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_shell_process_denies_complex_or_mutating_shell_without_runner_call() {
    for (name, argv) in [
        ("redirect", ["bash", "-lc", "rg ProcessRunner > out.txt"]),
        ("substitution", ["bash", "-lc", "echo $(pwd)"]),
        (
            "mutating-segment",
            ["bash", "-lc", "rg ProcessRunner | rm -rf target"],
        ),
    ] {
        let executor = ProcessProposingToolExecutor::with_argv(argv);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec(&format!("policy_command_shell_{name}")),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            &format!("runtime-policy-command-exec-shell-{name}"),
            &format!("policy_command_shell_{name}"),
            &format!("call-command-exec-shell-{name}"),
            tool,
            |builder| {
                builder
                    .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("non-read-only shell process proposal should be denied durably");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Failed
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_shell_process_runner_cancel_keeps_input_artifact_before_unresolved_pending() {
    let script = "rg ProcessRunner | wc -l";
    let executor = ProcessProposingToolExecutor::with_argv(["bash", "-lc", script]);
    let runner = FakeProcessRunner::cancelling();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_shell_runner_cancel"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-shell-runner-cancel",
        "policy_command_shell_runner_cancel",
        "call-command-exec-shell-runner-cancel",
        tool,
        |builder| {
            builder
                .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("runner-cancelled shell process action should not resolve");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. } if call_id == *pending.id()
    ));
    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert!(action_audit_records(&runtime).await.is_empty());

    let input_content = runtime
        .read_artifact_content(&artifact_id("process-input-2"))
        .await
        .expect("shell input artifact should be durable before runner output");
    let input_payload: serde_json::Value = serde_json::from_str(
        input_content
            .as_text()
            .expect("shell input artifact should be textual JSON"),
    )
    .expect("shell input artifact should parse as JSON");
    assert_eq!(
        input_payload["input_evidence"]["script"],
        "rg ProcessRunner | wc -l"
    );
    assert_eq!(
        input_payload["input_evidence"]["script_fingerprint"],
        stable_process_input_fingerprint(script.as_bytes())
    );
    let input_evidence = runtime
        .evidence_ref(
            &artifact_id("process-input-2"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect("shell input artifact should have an exact evidence ref");
    assert_eq!(input_evidence.artifact_id, artifact_id("process-input-2"));
    assert!(
        lifecycle_kinds(&runtime.ledger_projection().await)
            .contains(&LedgerFactKind::ArtifactRecorded)
    );

    let expected_result_artifact_id = artifact_id("tool-result-3");
    let evidence_err = runtime
        .evidence_ref(
            &expected_result_artifact_id,
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("runner-cancelled shell action must not record result artifact");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == expected_result_artifact_id
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn read_only_shell_process_runner_failure_keeps_input_artifact_before_unresolved_pending() {
    let script = "rg ProcessRunner | wc -l";
    let executor = ProcessProposingToolExecutor::with_argv(["bash", "-lc", script]);
    let runner = FakeProcessRunner::infrastructure_failure("shell runner unavailable");
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_shell_runner_failure"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-shell-runner-failure",
        "policy_command_shell_runner_failure",
        "call-command-exec-shell-runner-failure",
        tool,
        |builder| {
            builder
                .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("infrastructure-failed shell process action should not resolve");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionFailed { call_id, message, .. }
            if call_id == *pending.id() && message == "shell runner unavailable"
    ));
    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert!(action_audit_records(&runtime).await.is_empty());

    let input_content = runtime
        .read_artifact_content(&artifact_id("process-input-2"))
        .await
        .expect("shell input artifact should be durable before runner failure");
    let input_payload: serde_json::Value = serde_json::from_str(
        input_content
            .as_text()
            .expect("shell input artifact should be textual JSON"),
    )
    .expect("shell input artifact should parse as JSON");
    assert_eq!(
        input_payload["input_evidence"]["script"],
        "rg ProcessRunner | wc -l"
    );
    assert_eq!(
        input_payload["input_evidence"]["script_fingerprint"],
        stable_process_input_fingerprint(script.as_bytes())
    );
    let input_evidence = runtime
        .evidence_ref(
            &artifact_id("process-input-2"),
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect("shell input artifact should have an exact evidence ref");
    assert_eq!(input_evidence.artifact_id, artifact_id("process-input-2"));
    assert!(
        lifecycle_kinds(&runtime.ledger_projection().await)
            .contains(&LedgerFactKind::ArtifactRecorded)
    );

    let expected_result_artifact_id = artifact_id("tool-result-3");
    let evidence_err = runtime
        .evidence_ref(
            &expected_result_artifact_id,
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("failed shell action must not record result artifact");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == expected_result_artifact_id
    ));
}
