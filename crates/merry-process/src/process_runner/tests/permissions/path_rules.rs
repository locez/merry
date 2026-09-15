//! Path capability admission: trusted rules, review materialization, session
//! retention, symlink aliases, and the read-only and deny ceilings.

use crate::process_runner::tests::{
    contains_sequence, os_args, permission_request, request_process_intent,
};
use crate::process_runner::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, BwrapSessionPermissions,
    bwrap_process_plan,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
};
use serde_json::json;
use std::path::PathBuf;

#[cfg(unix)]
#[test]
fn bwrap_permissioned_factory_matches_rules_through_symlink_aliases() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary path");
    let real = temp.path().join("real");
    let link = temp.path().join("link");
    std::fs::create_dir_all(&real).expect("real directory");
    symlink(&real, &link).expect("directory symlink");

    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
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

    let denied_factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
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
fn bwrap_permissioned_factory_preserves_trusted_path_rules() {
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_path_rules([PathAccessRule::new(
            PathBuf::from("/var/log"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    let request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "cargo test", "cwd": "." }
    }));

    let runner = factory.build_runner(&request);
    let plan = bwrap_process_plan(
        request_process_intent(&request),
        &runner.cwd_root,
        runner.network_allowed,
        &runner.path_rules,
        &runner.bwrap_program,
    );
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &["--ro-bind-try", "/var/log", "/var/log"]
    ));
}

#[test]
fn bwrap_permissioned_factory_materializes_requested_path_rules() {
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap");
    let request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "deps/cache", "access": "rw" }]
        },
        "for_action": { "command": "cargo test", "cwd": "." }
    }));

    let runner = factory.build_runner(&request);
    let plan = bwrap_process_plan(
        request_process_intent(&request),
        &runner.cwd_root,
        runner.network_allowed,
        &runner.path_rules,
        &runner.bwrap_program,
    );
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &[
            "--bind",
            "/workspace/merry/deps/cache",
            "/workspace/merry/deps/cache"
        ]
    ));
}

#[test]
fn bwrap_permissioned_factory_keeps_approved_paths_for_later_actions() {
    let session_permissions = BwrapSessionPermissions::new();
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_session_permissions(session_permissions.clone());
    let base_runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_session_permissions(session_permissions);
    let first_request = permission_request(json!({
        "requested": {
            "paths": [
                { "path": "/tmp", "access": "rw" },
                { "path": "/var/lib/merry-demo.txt", "access": "ro" }
            ]
        },
        "for_action": { "command": "touch /var/lib/merry-demo.txt", "cwd": null }
    }));
    let later_request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "cat /var/lib/merry-demo.txt", "cwd": null }
    }));
    let narrower_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/tmp", "access": "ro" }]
        },
        "for_action": { "command": "ls /tmp", "cwd": null }
    }));

    let _ = factory.runner_for(&first_request);
    let _ = factory.runner_for(&narrower_request);
    let plan = base_runner
        .plan_for(request_process_intent(&later_request))
        .expect("later ordinary process plan should build");
    let args = os_args(&plan.args);

    assert!(contains_sequence(&args, &["--bind", "/tmp", "/tmp"]));
    assert!(!contains_sequence(&args, &["--ro-bind", "/tmp", "/tmp"]));
    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind",
            "/var/lib/merry-demo.txt",
            "/var/lib/merry-demo.txt"
        ]
    ));
    assert!(!contains_sequence(
        &args,
        &["--bind-try", "/var/tmp", "/var/tmp"]
    ));
    assert!(args.iter().any(|arg| arg == "--unshare-net"));
}

#[test]
fn bwrap_permissioned_factory_reuses_parent_path_grants_for_descendants() {
    let session_permissions = BwrapSessionPermissions::new();
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_session_permissions(session_permissions.clone());
    let parent_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/tmp", "access": "rw" }]
        },
        "for_action": { "command": "mkdir -p /tmp/work", "cwd": null }
    }));
    let descendant_read_write_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/tmp/hello_world.txt", "access": "rw" }]
        },
        "for_action": { "command": "cat /tmp/hello_world.txt", "cwd": null }
    }));
    let descendant_read_only_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/tmp/hello_world.txt", "access": "ro" }]
        },
        "for_action": { "command": "cat /tmp/hello_world.txt", "cwd": null }
    }));
    let network_request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "curl https://example.invalid", "cwd": null }
    }));

    let _ = factory.runner_for(&parent_request);
    let descendant_runner = factory
        .backend()
        .build_runner(&descendant_read_write_request);
    let descendant_plan = descendant_runner
        .plan_for(request_process_intent(&descendant_read_write_request))
        .expect("covered descendant process plan should build");
    let descendant_args = os_args(&descendant_plan.args);
    assert!(contains_sequence(
        &descendant_args,
        &["--bind", "/tmp", "/tmp"]
    ));
    assert!(!contains_sequence(
        &descendant_args,
        &["--bind", "/tmp/hello_world.txt", "/tmp/hello_world.txt"]
    ));
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

    let read_only_session = BwrapSessionPermissions::new();
    let read_only_factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
            .with_bwrap_program("/custom/bin/bwrap")
            .with_session_permissions(read_only_session);
    let read_only_parent_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "/tmp", "access": "ro" }]
        },
        "for_action": { "command": "cat /tmp/hello_world.txt", "cwd": null }
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
fn bwrap_permissioned_factory_caps_requested_write_to_trusted_read_only_rule() {
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_path_rules([PathAccessRule::new(
            PathBuf::from("/workspace/merry/deps"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    let request = permission_request(json!({
        "requested": {
            "paths": [{ "path": "deps", "access": "rw" }]
        },
        "for_action": { "command": "cargo test", "cwd": "." }
    }));

    factory
        .validate_request(&request)
        .expect("read-only policy should cap rather than reject a write request");
    let runner = factory.build_runner(&request);
    let plan = bwrap_process_plan(
        request_process_intent(&request),
        &runner.cwd_root,
        runner.network_allowed,
        &runner.path_rules,
        &runner.bwrap_program,
    );
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind-try",
            "/workspace/merry/deps",
            "/workspace/merry/deps"
        ]
    ));
    assert!(!contains_sequence(
        &args,
        &[
            "--bind-try",
            "/workspace/merry/deps",
            "/workspace/merry/deps"
        ]
    ));
}

#[test]
fn bwrap_permissioned_factory_rejects_requested_path_under_configured_deny() {
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
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
