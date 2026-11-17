//! Path capability admission: trusted rules, review materialization, session
//! retention, symlink aliases, and the read-only and deny ceilings.

use crate::process_runner::tests::{permission_request, request_process_intent};
use crate::process_runner::{
    ProcessSessionPermissions, WorkspaceProcessRunner, WorkspaceProcessRunnerFactory,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
};
use serde_json::json;
use std::path::PathBuf;

fn contains_path(paths: &[String], expected: &str) -> bool {
    paths.iter().any(|path| path == expected)
}

#[cfg(unix)]
#[test]
fn permissioned_factory_matches_rules_through_symlink_aliases() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary path");
    let real = temp.path().join("real");
    let link = temp.path().join("link");
    std::fs::create_dir_all(&real).expect("real directory");
    symlink(&real, &link).expect("directory symlink");

    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_path_rules([PathAccessRule::new(
            real.clone(),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    let request = permission_request(json!({
        "requested": {
            "paths": [{
                "path": link.to_str().expect("UTF-8 test path"),
                "access": "rw"
            }]
        },
        "for_action": { "command": "touch", "cwd": "." }
    }));

    assert!(
        factory
            .request_capabilities_are_satisfied(&request)
            .expect("path capability should be evaluated")
    );

    let denied_factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_path_rules([PathAccessRule::new(
            real,
            PathAccess::Deny,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    assert!(
        !denied_factory
            .request_capabilities_are_satisfied(&request)
            .expect("denied path capability should be evaluated")
    );
}

#[test]
fn permissioned_factory_preserves_trusted_path_rules() {
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_path_rules([PathAccessRule::new(
            PathBuf::from("/var/log"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    let request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "cargo test", "cwd": "." }
    }));

    let launch = factory
        .build_runner(&request)
        .plan_for(request_process_intent(&request))
        .expect("trusted path launch");

    assert!(contains_path(launch.read_only(), "/var/log"));
    assert!(launch.network());
}

#[test]
fn permissioned_factory_materializes_requested_path_rules() {
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry");
    let request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "deps/cache", "access": "rw" }]
        },
        "for_action": { "command": "cargo test", "cwd": "." }
    }));

    let launch = factory
        .build_runner(&request)
        .plan_for(request_process_intent(&request))
        .expect("requested path launch");

    assert!(contains_path(
        launch.read_write(),
        "/workspace/merry/deps/cache"
    ));
    assert!(!launch.network());
}

#[test]
fn permissioned_factory_keeps_approved_paths_for_later_actions() {
    let session_permissions = ProcessSessionPermissions::new();
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_session_permissions(session_permissions.clone());
    let base_runner = WorkspaceProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_session_permissions(session_permissions);
    let first_request = permission_request(json!({
        "requested": {
            "paths": [
                { "path": "/var/lib/merry-demo.txt", "access": "ro" }
            ]
        },
        "for_action": { "command": "touch /var/lib/merry-demo.txt", "cwd": null }
    }));
    let later_request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "cat /var/lib/merry-demo.txt", "cwd": null }
    }));

    let _ = factory.runner_for(&first_request);
    let launch = base_runner
        .plan_for(request_process_intent(&later_request))
        .expect("later ordinary process plan should build");

    assert!(contains_path(launch.read_only(), "/var/lib/merry-demo.txt"));
    assert!(!launch.network());
    assert!(contains_path(launch.read_write(), "/tmp"));
    assert!(contains_path(launch.read_write(), "/var/tmp"));
}

#[test]
fn permissioned_factory_reuses_parent_path_grants_for_descendants() {
    let session_permissions = ProcessSessionPermissions::new();
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_session_permissions(session_permissions);
    let parent_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/var/merry-parent", "access": "rw" }]
        },
        "for_action": { "command": "mkdir -p /var/merry-parent", "cwd": null }
    }));
    let descendant_read_write_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/var/merry-parent/hello.txt", "access": "rw" }]
        },
        "for_action": { "command": "cat /var/merry-parent/hello.txt", "cwd": null }
    }));
    let descendant_read_only_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/var/merry-parent/hello.txt", "access": "ro" }]
        },
        "for_action": { "command": "cat /var/merry-parent/hello.txt", "cwd": null }
    }));
    let network_request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "curl https://example.invalid", "cwd": null }
    }));

    let _ = factory.runner_for(&parent_request);
    assert!(
        factory
            .request_capabilities_are_satisfied(&descendant_read_write_request)
            .expect("descendant write capability query should succeed")
    );
    assert!(
        factory
            .request_capabilities_are_satisfied(&descendant_read_only_request)
            .expect("descendant read capability query should succeed")
    );
    assert!(
        !factory
            .request_capabilities_are_satisfied(&network_request)
            .expect("network capability query should succeed")
    );

    let read_only_factory =
        WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
            .with_session_permissions(ProcessSessionPermissions::new());
    let read_only_parent_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/var/merry-parent", "access": "ro" }]
        },
        "for_action": { "command": "cat /var/merry-parent/hello.txt", "cwd": null }
    }));

    let _ = read_only_factory.runner_for(&read_only_parent_request);
    assert!(
        read_only_factory
            .request_capabilities_are_satisfied(&descendant_read_only_request)
            .expect("read-only descendant capability query should succeed")
    );
    assert!(
        !read_only_factory
            .request_capabilities_are_satisfied(&descendant_read_write_request)
            .expect("read-write upgrade capability query should succeed")
    );
}

#[test]
fn permissioned_factory_caps_requested_write_to_trusted_read_only_rule() {
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_path_rules([PathAccessRule::new(
            PathBuf::from("/opt/merry-readonly"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    let request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/opt/merry-readonly", "access": "rw" }]
        },
        "for_action": { "command": "cargo test", "cwd": "." }
    }));

    factory
        .validate_request(&request)
        .expect("read-only policy should cap rather than reject a write request");
    let launch = factory
        .build_runner(&request)
        .plan_for(request_process_intent(&request))
        .expect("capped path launch");

    assert!(contains_path(launch.read_only(), "/opt/merry-readonly"));
    assert!(!contains_path(launch.read_write(), "/opt/merry-readonly"));
}

#[test]
fn permissioned_factory_rejects_requested_path_under_configured_deny() {
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_path_rules([PathAccessRule::new(
            PathBuf::from("/workspace/merry/secrets"),
            PathAccess::Deny,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    let request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "secrets/token", "access": "ro" }]
        },
        "for_action": { "command": "cat secrets/token", "cwd": null }
    }));

    let error = factory
        .validate_request(&request)
        .expect_err("configured deny must be a hard policy boundary");
    assert!(
        error
            .to_string()
            .contains("denied by configured path policy")
    );
}
