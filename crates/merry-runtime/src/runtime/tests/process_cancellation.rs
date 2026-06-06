use super::*;

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_commits_output_after_runner_cancels_token() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::succeeding_then_cancelling_token();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_post_output_cancel"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-post-output-cancel",
        "policy_command_post_output_cancel",
        "call-command-exec-post-output-cancel",
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
        .expect("runner output should commit even if token is cancelled afterward");

    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
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
        payload
            .pointer("/stdout/text")
            .expect("process stdout text should be present"),
        "runtime tests passed after token cancellation\n"
    );

    let audits = action_audit_records(&runtime).await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
    assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
    let ActionExecutionEvidence::ProcessAction(evidence) = audits[1]
        .execution_evidence()
        .expect("executed audit should include process evidence")
    else {
        panic!("process action should record execution evidence");
    };
    assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
    assert_eq!(
        evidence.stdout_bytes(),
        "runtime tests passed after token cancellation\n".len()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_pre_cancel_keeps_pending_without_audit_or_result_artifact() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::succeeding();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_pre_cancel"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-pre-cancel",
        "policy_command_pre_cancel",
        "call-command-exec-pre-cancel",
        tool,
        |builder| {
            builder
                .allow_low_risk_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;
    let projection_before = runtime.ledger_projection().await;
    let token = CancellationToken::new();
    token.cancel();

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::new(token))
        .await
        .expect_err("pre-cancelled process action should not resolve");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. } if call_id == *pending.id()
    ));
    assert_eq!(executor.propose_count(), 0);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 0);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert_eq!(runtime.ledger_projection().await, projection_before);
    assert!(action_audit_records(&runtime).await.is_empty());
    let expected_result_artifact_id = artifact_id("tool-result-2");
    let evidence_err = runtime
        .evidence_ref(
            &expected_result_artifact_id,
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("pre-cancelled process action must not record result artifact");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == expected_result_artifact_id
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn opt_in_process_action_runner_cancel_keeps_pending_without_audit_or_result_artifact() {
    let executor = ProcessProposingToolExecutor::new();
    let runner = FakeProcessRunner::cancelling();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_command_runner_cancel"),
        Arc::new(executor.clone()),
        ToolActionKind::CommandExec,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        "runtime-policy-command-exec-runner-cancel",
        "policy_command_runner_cancel",
        "call-command-exec-runner-cancel",
        tool,
        |builder| {
            builder
                .allow_low_risk_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;
    let projection_before = runtime.ledger_projection().await;

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("runner-cancelled process action should not resolve");

    assert!(matches!(
        err,
        RuntimeError::ToolExecutionCancelled { call_id, .. } if call_id == *pending.id()
    ));
    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert_eq!(runtime.ledger_projection().await, projection_before);
    assert!(action_audit_records(&runtime).await.is_empty());
    let expected_result_artifact_id = artifact_id("tool-result-2");
    let evidence_err = runtime
        .evidence_ref(
            &expected_result_artifact_id,
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("runner-cancelled process action must not record result artifact");
    assert!(matches!(
        evidence_err,
        RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == expected_result_artifact_id
    ));
}
