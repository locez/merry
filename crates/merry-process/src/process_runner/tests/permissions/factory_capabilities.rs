//! How the non-sandbox factories answer requested capabilities: neither
//! silently ignores them nor invents a restriction the host mode does not have.

use crate::UnrestrictedPermissionedProcessRunnerFactory;
use crate::process_runner::tests::permission_request;
use crate::process_runner::{BwrapProcessRunner, TokioProcessRunner};
use merry_runtime::{
    PermissionedProcessRunnerFactory, ProcessRunner, StaticPermissionedProcessRunnerFactory,
};
use serde_json::json;
use std::sync::Arc;

#[test]
fn static_permissioned_factory_rejects_requested_path_capabilities() {
    let factory = StaticPermissionedProcessRunnerFactory::new(Arc::new(
        BwrapProcessRunner::new_at_workspace_root("/workspace/merry"),
    ));
    let request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "deps/cache", "access": "rw" }]
        },
        "for_action": { "command": "cargo test", "cwd": null }
    }));

    let error = factory
        .validate_request(&request)
        .expect_err("static runner must not silently ignore path capabilities");
    assert!(
        error
            .to_string()
            .contains("cannot enforce requested path capabilities")
    );
}

#[test]
fn unrestricted_permissioned_factory_accepts_host_capabilities() {
    let runner: Arc<dyn ProcessRunner> = Arc::new(TokioProcessRunner::new());
    let factory = UnrestrictedPermissionedProcessRunnerFactory::new(runner);
    let request = permission_request(json!({
        "requested": {
            "network": true,
            "paths": [{ "path": "/var/lib/merry-demo.txt", "access": "rw" }],
            "host_integrations": ["dbus"]
        },
        "for_action": { "command": "gh auth status", "cwd": null }
    }));

    factory
        .validate_request(&request)
        .expect("unrestricted host mode should not reject already-host-visible capabilities");
}
