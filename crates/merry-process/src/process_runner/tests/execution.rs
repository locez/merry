use super::{permission_request, request_process_intent};
use crate::process_runner::{
    TokioProcessRunner, WorkspaceProcessEnvironment, WorkspaceProcessRunner,
    WorkspaceProcessRunnerFactory,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
    ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerOutput,
};
use serde_json::json;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn permissioned_factory_runner_for_invalid_request_fails_closed() {
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
async fn workspace_process_runner_reuses_tmp_and_writes_workspace() {
    let workspace = tempfile::tempdir().expect("workspace tempdir should be created");
    let marker = format!("merry-process-runner-{}", std::process::id());
    let runner = WorkspaceProcessRunner::new_at_workspace_root(workspace.path()).allow_network();

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
        .expect("first workspace action should run");
    assert!(first.ok(), "first workspace action failed: {first:?}");

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
        .expect("second workspace action should run");
    assert!(second.ok(), "second workspace action failed: {second:?}");
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
async fn workspace_git_init_and_local_config_need_no_extra_grant() {
    let workspace = tempfile::tempdir().expect("workspace tempdir should be created");
    let runner = WorkspaceProcessRunner::new_at_workspace_root(workspace.path()).allow_network();
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
    assert!(initialized.ok(), "git init should succeed: {initialized:?}");
    assert!(workspace.path().join(".git").is_dir());

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
    let configured = runner
        .run(
            modify_intent,
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("git config should return process output");
    assert!(
        configured.ok(),
        "workspace git metadata should stay writable: {configured:?}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn read_only_workspace_can_read_and_cannot_write() {
    let workspace =
        tempfile::tempdir_in(fixture_root()).expect("workspace outside baseline writes");
    std::fs::write(workspace.path().join("note.txt"), "hello").expect("note");
    let runner =
        WorkspaceProcessRunner::new_at_workspace_root(workspace.path()).read_only_workspace();
    let context = ProcessRunnerContext::new(CancellationToken::new());

    let read = run_shell(&runner, "cat note.txt", &context).await;
    assert!(read.ok(), "read should succeed: {}", read.stderr_text());
    assert_eq!(read.stdout_text(), "hello");

    let write = run_shell(&runner, "touch denied.txt", &context).await;
    assert!(
        !write.ok(),
        "a read-only workspace must reject writes: {}",
        write.stderr_text()
    );
    assert!(!workspace.path().join("denied.txt").exists());

    let temporary = run_shell(
        &runner,
        "touch /tmp/merry-readonly-workspace-marker",
        &context,
    )
    .await;
    assert!(
        temporary.ok(),
        "/tmp stays writable: {}",
        temporary.stderr_text()
    );
    let _ = std::fs::remove_file("/tmp/merry-readonly-workspace-marker");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_git_status_reads_the_user_ignore_file() {
    let root = tempfile::tempdir_in(fixture_root()).expect("fixture root");
    let home = root.path().join("home");
    let workspace = root.path().join("repo");
    std::fs::create_dir_all(home.join(".config/git")).expect("git config dir");
    std::fs::write(home.join(".config/git/ignore"), "*.log\n").expect("ignore");
    std::fs::write(home.join(".git-credentials"), "secret\n").expect("credentials");
    std::fs::create_dir_all(&workspace).expect("repo");
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&workspace)
        .env("HOME", &home)
        .status()
        .expect("git init");
    assert!(init.success(), "host git init failed");

    let environment =
        WorkspaceProcessEnvironment::new("/usr/bin:/bin", &home, "/tmp").expect("environment");
    let runner =
        WorkspaceProcessRunner::new_at_workspace_root(&workspace).with_environment(environment);
    let context = ProcessRunnerContext::new(CancellationToken::new());
    let status = run_shell(&runner, "git status --short --branch", &context).await;
    assert!(
        status.ok(),
        "git status should read the user ignore file: {}",
        status.stderr_text()
    );

    let secret = run_shell(&runner, "cat \"$HOME/.git-credentials\"", &context).await;
    assert!(
        !secret.ok(),
        "git credentials stay unreadable: {}",
        secret.stdout_text()
    );
}

#[cfg(target_os = "linux")]
async fn run_shell(
    runner: &WorkspaceProcessRunner,
    command: &str,
    context: &ProcessRunnerContext,
) -> ProcessRunnerOutput {
    let intent = ProcessActionIntent::new(
        vec!["/bin/sh".to_owned(), "-c".to_owned(), command.to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        4096,
        4096,
    )
    .expect("shell intent");
    runner
        .run(intent, context.clone())
        .await
        .expect("sandboxed command should return output")
}

#[cfg(target_os = "linux")]
fn fixture_root() -> PathBuf {
    for candidate in [
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("HOME"),
    ] {
        let Some(value) = candidate else {
            continue;
        };
        let path = PathBuf::from(value);
        let baseline = ["/tmp", "/var/tmp", "/dev", "/dev/shm", "/dev/pts"]
            .iter()
            .any(|root| path == Path::new(root) || path.starts_with(root));
        if path.is_dir() && !baseline {
            return path;
        }
    }
    panic!("need a directory outside /tmp, /var/tmp, and /dev");
}
