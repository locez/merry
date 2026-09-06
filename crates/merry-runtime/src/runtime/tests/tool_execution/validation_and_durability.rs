use crate::{
    ArtifactError, FileSessionStore, RuntimeError,
    runtime::{
        Runtime,
        tests::support::{
            common::{RuntimeSessionStateTestExt, artifact_id, session_id},
            tool_executors::{
                CancelDuringRuntimeControlExecutor, ProposingToolExecutor, SuccessfulToolExecutor,
            },
            tool_helpers::{
                action_audit_records, event_kind_names_for_tool_execution, policy_tool_spec,
                register_policy_pending_registered_tool,
                register_policy_pending_registered_tool_with_builder, register_policy_pending_tool,
                required_query_tool_spec, resolved_tool_result,
            },
        },
    },
    session_store::SessionStoreCommitPause,
    tool::{RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionOutcome},
};
use merry_core::{
    EvidenceLocator, PendingToolCall, RuntimeJournalPayload, ToolCallArguments, ToolCallId,
    ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec, TrajectoryRecordStatus,
};
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn tool_execution_persists_trajectory_before_returning_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pause = SessionStoreCommitPause::new();
    let store = FileSessionStore::new(temp.path()).with_commit_pause_for_tests(pause.clone());
    let session = "runtime-tool-trajectory-savepoint";
    let resume_store = store.clone();
    let executor = SuccessfulToolExecutor::new();
    let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
        session,
        "trajectory_tool",
        "trajectory-tool-call",
        RegisteredTool::read_only(policy_tool_spec("trajectory_tool"), Arc::new(executor)),
        |builder| builder.session_store(store).build(),
    )
    .await;
    let call_id = pending.id().clone();
    let task_runtime = runtime.clone();
    let task = tokio::spawn(async move {
        task_runtime
            .execute_tool_call(&call_id, ToolExecutionContext::default())
            .await
    });

    pause.wait_until_committed().await;
    assert!(!task.is_finished());
    pause.resume();
    let events = task
        .await
        .expect("tool execution task joins")
        .expect("tool execution succeeds");
    let resolved_sequence = events
        .iter()
        .find(|event| {
            matches!(
                event.payload,
                RuntimeJournalPayload::ToolCallResolved { .. }
            )
        })
        .expect("tool call resolves")
        .sequence;

    let resumed = Runtime::builder(session_id(session))
        .resume_from_store(resume_store)
        .await
        .expect("runtime resumes after tool savepoint");
    let snapshot = resumed
        .trajectory_snapshot()
        .await
        .expect("trajectory snapshot reads");
    assert_eq!(snapshot.latest_sequence(), resolved_sequence);
    let tool_record = snapshot
        .records()
        .iter()
        .find(|record| record.tool_call_id().is_some())
        .expect("tool trajectory record resumes");
    assert_eq!(tool_record.status(), TrajectoryRecordStatus::Succeeded);
    assert_eq!(tool_record.end_sequence(), Some(resolved_sequence));
}

#[tokio::test(flavor = "current_thread")]
async fn registered_tool_arguments_are_validated_before_execution() {
    let executor = SuccessfulToolExecutor::new();
    let pending = PendingToolCall::new(
        ToolCallId::new("call-invalid-schema").expect("valid tool call id"),
        ToolName::new("validated_tool").expect("valid tool name"),
        ToolCallArguments::new(Default::default()),
    );
    let runtime = Runtime::builder(session_id("runtime-tool-input-schema-invalid"))
        .register_tool(RegisteredTool::read_only(
            required_query_tool_spec("validated_tool"),
            Arc::new(executor.clone()),
        ))
        .build()
        .expect("runtime should build");
    {
        let mut session = runtime.inner.session.lock().await;
        session.record_session_started_if_needed();
        session
            .record_test_tool_call_pending(pending.clone())
            .expect("pending call should record");
    }

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("invalid tool arguments should resolve as a failed tool result");

    assert_eq!(executor.call_count(), 0);
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("schema failure should carry diagnostic")
            .code(),
        "tool_input_schema_invalid"
    );
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("schema failure artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("schema failure artifact should be textual JSON"),
    )
    .expect("schema failure artifact should parse as JSON");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["tool"], "validated_tool");
    assert_eq!(payload["error"]["code"], "tool_input_schema_invalid");
    assert!(
        payload["error"]["violations"]
            .as_array()
            .expect("schema failure should list violations")
            .iter()
            .any(|violation| violation["message"]
                .as_str()
                .is_some_and(|message| message.contains("query")))
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[test]
fn runtime_builder_precompiles_registered_tool_input_schema() {
    let schema = Schema::try_from(json!({ "type": "not-a-json-schema-type" }))
        .expect("test schema should be a JSON object");
    let spec = ToolSpec::new(
        ToolName::new("invalid_schema_tool").expect("valid tool name"),
        "Invalid schema test tool",
        ToolInputSchema::new(schema).expect("tool input schema only checks object shape"),
    )
    .expect("tool spec should build before runtime schema compilation");

    let result = Runtime::builder(session_id("runtime-tool-input-schema-precompile"))
        .register_tool(RegisteredTool::read_only(
            spec,
            Arc::new(SuccessfulToolExecutor::new()),
        ))
        .build();
    let error = match result {
        Ok(_) => panic!("runtime should reject invalid registered tool schema"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        RuntimeError::InvalidToolInputSchema { ref name, .. }
            if name.as_str() == "invalid_schema_tool"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_control_tool_resolves_even_if_token_is_cancelled_during_execution() {
    let executor = CancelDuringRuntimeControlExecutor::new();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-runtime-control-cancel-race",
        "policy_control",
        "call-runtime-control",
        ToolActionKind::RuntimeControl,
        executor.clone(),
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("runtime control outcome should survive in-flight cancellation");

    assert_eq!(executor.call_count(), 1);
    assert!(executor.token_seen().is_cancelled());
    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn preflight_outcome_must_be_failed_to_resolve_without_policy_bypass() {
    let executor = ProposingToolExecutor::with_preflight_outcome(
        ToolExecutionOutcome::succeeded_text("must not bypass policy\n"),
    );
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_write_preflight_success"),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool(
        "runtime-policy-preflight-success-rejected",
        "policy_write_preflight_success",
        "call-preflight-success-rejected",
        tool,
    )
    .await;

    let error = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect_err("successful preflight outcome must not bypass action policy");

    match error {
        RuntimeError::Core { source } => assert!(
            source.to_string().contains("preflight tool outcome"),
            "unexpected core error: {source}"
        ),
        other => panic!("expected core validation error, got {other:?}"),
    }
    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(
        runtime
            .pending_tool_calls()
            .await
            .iter()
            .map(|call| call.id().as_str().to_owned())
            .collect::<Vec<_>>(),
        vec![pending.id().as_str().to_owned()]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pre_cancelled_denied_tool_execution_keeps_pending_without_artifact() {
    let executor = SuccessfulToolExecutor::new();
    let (runtime, pending) = register_policy_pending_tool(
        "runtime-policy-pre-cancel",
        "policy_pre_cancel",
        "call-policy-pre-cancel",
        ToolActionKind::WorkspaceWrite,
        executor.clone(),
    )
    .await;
    let projection_before = runtime.ledger_projection().await;
    let token = CancellationToken::new();
    token.cancel();

    let err = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::new(token))
        .await
        .expect_err("pre-cancelled denied tool should not resolve");

    assert!(matches!(
        err,
        crate::RuntimeError::ToolExecutionCancelled { call_id, .. }
            if call_id == *pending.id()
    ));
    assert_eq!(executor.call_count(), 0);
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
        .expect_err("pre-cancelled policy denial must not record result artifact");
    assert!(matches!(
        evidence_err,
        crate::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == expected_result_artifact_id
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_before_proposal_commit_keeps_pending_without_audit_or_result_artifact() {
    let executor = ProposingToolExecutor::cancelling();
    let tool = RegisteredTool::new(
        policy_tool_spec("policy_proposal_cancel"),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let (runtime, pending) = register_policy_pending_registered_tool(
        "runtime-policy-proposal-cancel",
        "policy_proposal_cancel",
        "call-policy-proposal-cancel",
        tool,
    )
    .await;
    let projection_before = runtime.ledger_projection().await;
    let token = CancellationToken::new();
    let execute_runtime = runtime.clone();
    let execute_call_id = pending.id().clone();
    let execute_token = token.clone();

    let handle = tokio::spawn(async move {
        execute_runtime
            .execute_tool_call(&execute_call_id, ToolExecutionContext::new(execute_token))
            .await
    });
    tokio::task::yield_now().await;
    assert_eq!(executor.propose_count(), 1);

    token.cancel();
    let err = handle
        .await
        .expect("proposal cancellation task should not panic")
        .expect_err("cancelled proposal should not resolve");

    assert!(matches!(
        err,
        crate::RuntimeError::ToolExecutionCancelled { call_id, .. }
            if call_id == *pending.id()
    ));
    assert_eq!(executor.execute_count(), 0);
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
        .expect_err("cancelled proposal must not record result artifact");
    assert!(matches!(
        evidence_err,
        crate::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == expected_result_artifact_id
    ));
}
