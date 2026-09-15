//! Permission admission driven by a model reviewer.
//!
//! Review-mode, trust-level, and host-fallback coverage lives in
//! [`review_modes`]; these tests keep the model-review path itself: request
//! shape, the approval role, reviewer output handling, and negative grants.

mod review_modes;

use crate::{
    RuntimeModelRole,
    process::ProcessPermissionProfileId,
    request_permissions_tool,
    runtime::{
        Runtime,
        tests::support::{
            common::{
                RuntimeSessionStateTestExt, completed_event_with, named_model,
                permission_review_completed_event, session_id,
            },
            memory::record_prior_failed_tool_result,
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            process::{
                FakeProcessRunner, RecordingPermissionedProcessRunnerFactory,
                StaticPermissionAdmissionSource,
            },
            tool_helpers::{
                denied_action_content, event_kind_names_for_tool_execution,
                invalid_permission_pending_tool_call, path_permission_pending_tool_call,
                register_permission_pending_tool_with_builder, resolved_artifact_json,
                resolved_tool_result,
            },
        },
    },
    tool::ToolExecutionContext,
};
use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolCallResultStatus, ToolName};
use merry_llm::{FinishReason, ModelOutput};
use std::sync::Arc;

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
    let payload = resolved_artifact_json(&runtime, result, "invalid permission").await;
    assert_eq!(
        payload["guidance"]["kind"],
        "permission_request_invalid_arguments"
    );
    let guidance_message = payload["guidance"]["message"]
        .as_str()
        .expect("invalid permission request should include guidance message");
    assert!(guidance_message.contains("provide the exact command string and cwd"));
    assert!(!guidance_message.contains("set for_action"));
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
        ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
    );
    assert_eq!(runner_factory.call_count(), 1);
    assert_eq!(runner_factory.observed_network_requests(), [true]);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        runner.observed_intents()[0].argv(),
        ["bash", "-lc", "cargo test"]
    );
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
    let result = resolved_tool_result(&events);
    let payload = resolved_artifact_json(&runtime, result, "permissioned process").await;
    assert_eq!(payload["kind"], "process_action");
    assert_eq!(
        payload["permission_profile_id"],
        ProcessPermissionProfileId::APPROVED_PERMISSION_REQUEST.as_str()
    );
    assert_eq!(
        payload["permission_review"]["rationale"],
        "The user asked to run this command."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_review_accepts_reviewer_extra_fields() {
    // Reviewer models on smaller providers may echo prompt metadata such as
    // `reviewed_tool_call_id` or append their own commentary fields. The
    // review must still approve the exact action instead of degrading to the
    // host fallback.
    let reviewer_output = concat!(
        r#"{"schema_version":"permission_review.v1","decision":"approve","risk":"low","#,
        r#""user_authorization":"high","rationale":"The user asked to run this command.","#,
        r#""reviewed_tool_call_id":"call-permission-extra-fields","#,
        r#""reviewed_tool_name":"request_permissions","review_only":false}"#
    );
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            completed_event_with(vec![ModelOutput::text(reviewer_output)], FinishReason::Stop),
        )])]);
    let runner = FakeProcessRunner::succeeding();
    let runner_factory = RecordingPermissionedProcessRunnerFactory::new(Arc::new(runner.clone()));
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-review-extra-fields",
        "call-permission-extra-fields",
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
        .expect("reviewer extra fields should not block the approved action");

    assert_eq!(runner_factory.call_count(), 1);
    assert_eq!(runner.call_count(), 1);
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    let payload = resolved_artifact_json(&runtime, result, "permissioned process").await;
    assert_eq!(payload["permission_review"]["source"], "model");
    assert_eq!(
        payload["permission_review"]["rationale"],
        "The user asked to run this command."
    );
}

#[tokio::test(flavor = "current_thread")]
async fn run_process_inline_permissions_review_before_execution() {
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event("approve", "The command needs network access."),
        )])]);
    let runner = FakeProcessRunner::succeeding();
    let runner_factory = RecordingPermissionedProcessRunnerFactory::new(Arc::new(runner.clone()));
    let pending = PendingToolCall::new(
        ToolCallId::new("call-process-inline-permission").expect("valid call id"),
        ToolName::new("run_process").expect("valid tool name"),
        ToolCallArguments::try_from(serde_json::json!({
            "command": "cargo fetch",
            "cwd": ".",
            "reason": "The dependency fetch needs network access.",
            "permissions": { "network": true },
        }))
        .expect("valid process arguments"),
    );
    let runtime = Runtime::builder(session_id("runtime-process-inline-permission"))
        .register_tool(
            crate::process_command_tool(
                ToolName::new("run_process").expect("valid tool name"),
                "Run a process.",
            )
            .expect("process tool should build"),
        )
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(review_provider.clone()),
            named_model("fake/approval-review"),
        )
        .permissioned_process_runner_factory(Arc::new(runner_factory.clone()))
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
        .expect("inline process permission should execute after review");

    assert_eq!(review_provider.recorded_requests().len(), 1);
    assert_eq!(runner_factory.call_count(), 1);
    assert_eq!(runner_factory.observed_network_requests(), [true]);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        runner.observed_intents()[0].argv(),
        ["bash", "-lc", "cargo fetch"]
    );
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
    let payload = resolved_artifact_json(&runtime, result, "process").await;
    assert_eq!(
        payload["permission_profile_id"],
        ProcessPermissionProfileId::APPROVED_PERMISSION_REQUEST.as_str()
    );
    assert_eq!(payload["permission_review"]["source"], "model");
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_path_capability_uses_model_review_before_host_fallback() {
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event("approve", "The user asked for this temporary path."),
        )])]);
    let host_admission = StaticPermissionAdmissionSource::approving();
    let runner = FakeProcessRunner::succeeding();
    let runner_factory = RecordingPermissionedProcessRunnerFactory::new(Arc::new(runner.clone()));
    let pending =
        path_permission_pending_tool_call("call-permission-path-model-review", "/tmp", "rw");
    let runtime = Runtime::builder(session_id("runtime-permission-path-model-review"))
        .register_tool(request_permissions_tool().expect("permission tool builds"))
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(review_provider.clone()),
            named_model("fake/approval-review"),
        )
        .permission_admission_source(Arc::new(host_admission.clone()))
        .permissioned_process_runner_factory(Arc::new(runner_factory.clone()))
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
        .expect("path permission request should execute after model review");

    assert_eq!(review_provider.recorded_requests().len(), 1);
    assert_eq!(host_admission.call_count(), 0);
    assert_eq!(runner_factory.call_count(), 1);
    assert_eq!(runner.call_count(), 1);
    let review_requests = review_provider.recorded_requests();
    let user_prompt = review_requests[0].messages()[1].content().as_text();
    assert!(user_prompt.contains("\"path\":\"/tmp\""));
    assert!(user_prompt.contains("\"access\":\"rw\""));
    let result = resolved_tool_result(&events);
    let payload = resolved_artifact_json(&runtime, result, "path process").await;
    assert_eq!(payload["permission_review"]["source"], "model");
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_reuses_existing_path_grant_without_reopening_review() {
    let review_provider = RecordingModelProvider::with_script(Vec::new());
    let host_admission = StaticPermissionAdmissionSource::approving();
    let runner = FakeProcessRunner::succeeding();
    let runner_factory = RecordingPermissionedProcessRunnerFactory::new(Arc::new(runner.clone()))
        .with_capabilities_satisfied();
    let pending = path_permission_pending_tool_call(
        "call-permission-existing-path-grant",
        "/tmp/hello-work.txt",
        "rw",
    );
    let runtime = Runtime::builder(session_id("runtime-permission-existing-path-grant"))
        .register_tool(request_permissions_tool().expect("permission tool builds"))
        .model_provider_for_role(
            RuntimeModelRole::ApprovalReview,
            Arc::new(review_provider.clone()),
            named_model("fake/approval-review"),
        )
        .permission_admission_source(Arc::new(host_admission.clone()))
        .permissioned_process_runner_factory(Arc::new(runner_factory.clone()))
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
        .expect("existing path grant should execute without another review");

    assert!(review_provider.recorded_requests().is_empty());
    assert_eq!(host_admission.call_count(), 0);
    assert_eq!(runner_factory.call_count(), 1);
    assert_eq!(runner.call_count(), 1);
    let result = resolved_tool_result(&events);
    let payload = resolved_artifact_json(&runtime, result, "existing grant").await;
    assert_eq!(payload["permission_review"]["source"], "existing_grant");
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
    assert!(user_prompt.contains("\"command\":\"cargo test\""));
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
