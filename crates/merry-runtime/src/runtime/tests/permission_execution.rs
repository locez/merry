use super::*;

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_invalid_arguments_skip_review_and_runner() {
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event(
                "approve",
                "Invalid arguments should not reach review.",
            ),
        )])]);
    let runner = FakeProcessRunner::succeeding();
    let pending = invalid_permission_pending_tool_call("call-permission-invalid-arguments");
    let runtime = Runtime::builder(session_id("runtime-permission-invalid-arguments"))
        .register_tool(request_permissions_tool().expect("permission tool builds"))
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(review_provider.clone()),
            named_model("fake/approval-review"),
        )
        .allow_permissioned_process_actions(Arc::new(runner.clone()))
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
        .expect("invalid permission request should resolve failed tool result");

    assert_eq!(runner.call_count(), 0);
    assert!(
        review_provider.recorded_requests().is_empty(),
        "invalid permission arguments must not invoke review"
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Failed
    );
    let result = resolved_tool_result(&events);
    assert_eq!(
        result
            .diagnostic()
            .expect("invalid permission request should include diagnostic")
            .code(),
        "permission_request_invalid_arguments"
    );
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("invalid permission artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("invalid permission result should be textual JSON"),
    )
    .expect("invalid permission artifact should parse as JSON");
    assert_eq!(
        payload["guidance"]["kind"],
        "permission_request_invalid_arguments"
    );
    let guidance_message = payload["guidance"]["message"]
        .as_str()
        .expect("invalid permission request should include guidance message");
    assert!(guidance_message.contains("for_action.kind"));
    assert!(!guidance_message.contains("for_action.payload"));
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_approved_by_review_executes_exact_process_action() {
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event("approve", "The user asked to run this command."),
        )])]);
    let runner = FakeProcessRunner::succeeding();
    let runner_factory = RecordingPermissionedProcessRunnerFactory::new(Arc::new(runner.clone()));
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-approved-process",
        "call-permission-approved-process",
        |builder| {
            builder
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(review_provider.clone()),
                    named_model("fake/approval-review"),
                )
                .permissioned_process_runner_factory(Arc::new(runner_factory.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("approved permission request should execute exact action");

    assert_eq!(
        event_kind_names_for_tool_execution(&events),
        ["ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(runner_factory.call_count(), 1);
    assert_eq!(runner_factory.observed_network_requests(), [true]);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(runner.observed_intents()[0].argv(), ["cargo", "test"]);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
    let result = resolved_tool_result(&events);
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("process artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("permissioned process result should be JSON"),
    )
    .expect("process artifact should parse as JSON");
    assert_eq!(payload["kind"], "process_action");
    assert_eq!(
        payload["permission_profile_id"],
        ProcessPermissionProfileId::APPROVED_PERMISSION_REQUEST_V1.as_str()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_denied_by_review_does_not_execute_process() {
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event(
                "deny",
                "The requested network access is not authorized.",
            ),
        )])]);
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-denied-process",
        "call-permission-denied-process",
        |builder| {
            builder
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(review_provider.clone()),
                    named_model("fake/approval-review"),
                )
                .allow_permissioned_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("denied permission request should resolve the tool call");

    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Failed
    );
    let result = resolved_tool_result(&events);
    assert_eq!(
        result
            .diagnostic()
            .expect("denied permission should include diagnostic")
            .code(),
        "permission_request_denied"
    );
    let payload = denied_action_content(&runtime, &events).await;
    assert_eq!(payload["guidance"]["kind"], "permission_request_denied");
    assert!(
        payload["guidance"]["message"]
            .as_str()
            .expect("denial guidance should be text")
            .contains("use_current_plan")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_review_uses_approval_role_and_runtime_context() {
    let primary_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event("approve", "Primary should not be used."),
        )])]);
    let approval_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event("deny", "Review saw no sufficient authorization."),
        )])]);
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-review-role-context",
        "call-permission-review-role-context",
        |builder| {
            builder
                .model_provider(
                    Arc::new(primary_provider.clone()),
                    named_model("fake/primary"),
                )
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(approval_provider.clone()),
                    named_model("fake/approval-review"),
                )
                .allow_permissioned_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;
    record_prior_failed_tool_result(
        &runtime,
        r#"{"ok":false,"stderr":{"text":"Could not resolve host: crates.io"}}"#,
    );

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("review denial should resolve the tool call");

    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Failed
    );
    assert!(
        primary_provider.recorded_requests().is_empty(),
        "approval role should be preferred over primary when configured"
    );
    let approval_requests = approval_provider.recorded_requests();
    assert_eq!(approval_requests.len(), 1);
    assert_eq!(
        approval_requests[0].model().as_str(),
        "fake/approval-review"
    );
    let user_prompt = approval_requests[0].messages()[1].content().as_text();
    assert!(user_prompt.contains(">>> RECENT RUNTIME CONTEXT START"));
    assert!(user_prompt.contains("Please run cargo test"));
    assert!(user_prompt.contains("Could not resolve host: crates.io"));
    assert!(user_prompt.contains("\"network\":true"));
    assert!(user_prompt.contains("\"argv\":[\"cargo\",\"test\"]"));
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_review_falls_back_to_primary_model() {
    let primary_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event(
                "approve",
                "Primary review approved because no approval role is configured.",
            ),
        )])]);
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-primary-review-fallback",
        "call-permission-primary-review-fallback",
        |builder| {
            builder
                .model_provider(
                    Arc::new(primary_provider.clone()),
                    named_model("fake/primary"),
                )
                .allow_permissioned_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("primary fallback review should approve execution");

    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
    let requests = primary_provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model().as_str(), "fake/primary");
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_default_agent_without_review_model_fails_closed() {
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-no-review-model",
        "call-permission-no-review-model",
        |builder| {
            builder
                .allow_permissioned_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("missing review model should durably resolve failed permission request");

    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Failed
    );
    let result = resolved_tool_result(&events);
    assert_eq!(
        result
            .diagnostic()
            .expect("blocked permission should include diagnostic")
            .code(),
        "permission_review_failed"
    );
    let payload = denied_action_content(&runtime, &events).await;
    assert_eq!(payload["guidance"]["kind"], "permission_review_failed");
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_without_permissioned_runner_guides_model_to_stop_retrying() {
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-no-runner",
        "call-permission-no-runner",
        RuntimeBuilder::build,
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("missing permissioned runner should resolve failed permission request");

    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("blocked permission should include diagnostic")
            .code(),
        "permission_request_blocked"
    );
    let payload = denied_action_content(&runtime, &events).await;
    assert_eq!(
        payload["guidance"]["kind"],
        "permission_request_unavailable"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_trusted_sdk_host_decision_can_skip_model_review() {
    let admission = StaticPermissionAdmissionSource::approving();
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-trusted-host-decision",
        "call-permission-trusted-host-decision",
        |builder| {
            builder
                .runtime_trust_level(RuntimeTrustLevel::TrustedSdk)
                .permission_review_mode(PermissionReviewMode::HostDecisionOnly)
                .permission_admission_source(Arc::new(admission.clone()))
                .allow_permissioned_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("trusted host admission should execute exact action");

    assert_eq!(admission.call_count(), 1);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}
