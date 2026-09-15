//! Automatic Git metadata protection: `.git` stays read-only even when a
//! configured or reviewed rule covers its parent, and only the workspace and
//! requested repositories are considered.

use crate::process_runner::tests::{
    contains_sequence, intent, os_args, permission_request, request_process_intent,
};
use crate::process_runner::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, BwrapSessionPermissions,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
};
use serde_json::json;

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

    let current_git_runner = factory.backend().build_runner(&git_request);
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
fn bwrap_git_baseline_still_protects_metadata_under_a_configured_write() {
    let root = tempfile::tempdir().expect("configured root should be created");
    let git = root.path().join(".git");
    std::fs::create_dir(&git).expect("git directory should be created");
    let root_path = root
        .path()
        .to_str()
        .expect("configured root path should be utf-8");
    let git_path = git.to_str().expect("git path should be utf-8");
    let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_path_rules([PathAccessRule::new(
            root.path(),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    let plan = runner
        .plan_for(&intent(None))
        .expect("configured path rules should build");
    let args = os_args(&plan.args);

    // `readwrite_paths` is preauthorized, but Git metadata inside it keeps the
    // automatic read-only baseline: a separate per-action grant is required.
    assert!(contains_sequence(
        &args,
        &["--bind-try", root_path, root_path]
    ));
    assert!(contains_sequence(&args, &["--ro-bind", git_path, git_path]));
    assert!(!contains_sequence(&args, &["--bind", git_path, git_path]));
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
