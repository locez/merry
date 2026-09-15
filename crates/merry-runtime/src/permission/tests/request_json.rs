//! Request/JSON layout tests owned by `permission::request_json`.

use super::call;
use crate::permission::{permission_request_fingerprint_json, permission_request_from_call};
use serde_json::json;

#[test]
fn permission_request_fingerprint_json_carries_only_correlation_fields() {
    // `PermissionRequest::fingerprint` hashes this object and the derived
    // approval id correlates host responses, so the field set is a
    // compatibility contract rather than a display detail.
    let request = permission_request_from_call(
        &call(json!({
            "reason": "Need dependency metadata",
            "requested": { "network": true },
            "for_action": { "command": "cargo metadata", "cwd": "." }
        })),
        Vec::new(),
    )
    .expect("request should parse");

    let fingerprint = permission_request_fingerprint_json(&request);
    let mut keys = fingerprint
        .as_object()
        .expect("fingerprint input should be an object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "action",
            "reason",
            "requested",
            "review_only",
            "tool_call_id",
            "tool_name"
        ]
    );
}
