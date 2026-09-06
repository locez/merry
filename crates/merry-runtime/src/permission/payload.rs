use super::{
    PermissionAdmissionError, PermissionAdmissionReview, PermissionAdmissionReviewSource,
    PermissionRequest,
};
use crate::ToolExecutionOutcome;
use crate::permission::review::{permissioned_action_json, requested_capabilities_json};
use merry_core::{ErrorInfo, PendingToolCall};
use serde_json::{Value, json};

pub(crate) fn permission_denied_outcome(
    pending: &PendingToolCall,
    request: &PermissionRequest,
    review: Option<&PermissionAdmissionReview>,
) -> ToolExecutionOutcome {
    let payload = permission_resolution_payload(
        false,
        "denied",
        pending,
        Some(request),
        review,
        Some(permission_denied_guidance()),
    );
    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new(
            "permission_request_denied",
            "permission request was denied by admission review",
        )
        .expect("static diagnostic is valid"),
    )
}

pub(crate) fn permission_blocked_outcome(
    pending: &PendingToolCall,
    message: &str,
    request: Option<&PermissionRequest>,
) -> ToolExecutionOutcome {
    let mut payload = json!({
        "ok": false,
        "kind": "permission_request",
        "status": "blocked",
        "tool_call_id": pending.id().as_str(),
        "error": {
            "code": "permission_request_blocked",
            "message": message,
        },
        "guidance": {
            "kind": "permission_request_unavailable",
            "message": "Do not repeat the same permission request in this runtime. Permissioned execution is unavailable here, so report the blocked capability or choose an already-authorized approach.",
        }
    });
    if let Some(request) = request {
        payload["request"] = permission_request_summary(request);
    }
    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new("permission_request_blocked", message).expect("static diagnostic is valid"),
    )
}

pub(crate) fn permission_review_error_outcome(
    pending: &PendingToolCall,
    request: &PermissionRequest,
    error: &PermissionAdmissionError,
) -> ToolExecutionOutcome {
    let message = error.to_string();
    let payload = json!({
        "ok": false,
        "kind": "permission_request",
        "status": "review_failed",
        "tool_call_id": pending.id().as_str(),
        "error": {
            "code": "permission_review_failed",
            "message": message,
        },
        "request": permission_request_summary(request),
        "review": {
            "source": "model",
            "risk": "unknown",
            "user_authorization": "unknown",
            "rationale": message,
        },
        "guidance": {
            "kind": "permission_review_failed",
            "message": "Do not assume the requested capability was granted. If the action is still necessary, make one narrower permission request with the exact action and minimum capabilities; otherwise report the blocker.",
        },
        "retry": {
            "allowed": true,
            "message": "The approval review did not produce a decision. Try another plan or a narrower exact capability request; this request was not executed."
        }
    });
    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new("permission_review_failed", &message).expect("static diagnostic is valid"),
    )
}

pub(crate) fn permission_request_review_summary(review: &PermissionAdmissionReview) -> Value {
    json!({
        "source": review.source.as_str(),
        "risk": review.risk.as_str(),
        "user_authorization": review.user_authorization.as_str(),
        "rationale": review.rationale,
    })
}

impl PermissionAdmissionReviewSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Model => "model",
            Self::ExistingGrant => "existing_grant",
        }
    }
}

fn permission_resolution_payload(
    ok: bool,
    status: &str,
    pending: &PendingToolCall,
    request: Option<&PermissionRequest>,
    review: Option<&PermissionAdmissionReview>,
    guidance: Option<Value>,
) -> Value {
    let mut payload = json!({
        "ok": ok,
        "kind": "permission_request",
        "status": status,
        "tool_call_id": pending.id().as_str(),
    });
    if let Some(review) = review {
        payload["review"] = permission_request_review_summary(review);
    }
    if let Some(request) = request {
        payload["request"] = permission_request_summary(request);
    }
    if let Some(guidance) = guidance {
        payload["guidance"] = guidance;
    }
    payload
}

fn permission_denied_guidance() -> Value {
    json!({
        "kind": "permission_request_denied",
        "message": "Do not repeat the same permission request. Either continue with an already-authorized method, ask for a narrower exact capability only if it is genuinely required, or report that the requested action is blocked by policy. The current Plan remains in its existing phase; if it is executing, do not call update_plan with use_current_plan after this denial.",
    })
}

fn permission_request_summary(request: &PermissionRequest) -> Value {
    json!({
        "fingerprint": request.fingerprint(),
        "approval_id": request.approval_id(),
        "tool_call_id": request.tool_call_id().as_str(),
        "tool_name": request.tool_name().as_str(),
        "reason": request.reason(),
        "review_only": request.is_action_review(),
        "requested": requested_capabilities_json(request.requested()),
        "action": permissioned_action_json(request.action()),
    })
}

pub(crate) fn permission_invalid_arguments_outcome(
    tool_name: &str,
    error: PermissionAdmissionError,
) -> ToolExecutionOutcome {
    let message = error.to_string();
    let payload = json!({
        "ok": false,
        "tool": tool_name,
        "error": {
            "code": "permission_request_invalid_arguments",
            "message": message,
        },
        "guidance": {
            "kind": "permission_request_invalid_arguments",
            "message": "Fix the request_permissions arguments before retrying. Include requested and for_action, provide the exact command string and cwd, and request only minimum network/path/host-integration capability. An unmodeled Linux Unix socket may be requested as its exact filesystem path.",
        }
    });
    ToolExecutionOutcome::failed_json(
        payload.to_string(),
        ErrorInfo::new("permission_request_invalid_arguments", &message)
            .expect("static diagnostic is valid"),
    )
}
