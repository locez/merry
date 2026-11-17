//! Review and deny rules that Landlock cannot subtract from an allow list.
//!
//! A review or deny path inside the workspace, `/tmp`, or another granted
//! directory fails the launch. It is not masked, and the command does not
//! start. A review path outside those grants is omitted from the launch.

use super::{permission_request, request_process_intent};
use crate::process_runner::{
    ProcessSessionPermissions, WorkspaceProcessRunner, WorkspaceProcessRunnerFactory,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
    ProcessActionIntent, ProcessEnvPolicy, ProcessRunner, ProcessRunnerContext,
};
use serde_json::json;
use std::{fs, path::PathBuf};
use tokio_util::sync::CancellationToken;

fn launch_intent() -> ProcessActionIntent {
    ProcessActionIntent::new(
        vec!["/bin/true".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("process intent should be valid")
}

fn contains_path(paths: &[String], expected: &str) -> bool {
    paths.iter().any(|path| path == expected)
}

fn rule(path: &str, access: PathAccess) -> PathAccessRule {
    PathAccessRule::new(path, access, PathAccessRuleSource::TrustedGlobalConfig)
}

#[test]
fn review_inside_the_workspace_fails_closed_before_launch() {
    let runner = WorkspaceProcessRunner::new_at_workspace_root("/workspace/merry").with_path_rules(
        [rule("/workspace/merry/secret", PathAccess::ReadOnly).with_review_required()],
    );

    let error = runner
        .plan_for(&launch_intent())
        .expect_err("a review path inside the workspace must not launch");

    let message = error.to_string();
    assert!(message.contains("review_paths"), "{message}");
    assert!(message.contains("writable workspace"), "{message}");
    assert!(message.contains("/workspace/merry/secret"), "{message}");
    assert!(
        message.contains("Remove `/workspace/merry/secret` from review_paths"),
        "{message}"
    );
}

#[test]
fn deny_inside_the_workspace_fails_closed_before_launch() {
    let runner = WorkspaceProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_path_rules([rule("/workspace/merry/secret", PathAccess::Deny)]);

    let error = runner
        .plan_for(&launch_intent())
        .expect_err("a deny path inside the workspace must not launch");

    let message = error.to_string();
    assert!(message.contains("deny_paths"), "{message}");
    assert!(
        message.contains("Remove `/workspace/merry/secret` from deny_paths"),
        "{message}"
    );
}

#[test]
fn review_outside_allowed_directories_is_omitted_from_the_launch() {
    let runner = WorkspaceProcessRunner::new_at_workspace_root("/workspace/merry").with_path_rules(
        [rule("/var/merry-review-outside", PathAccess::ReadOnly).with_review_required()],
    );

    let launch = runner
        .plan_for(&launch_intent())
        .expect("a review path outside the allow list can be omitted");

    assert!(!contains_path(
        launch.read_only(),
        "/var/merry-review-outside"
    ));
    assert!(!contains_path(
        launch.read_write(),
        "/var/merry-review-outside"
    ));
    assert!(contains_path(launch.read_write(), "/workspace/merry"));
    assert!(contains_path(launch.read_write(), "/tmp"));
}

#[test]
fn trusted_read_only_inside_the_workspace_does_not_fail_the_launch() {
    let runner = WorkspaceProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_path_rules([rule("/workspace/merry/notes", PathAccess::ReadOnly)]);

    let launch = runner
        .plan_for(&launch_intent())
        .expect("a read-only rule inside a writable parent is not a Landlock exclusion");

    assert!(contains_path(launch.read_write(), "/workspace/merry"));
    assert!(contains_path(launch.read_only(), "/workspace/merry/notes"));
}

#[test]
fn review_grants_stay_action_scoped() {
    let session = ProcessSessionPermissions::new();
    let view = session.view();
    let secret = PathBuf::from("/var/merry-parent/secret");
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_path_rules([
            rule("/var/merry-parent", PathAccess::ReadWrite),
            rule("/var/merry-parent/secret", PathAccess::ReadOnly).with_review_required(),
        ])
        .with_session_permissions(session.clone());
    let secret_request = permission_request(json!({
        "requested": { "paths": [{ "path": secret, "access": "ro" }] },
        "for_action": { "command": "cat /var/merry-parent/secret", "cwd": null }
    }));
    let parent_request = permission_request(json!({
        "requested": { "paths": [{ "path": "/var/merry-parent", "access": "rw" }] },
        "for_action": { "command": "ls /var/merry-parent", "cwd": null }
    }));

    assert!(
        !factory
            .request_capabilities_are_satisfied(&secret_request)
            .expect("review capability query")
    );
    factory
        .validate_request(&secret_request)
        .expect("review can still be approved for one action");
    let _secret_runner = factory.runner_for(&secret_request);
    assert!(
        view.snapshot()
            .expect("session snapshot")
            .path_rules()
            .iter()
            .all(|rule| rule.path() != secret)
    );
    assert!(
        !factory
            .request_capabilities_are_satisfied(&secret_request)
            .expect("review capability query after approval")
    );

    assert!(
        factory
            .request_capabilities_are_satisfied(&parent_request)
            .expect("parent capability query")
    );
    let _parent_runner = factory.runner_for(&parent_request);
    assert!(
        !factory
            .request_capabilities_are_satisfied(&secret_request)
            .expect("parent approval must not satisfy a reviewed child")
    );

    let runner =
        WorkspaceProcessRunner::new_at_workspace_root("/workspace/merry").with_path_rules([
            rule("/var/merry-parent", PathAccess::ReadWrite),
            rule("/var/merry-parent/secret", PathAccess::ReadOnly).with_review_required(),
        ]);
    let error = runner
        .plan_for(request_process_intent(&parent_request))
        .expect_err("a reviewed child of an allowed directory cannot be subtracted");
    let message = error.to_string();
    assert!(message.contains("review_paths"), "{message}");
    assert!(message.contains("readwrite_paths"), "{message}");
    assert!(
        message.contains("Remove `/var/merry-parent/secret` from review_paths"),
        "{message}"
    );
}

#[tokio::test]
async fn excluded_review_path_does_not_start_the_command() {
    let directory = tempfile::tempdir().expect("fixture");
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let secret = workspace.join("secret");
    fs::write(&secret, "protected").expect("secret");
    let executed = workspace.join("executed");
    let executed_arg = executed.to_str().expect("fixture path is UTF-8").to_owned();
    let runner = WorkspaceProcessRunner::new_at_workspace_root(&workspace).with_path_rules([
        PathAccessRule::new(
            secret,
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )
        .with_review_required(),
    ]);
    let intent = ProcessActionIntent::new(
        vec!["/usr/bin/touch".into(), executed_arg],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("touch intent");

    let error = runner
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .expect_err("review inside /tmp must fail before the command starts");

    let message = error.to_string();
    assert!(message.contains("review_paths"), "{message}");
    assert!(message.contains("writable workspace"), "{message}");
    assert!(!executed.exists());
}
