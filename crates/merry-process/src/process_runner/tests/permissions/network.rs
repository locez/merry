//! The network ceiling: a request can widen one action, never the session.

use crate::process_runner::tests::{intent, os_args, permission_request, request_process_intent};
use crate::process_runner::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, BwrapSessionPermissions,
    bwrap_process_plan,
};
use merry_runtime::PermissionedProcessRunnerFactory;
use serde_json::json;

#[test]
fn bwrap_permissioned_factory_allows_network_only_when_requested() {
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap");
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

    let runner_without_network = factory.build_runner(&request_without_network);
    let plan_without_network = bwrap_process_plan(
        request_process_intent(&request_without_network),
        &runner_without_network.cwd_root,
        runner_without_network.network_allowed,
        &runner_without_network.path_rules,
        &runner_without_network.bwrap_program,
    );
    let runner_with_network = factory.build_runner(&request_with_network);
    let plan_with_network = bwrap_process_plan(
        request_process_intent(&request_with_network),
        &runner_with_network.cwd_root,
        runner_with_network.network_allowed,
        &runner_with_network.path_rules,
        &runner_with_network.bwrap_program,
    );

    assert!(os_args(&plan_without_network.args).contains(&"--unshare-net".to_owned()));
    assert!(!os_args(&plan_with_network.args).contains(&"--unshare-net".to_owned()));
}

#[test]
fn bwrap_permissioned_factory_keeps_network_scoped_to_current_action() {
    let session_permissions = BwrapSessionPermissions::new();
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_session_permissions(session_permissions.clone());
    let base_runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_session_permissions(session_permissions);
    let network_request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "curl https://example.invalid", "cwd": null }
    }));

    let _ = factory.runner_for(&network_request);
    let approved_runner = factory.backend().build_runner(&network_request);
    let approved_plan = approved_runner
        .plan_for(request_process_intent(&network_request))
        .expect("approved network process plan should build");
    let approved_args = os_args(&approved_plan.args);
    assert!(!approved_args.iter().any(|arg| arg == "--unshare-net"));

    let plan = base_runner
        .plan_for(&intent(None))
        .expect("later process plan should build");
    let args = os_args(&plan.args);

    assert!(args.iter().any(|arg| arg == "--unshare-net"));
}
