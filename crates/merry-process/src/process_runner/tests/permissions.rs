use super::{contains_sequence, intent, os_args, permission_request, request_process_intent};
use crate::UnrestrictedPermissionedProcessRunnerFactory;
use crate::process_runner::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessEnvironment, BwrapProcessRunner,
    BwrapSessionPermissions, TokioProcessRunner, bwrap_process_plan,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
    ProcessRunner, StaticPermissionedProcessRunnerFactory,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

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
    let descendant_runner = factory.build_runner(&descendant_read_write_request);
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
    let approved_runner = factory.build_runner(&network_request);
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

#[test]
fn bwrap_permissioned_factory_requires_git_write_per_action() {
    let external_root = tempfile::tempdir().expect("external root should be created");
    let external_git = external_root.path().join(".git");
    std::fs::create_dir(&external_git).expect("external git directory should be created");
    let external_root_path = external_root
        .path()
        .to_str()
        .expect("root path should be utf-8")
        .to_owned();
    let external_git_path = external_git
        .to_str()
        .expect("git path should be utf-8")
        .to_owned();
    let session_permissions = BwrapSessionPermissions::new();
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_session_permissions(session_permissions.clone());
    let base_runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_session_permissions(session_permissions);
    let parent_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": external_root_path.clone(), "access": "rw" }]
        },
        "for_action": { "command": "cp a external/out", "cwd": null }
    }));
    let git_request = permission_request(json!({
        "requested": {
            "paths": [{ "path": external_git_path.clone(), "access": "rw" }]
        },
        "for_action": { "command": "git checkout -- README.md", "cwd": null }
    }));

    let _ = factory.runner_for(&parent_request);
    let _ = factory.runner_for(&git_request);

    let current_git_runner = factory.build_runner(&git_request);
    let current_git_plan = current_git_runner
        .plan_for(request_process_intent(&git_request))
        .expect("the reviewed git action should build");
    let current_git_args = os_args(&current_git_plan.args);
    assert!(contains_sequence(
        &current_git_args,
        &[
            "--bind",
            external_git_path.as_str(),
            external_git_path.as_str()
        ]
    ));
    assert!(!contains_sequence(
        &current_git_args,
        &[
            "--ro-bind",
            external_git_path.as_str(),
            external_git_path.as_str()
        ]
    ));

    let later_plan = base_runner
        .plan_for(request_process_intent(&parent_request))
        .expect("later ordinary action should build");
    let later_args = os_args(&later_plan.args);
    assert!(contains_sequence(
        &later_args,
        &[
            "--bind",
            external_root_path.as_str(),
            external_root_path.as_str()
        ]
    ));
    assert!(contains_sequence(
        &later_args,
        &[
            "--ro-bind",
            external_git_path.as_str(),
            external_git_path.as_str()
        ]
    ));
    assert!(!contains_sequence(
        &later_args,
        &[
            "--bind",
            external_git_path.as_str(),
            external_git_path.as_str()
        ]
    ));
}

#[test]
fn bwrap_git_baseline_cannot_be_upgraded_by_unreviewed_configured_write() {
    let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_path_rules([PathAccessRule::new(
            PathBuf::from("/pathA/.git"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfigWritableCeiling,
        )]);
    let plan = runner
        .plan_for(&intent(None))
        .expect("configured path rules should build");
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &["--ro-bind-try", "/pathA/.git", "/pathA/.git"]
    ));
    assert!(!contains_sequence(
        &args,
        &["--bind-try", "/pathA/.git", "/pathA/.git"]
    ));
}

#[test]
fn bwrap_git_baseline_checks_only_workspace_and_requested_git_paths() {
    let workspace = tempfile::tempdir().expect("workspace should be created");
    let workspace_git = workspace.path().join(".git");
    std::fs::create_dir_all(&workspace_git).expect("workspace git directory should be created");
    let nested_git = workspace.path().join("nested-repo/.git");
    std::fs::create_dir_all(&nested_git).expect("nested git directory should be created");
    let nested_missing_git = workspace.path().join("nested-worktree/.git");
    std::fs::create_dir_all(
        nested_missing_git
            .parent()
            .expect("nested missing git parent should exist"),
    )
    .expect("nested missing git parent should be created");
    let unrequested_git = workspace.path().join("unrequested-repo/.git");
    std::fs::create_dir_all(&unrequested_git).expect("unrequested git directory should be created");
    let requested_repo = workspace.path().join("nested-repo");
    let workspace_git_path = workspace_git
        .to_str()
        .expect("workspace git path should be utf-8");
    let nested_git_path = nested_git
        .to_str()
        .expect("nested git path should be utf-8");
    let nested_missing_git_path = nested_missing_git
        .to_str()
        .expect("nested missing git path should be utf-8");
    let unrequested_git_path = unrequested_git
        .to_str()
        .expect("unrequested git path should be utf-8");

    let runner = BwrapProcessRunner::new_at_workspace_root(workspace.path())
        .with_bwrap_program("/custom/bin/bwrap")
        .with_path_rules([PathAccessRule::new(
            requested_repo,
            PathAccess::ReadWrite,
            PathAccessRuleSource::PermissionReview,
        )]);
    let plan = runner
        .plan_for(&intent(None))
        .expect("git metadata plan should build");
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &["--ro-bind", workspace_git_path, workspace_git_path]
    ));
    assert!(contains_sequence(
        &args,
        &["--ro-bind", nested_git_path, nested_git_path]
    ));
    assert!(!contains_sequence(
        &args,
        &["--bind", nested_git_path, nested_git_path]
    ));
    assert!(!contains_sequence(
        &args,
        &["--tmpfs", nested_missing_git_path]
    ));
    assert!(!contains_sequence(
        &args,
        &[
            "--ro-bind",
            nested_missing_git_path,
            nested_missing_git_path
        ]
    ));
    assert!(!contains_sequence(
        &args,
        &["--ro-bind", unrequested_git_path, unrequested_git_path]
    ));
}

#[test]
fn bwrap_permissioned_factory_keeps_approved_host_integrations_for_later_actions() {
    let mut environment =
        BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
            .expect("environment layout should validate");
    environment.ssh_agent_socket = Some(PathBuf::from("/run/user/1000/ssh-agent.sock"));
    let session_permissions = BwrapSessionPermissions::new();
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_environment(environment.clone())
        .with_session_permissions(session_permissions.clone());
    let request = permission_request(json!({
        "requested": { "host_integrations": ["ssh-agent"] },
        "for_action": { "command": "ssh -T git@example.test", "cwd": null }
    }));

    factory
        .validate_request(&request)
        .expect("configured host integration should be materializable");
    let _ = factory.runner_for(&request);

    let base_runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_environment(environment)
        .with_session_permissions(session_permissions);
    let later_request = permission_request(json!({
        "requested": { "paths": [{ "path": ".", "access": "rw" }] },
        "for_action": { "command": "ssh -T git@example.test", "cwd": null }
    }));
    let plan = base_runner
        .plan_for(request_process_intent(&later_request))
        .expect("later process plan should build");
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind",
            "/run/user/1000/ssh-agent.sock",
            "/run/user/1000/ssh-agent.sock"
        ]
    ));
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
