use super::*;

#[tokio::test(flavor = "current_thread")]
async fn cancelling_unregistered_tool_while_waiting_to_submit_keeps_pending() {
    let session_id =
        SessionId::new("runtime-unregistered-submit-cancel").expect("valid session id");
    let call_id = ToolCallId::new("call-unregistered").expect("valid tool call id");
    let pending = PendingToolCall::new(
        call_id.clone(),
        ToolName::new("missing_tool").expect("valid tool name"),
        ToolCallArguments::new(Default::default()),
    );
    let runtime = Runtime::builder(session_id)
        .build()
        .expect("runtime should build");

    let mut initial_session_guard = runtime.inner.session.lock().await;
    initial_session_guard
        .record_test_tool_call_pending(pending.clone())
        .expect("pending call should record");
    let projection_before = initial_session_guard.ledger_projection();

    let token = CancellationToken::new();
    let execute_runtime = runtime.clone();
    let execute_call_id = call_id.clone();
    let execute_token = token.clone();
    let execute_handle = tokio::spawn(async move {
        execute_runtime
            .execute_tool_call(&execute_call_id, ToolExecutionContext::new(execute_token))
            .await
    });
    tokio::task::yield_now().await;

    let (lock_acquired_sender, lock_acquired_receiver) = oneshot::channel();
    let (release_lock_sender, release_lock_receiver) = oneshot::channel();
    let blocker_runtime = runtime.clone();
    let blocker_handle = tokio::spawn(async move {
        let _session_guard = blocker_runtime.inner.session.lock().await;
        let _ = lock_acquired_sender.send(());
        let _ = release_lock_receiver.await;
    });
    tokio::task::yield_now().await;

    drop(initial_session_guard);
    lock_acquired_receiver
        .await
        .expect("blocker should acquire the session lock after pending lookup");
    tokio::task::yield_now().await;

    token.cancel();
    release_lock_sender
        .send(())
        .expect("blocker should still be waiting for release");

    let err = execute_handle
        .await
        .expect("tool execution task should not panic")
        .expect_err("cancelled unregistered tool execution should not resolve pending");
    blocker_handle
        .await
        .expect("session lock blocker should not panic");

    assert!(matches!(
        err,
        crate::RuntimeError::ToolExecutionCancelled { call_id: cancelled, .. }
            if cancelled == call_id
    ));
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert_eq!(runtime.ledger_projection().await, projection_before);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_registered_tool_after_success_before_submit_keeps_pending() {
    let session_id = SessionId::new("runtime-registered-submit-cancel").expect("valid session id");
    let call_id = ToolCallId::new("call-registered").expect("valid tool call id");
    let tool_spec = registered_tool_spec();
    let pending = PendingToolCall::new(
        call_id.clone(),
        tool_spec.name().clone(),
        ToolCallArguments::new(Default::default()),
    );
    let executor = SuccessfulToolExecutor::new();
    let runtime = Runtime::builder(session_id)
        .register_tool(RegisteredTool::read_only(
            tool_spec,
            Arc::new(executor.clone()),
        ))
        .build()
        .expect("runtime should build");

    let mut initial_session_guard = runtime.inner.session.lock().await;
    initial_session_guard
        .record_test_tool_call_pending(pending.clone())
        .expect("pending call should record");
    let projection_before = initial_session_guard.ledger_projection();

    let token = CancellationToken::new();
    let execute_runtime = runtime.clone();
    let execute_call_id = call_id.clone();
    let execute_token = token.clone();
    let execute_handle = tokio::spawn(async move {
        execute_runtime
            .execute_tool_call(&execute_call_id, ToolExecutionContext::new(execute_token))
            .await
    });
    tokio::task::yield_now().await;

    let (lock_acquired_sender, lock_acquired_receiver) = oneshot::channel();
    let (release_lock_sender, release_lock_receiver) = oneshot::channel();
    let blocker_runtime = runtime.clone();
    let blocker_handle = tokio::spawn(async move {
        let _session_guard = blocker_runtime.inner.session.lock().await;
        let _ = lock_acquired_sender.send(());
        let _ = release_lock_receiver.await;
    });
    tokio::task::yield_now().await;

    drop(initial_session_guard);
    lock_acquired_receiver
        .await
        .expect("blocker should acquire the session lock after pending lookup");
    tokio::task::yield_now().await;
    assert_eq!(executor.call_count(), 1);

    token.cancel();
    release_lock_sender
        .send(())
        .expect("blocker should still be waiting for release");

    let err = execute_handle
        .await
        .expect("tool execution task should not panic")
        .expect_err("late-cancelled registered tool execution should not resolve pending");
    blocker_handle
        .await
        .expect("session lock blocker should not panic");

    assert!(matches!(
        err,
        crate::RuntimeError::ToolExecutionCancelled { call_id: cancelled, .. }
            if cancelled == call_id
    ));
    assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
    assert_eq!(runtime.ledger_projection().await, projection_before);

    let expected_result_artifact_id = artifact_id("tool-result-1");
    let evidence_err = runtime
        .evidence_ref(
            &expected_result_artifact_id,
            EvidenceLocator::whole_artifact(),
        )
        .await
        .expect_err("cancelled tool execution must not record runtime-owned result artifact");
    assert!(matches!(
        evidence_err,
        crate::RuntimeError::Artifact {
            source: ArtifactError::MissingArtifact { id }
        } if id == expected_result_artifact_id
    ));
}
