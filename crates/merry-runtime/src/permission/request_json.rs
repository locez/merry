//! JSON rendering for a permission request and its parts.
//!
//! Reviewer prompts, request fingerprints, and execution-result payloads all
//! show the same request fields, so this module owns that one layout instead of
//! letting each caller re-derive it. It holds no admission policy, no reviewer
//! contract, and no provider types.

use super::{PermissionRequest, PermissionedAction, RequestedCapability};
use serde_json::{Value, json};

/// Renders the canonical capability object for one request.
///
/// Only requested classes appear, so a missing key means "not requested"
/// instead of "requested as false".
pub(crate) fn requested_capabilities_json(requested: &[RequestedCapability]) -> Value {
    let mut network = false;
    let mut paths = Vec::new();
    let mut integrations = Vec::new();
    for capability in requested {
        match capability {
            RequestedCapability::Network => network = true,
            RequestedCapability::Path(path) => {
                paths.push(json!({
                    "path": path.path(),
                    "access": path.access().as_str(),
                }));
            }
            RequestedCapability::HostIntegration(integration) => {
                integrations.push(integration.as_str());
            }
        }
    }
    let mut payload = json!({});
    if network {
        payload["network"] = json!(true);
    }
    if !paths.is_empty() {
        payload["paths"] = Value::Array(paths);
    }
    if !integrations.is_empty() {
        payload["host_integrations"] = json!(integrations);
    }
    payload
}

/// Renders the canonical planned-action object for one request.
pub(crate) fn permissioned_action_json(action: &PermissionedAction) -> Value {
    match action {
        PermissionedAction::Process(intent) => json!({
            "kind": "process",
            "command": crate::shell_command_for_argv(intent.argv()),
            "cwd": intent.cwd(),
            "summary": intent.summary(),
        }),
    }
}

/// Renders the request fields shared by fingerprints and result payloads.
pub(crate) fn permission_request_json(request: &PermissionRequest) -> Value {
    json!({
        "tool_call_id": request.tool_call_id().as_str(),
        "tool_name": request.tool_name().as_str(),
        "reason": request.reason(),
        "review_only": request.is_action_review(),
        "requested": requested_capabilities_json(request.requested()),
        "action": permissioned_action_json(request.action()),
    })
}

/// Renders the hashed input of [`PermissionRequest::fingerprint`].
///
/// The derived `approval_id` is what correlates a host response with a pending
/// request, so this field set is a compatibility contract rather than a display
/// detail: renaming, adding, or removing a field changes every approval id.
pub(crate) fn permission_request_fingerprint_json(request: &PermissionRequest) -> Value {
    permission_request_json(request)
}
