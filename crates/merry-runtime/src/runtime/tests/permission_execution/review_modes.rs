//! Permission review modes, trust levels, and host fallback.
//!
//! These tests cover who decides when the model does not: fail-closed
//! behaviour without a reviewer, host-decision and deny-all modes, and the
//! escalation path from a model denial or failure to the configured host
//! admission source.

use crate::{
    HostFallbackReason, PermissionAdmissionReviewSource, PermissionReviewMode, RuntimeModelRole,
    RuntimeTrustLevel,
    runtime::{
        RuntimeBuilder,
        tests::support::{
            common::{named_model, permission_review_completed_event},
            model_provider::{RecordingModelProvider, ScriptedModelProviderResponse},
            process::{FakeProcessRunner, StaticPermissionAdmissionSource},
            tool_helpers::{
                denied_action_content, register_permission_pending_tool_with_builder,
                resolved_tool_result,
            },
        },
    },
    tool::ToolExecutionContext,
};
use merry_core::ToolCallResultStatus;
use std::sync::Arc;

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

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_fully_trusted_mode_skips_review_sources() {
    let admission = StaticPermissionAdmissionSource::approving();
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-fully-trusted",
        "call-permission-fully-trusted",
        |builder| {
            builder
                .permission_review_mode(PermissionReviewMode::FullyTrusted)
                .permission_admission_source(Arc::new(admission.clone()))
                .allow_permissioned_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("fully trusted permission request should execute");

    assert_eq!(admission.call_count(), 0);
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_deny_all_mode_rejects_without_review_sources() {
    let admission = StaticPermissionAdmissionSource::approving();
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-deny-all",
        "call-permission-deny-all",
        |builder| {
            builder
                .permission_review_mode(PermissionReviewMode::DenyAll)
                .permission_admission_source(Arc::new(admission.clone()))
                .allow_permissioned_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("deny-all permission request should resolve the tool call");

    assert_eq!(admission.call_count(), 0);
    assert_eq!(runner.call_count(), 0);
    let result = resolved_tool_result(&events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("denied permission should include diagnostic")
            .code(),
        "permission_request_denied"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_model_failure_uses_opt_in_host_fallback() {
    let admission = StaticPermissionAdmissionSource::approving();
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-human-fallback",
        "call-permission-human-fallback",
        |builder| {
            builder
                .permission_review_mode(PermissionReviewMode::ModelThenHostFallback)
                .permission_admission_source(Arc::new(admission.clone()))
                .allow_permissioned_process_actions(Arc::new(runner.clone()))
                .build()
        },
    )
    .await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("configured fallback should resolve missing model review");

    assert_eq!(admission.call_count(), 1);
    assert_eq!(
        admission.fallback_reasons(),
        vec![Some(HostFallbackReason::ReviewModelUnavailable)],
        "the host should be told that no review model was configured"
    );
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_model_denial_escalates_to_host_fallback() {
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event("deny", "The action is not grounded in the task."),
        )])]);
    let admission = StaticPermissionAdmissionSource::approving();
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-denial-human-fallback",
        "call-permission-denial-human-fallback",
        |builder| {
            builder
                .permission_review_mode(PermissionReviewMode::ModelThenHostFallback)
                .permission_admission_source(Arc::new(admission.clone()))
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(review_provider),
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
        .expect("model denial should hand the decision to the host");

    assert_eq!(admission.call_count(), 1);
    let reasons = admission.fallback_reasons();
    let Some(Some(HostFallbackReason::ModelDenied(review))) = reasons.first() else {
        panic!("the host should see the model denial, got {reasons:?}");
    };
    assert_eq!(review.source(), PermissionAdmissionReviewSource::Model);
    assert_eq!(
        review.rationale(),
        "The action is not grounded in the task."
    );
    assert_eq!(
        reasons[0].as_ref().map(ToString::to_string).as_deref(),
        Some("AI review denied: The action is not grounded in the task.")
    );
    assert_eq!(runner.call_count(), 1);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn request_permissions_model_denial_is_final_without_host_fallback_mode() {
    let review_provider =
        RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![Ok(
            permission_review_completed_event("deny", "The action is not grounded in the task."),
        )])]);
    let admission = StaticPermissionAdmissionSource::approving();
    let runner = FakeProcessRunner::succeeding();
    let (runtime, pending) = register_permission_pending_tool_with_builder(
        "runtime-permission-denial-model-only",
        "call-permission-denial-model-only",
        |builder| {
            builder
                .permission_review_mode(PermissionReviewMode::Required)
                .permission_admission_source(Arc::new(admission.clone()))
                .model_provider_for_role(
                    RuntimeModelRole::ApprovalReview,
                    Arc::new(review_provider),
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
        .expect("model denial should resolve without host escalation");

    assert_eq!(admission.call_count(), 0);
    assert_eq!(runner.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events).status(),
        ToolCallResultStatus::Failed
    );
}
