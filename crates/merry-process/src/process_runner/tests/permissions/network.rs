//! The network ceiling: a request can widen one action, never the session.

use crate::process_runner::tests::{intent, permission_request, request_process_intent};
use crate::process_runner::{
    ProcessSessionPermissions, WorkspaceProcessRunner, WorkspaceProcessRunnerFactory,
};
use merry_runtime::PermissionedProcessRunnerFactory;
use serde_json::json;

#[test]
fn permissioned_factory_allows_network_only_when_requested() {
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry");
    let request_without_network = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/workspace/merry", "access": "rw" }]
        },
        "for_action": { "command": "cargo test", "cwd": "." }
    }));
    let request_with_network = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "cargo test", "cwd": "." }
    }));

    let without_network = factory
        .build_runner(&request_without_network)
        .plan_for(request_process_intent(&request_without_network))
        .expect("path-only launch");
    let with_network = factory
        .build_runner(&request_with_network)
        .plan_for(request_process_intent(&request_with_network))
        .expect("network launch");

    assert!(!without_network.network());
    assert!(with_network.network());
}

#[test]
fn permissioned_factory_keeps_network_scoped_to_the_current_action() {
    let session_permissions = ProcessSessionPermissions::new();
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_session_permissions(session_permissions.clone());
    let base_runner = WorkspaceProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_session_permissions(session_permissions);
    let network_request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "curl https://example.invalid", "cwd": null }
    }));

    let _ = factory.runner_for(&network_request);
    let approved = factory
        .backend()
        .build_runner(&network_request)
        .plan_for(request_process_intent(&network_request))
        .expect("approved network launch");
    assert!(approved.network());

    let later = base_runner.plan_for(&intent(None)).expect("later launch");
    assert!(!later.network());
}
