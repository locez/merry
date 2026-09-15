//! Host review transport tests owned by `permission::channel`.

use super::call;
use crate::permission::{
    ChannelPermissionAdmissionSource, HostFallbackReason, PermissionAdmissionContext,
    PermissionAdmissionError, PermissionAdmissionSource, PermissionReviewResponse,
    permission_request_from_call,
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn channel_human_review_waits_for_a_correlated_typed_response() {
    let (source, mut requests) = ChannelPermissionAdmissionSource::channel(1);
    let source = Arc::new(source);
    let request = permission_request_from_call(
        &call(json!({
            "requested": { "network": true },
            "for_action": { "command": "cargo test", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("request should parse");
    let approval_id = request.approval_id();
    let fingerprint = request.fingerprint();
    let token = CancellationToken::new();
    let source_for_task = Arc::clone(&source);
    let task = tokio::spawn(async move {
        source_for_task
            .review(
                request,
                PermissionAdmissionContext::new(token).with_host_fallback_reason(
                    HostFallbackReason::ReviewFailed {
                        message: "approval provider was unavailable".to_owned(),
                    },
                ),
            )
            .await
    });

    let pending = requests
        .recv()
        .await
        .expect("host should receive review request");
    assert_eq!(pending.approval_id(), approval_id);
    assert_eq!(pending.fingerprint(), fingerprint);
    assert_eq!(
        pending.host_fallback_reason(),
        Some(&HostFallbackReason::ReviewFailed {
            message: "approval provider was unavailable".to_owned(),
        })
    );
    assert_eq!(
        pending
            .host_fallback_reason()
            .map(ToString::to_string)
            .as_deref(),
        Some("AI review unavailable: approval provider was unavailable")
    );
    pending
        .respond(PermissionReviewResponse::allow(
            approval_id,
            fingerprint,
            "Host confirmed the exact command.",
        ))
        .expect("typed response should be delivered");

    let decision = task
        .await
        .expect("review task should join")
        .expect("review should resolve");
    assert!(decision.is_approved());
    assert_eq!(
        decision.review().rationale(),
        "Host confirmed the exact command."
    );
}

#[tokio::test]
async fn channel_human_review_rejects_stale_response_identity() {
    let (source, mut requests) = ChannelPermissionAdmissionSource::channel(1);
    let source = Arc::new(source);
    let request = permission_request_from_call(
        &call(json!({
            "requested": { "network": true },
            "for_action": { "command": "cargo test", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("request should parse");
    let token = CancellationToken::new();
    let source_for_task = Arc::clone(&source);
    let task = tokio::spawn(async move {
        source_for_task
            .review(request, PermissionAdmissionContext::new(token))
            .await
    });
    let pending = requests
        .recv()
        .await
        .expect("host should receive review request");
    pending
        .respond(PermissionReviewResponse::allow(
            "stale-approval",
            "stale-fingerprint",
            "This must not grant the request.",
        ))
        .expect("stale response should still reach runtime validation");

    let error = task
        .await
        .expect("review task should join")
        .expect_err("stale response must be rejected");
    assert!(matches!(
        error,
        PermissionAdmissionError::StaleReviewResponse { .. }
    ));
}

#[tokio::test]
async fn channel_human_review_marks_queued_request_cancelled() {
    let (source, mut requests) = ChannelPermissionAdmissionSource::channel(1);
    let source = Arc::new(source);
    let request = permission_request_from_call(
        &call(json!({
            "requested": { "network": true },
            "for_action": { "command": "cargo test", "cwd": null }
        })),
        Vec::new(),
    )
    .expect("request should parse");
    let token = CancellationToken::new();
    let task_token = token.clone();
    let source_for_task = Arc::clone(&source);
    let task = tokio::spawn(async move {
        source_for_task
            .review(request, PermissionAdmissionContext::new(task_token))
            .await
    });
    let pending = requests
        .recv()
        .await
        .expect("host should receive review request");
    assert!(!pending.is_cancelled());
    token.cancel();
    assert!(pending.is_cancelled());

    let error = task
        .await
        .expect("review task should join")
        .expect_err("cancelled review must not remain pending");
    assert!(matches!(error, PermissionAdmissionError::Cancelled));
}
