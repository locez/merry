use super::{contains_sequence, os_args, permission_request, request_process_intent};
use crate::process_runner::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, BwrapSessionPermissions,
    TokioProcessRunner,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
    ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
};
use serde_json::json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn bwrap_permissioned_factory_runner_for_invalid_request_fails_closed() {
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

    let runner = factory.runner_for(&request);
    let error = runner
        .run(
            request_process_intent(&request).clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect_err("invalid path request must not reach the process backend");
    assert!(
        error
            .to_string()
            .contains("denied by configured path policy")
    );
}

#[tokio::test]
async fn tokio_process_runner_inherits_current_process_environment() {
    let Ok(path) = std::env::var("PATH") else {
        return;
    };
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let intent = ProcessActionIntent::new(
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf '%s\n%s' \"${PATH-}\" \"${HOME-}\"".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        64 * 1024,
        1024,
    )
    .expect("process intent should be valid");

    let output = TokioProcessRunner::new()
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .expect("process should run");

    assert_eq!(output.status(), ProcessExitStatus::Exited(0));
    assert_eq!(output.stdout_text(), format!("{path}\n{home}"));
}

#[tokio::test]
async fn tokio_process_runner_applies_validated_environment_overrides() {
    let intent = ProcessActionIntent::new(
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf '%s' \"${MERRY_TEST_MODE-}\"".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        64 * 1024,
        1024,
    )
    .expect("process intent should be valid");

    let output = TokioProcessRunner::new()
        .with_environment_overrides([(OsString::from("MERRY_TEST_MODE"), OsString::from("host"))])
        .expect("environment override should validate")
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .expect("process should run");

    assert_eq!(output.status(), ProcessExitStatus::Exited(0));
    assert_eq!(output.stdout_text(), "host");
}

#[cfg(unix)]
#[tokio::test]
async fn tokio_process_runner_preserves_non_utf8_output() {
    let intent = ProcessActionIntent::new(
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf '\\377\\000'; printf '\\376\\001' >&2".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("process intent should be valid");

    let output = TokioProcessRunner::new()
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .expect("binary process output must not fail the runner");

    assert_eq!(output.status(), ProcessExitStatus::Exited(0));
    assert_eq!(output.stdout_data(), &[0xff, 0]);
    assert_eq!(output.stderr_data(), &[0xfe, 1]);
    assert!(!output.stdout_is_utf8());
    assert!(!output.stderr_is_utf8());
    assert!(output.stdout_text().contains('\u{fffd}'));
    assert!(output.stderr_text().contains('\u{fffd}'));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn bwrap_process_runner_reuses_tmp_and_writes_workspace() {
    let workspace = tempfile::tempdir().expect("workspace tempdir should be created");
    let marker = format!("merry-process-runner-{}", std::process::id());
    // This container cannot create a network namespace; filesystem and
    // temporary-directory behavior are independent of that capability.
    let runner = BwrapProcessRunner::new_at_workspace_root(workspace.path())
        .allow_network()
        .with_bwrap_program("/usr/bin/bwrap");

    let write_tmp = ProcessActionIntent::new(
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("printf 'tmp-ok' > /tmp/{marker}"),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("temporary write intent should be valid");
    let first = runner
        .run(
            write_tmp,
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("first bwrap action should run");
    assert!(first.ok(), "first bwrap action failed: {first:?}");

    let read_tmp = ProcessActionIntent::new(
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("cat /tmp/{marker}"),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("temporary read intent should be valid");
    let second = runner
        .run(
            read_tmp,
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("second bwrap action should run");
    assert!(second.ok(), "second bwrap action failed: {second:?}");
    assert_eq!(second.stdout_text(), "tmp-ok");

    let write_workspace = ProcessActionIntent::new(
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf 'workspace-ok' > workspace-write.txt".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("workspace write intent should be valid");
    let third = runner
        .run(
            write_workspace,
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("workspace write action should run");
    assert!(third.ok(), "workspace write action failed: {third:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("workspace-write.txt"))
            .expect("workspace write should persist"),
        "workspace-ok"
    );

    let _ = std::fs::remove_file(Path::new("/tmp").join(marker));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn bwrap_git_init_allows_unreviewed_initialization_and_requires_later_git_write_grant() {
    let workspace = tempfile::tempdir().expect("workspace tempdir should be created");
    let runner = BwrapProcessRunner::new_at_workspace_root(workspace.path())
        .allow_network()
        .with_bwrap_program("/usr/bin/bwrap");
    let init_intent = ProcessActionIntent::new(
        vec!["git".to_owned(), "init".to_owned(), ".".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        4096,
        4096,
    )
    .expect("git init intent should be valid");

    let initialized = runner
        .run(
            init_intent,
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("git init should return process output");
    assert!(
        initialized.ok(),
        "unreviewed git init should succeed: {initialized:?}"
    );
    assert!(
        workspace.path().join(".git").is_dir(),
        "unreviewed git init should persist host metadata"
    );

    let modify_intent = ProcessActionIntent::new(
        vec![
            "git".to_owned(),
            "config".to_owned(),
            "--local".to_owned(),
            "user.name".to_owned(),
            "Merry".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        4096,
        4096,
    )
    .expect("git config intent should be valid");
    let denied_plan = runner
        .plan_for(&modify_intent)
        .expect("later git plan should build");
    let git_path = workspace.path().join(".git");
    let git_path = git_path.to_str().expect("git path should be utf-8");
    let denied_args = os_args(&denied_plan.args);
    assert!(contains_sequence(
        &denied_args,
        &["--ro-bind", git_path, git_path]
    ));
    let denied = runner
        .run(
            modify_intent.clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("blocked git modification should still return process output");
    assert!(
        !denied.ok(),
        "unreviewed git modification should fail: {denied:?}"
    );

    let session_permissions = BwrapSessionPermissions::new();
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(workspace.path())
        .with_bwrap_program("/usr/bin/bwrap")
        .with_session_permissions(session_permissions.clone());
    let request = permission_request(json!({
        "requested": {
            "network": true,
            "paths": [{ "path": ".git", "access": "rw" }]
        },
        "for_action": {
            "command": "git config --local user.name Merry",
            "cwd": null
        }
    }));
    factory
        .validate_request(&request)
        .expect("reviewed git modification request should validate");
    let reviewed = factory.runner_for(&request);
    let approved = reviewed
        .run(
            request_process_intent(&request).clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("reviewed git modification should return process output");
    assert!(
        approved.ok(),
        "reviewed git modification failed: {approved:?}"
    );

    let later = BwrapProcessRunner::new_at_workspace_root(workspace.path())
        .allow_network()
        .with_bwrap_program("/usr/bin/bwrap")
        .with_session_permissions(session_permissions);
    let later_plan = later
        .plan_for(&modify_intent)
        .expect("later process plan should build");
    let later_args = os_args(&later_plan.args);
    assert!(contains_sequence(
        &later_args,
        &["--ro-bind", git_path, git_path]
    ));
    let later_denied = later
        .run(
            modify_intent,
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("later unreviewed git modification should return process output");
    assert!(
        !later_denied.ok(),
        "later git modification should require another review: {later_denied:?}"
    );
}
