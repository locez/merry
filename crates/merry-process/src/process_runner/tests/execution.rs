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

#[cfg(unix)]
fn background_intent(argv: Vec<String>) -> ProcessActionIntent {
    ProcessActionIntent::new(argv, None, ProcessEnvPolicy::empty(), None, 1024, 1024)
        .expect("process intent should be valid")
        .with_background(true)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    rustix::process::Pid::from_raw(pid as rustix::process::RawPid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

#[cfg(unix)]
async fn wait_until_dead(pid: u32) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if !process_alive(pid) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("process {pid} was still alive");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn background_command_returns_while_the_process_stays_alive() {
    let runner = TokioProcessRunner::new();
    let started = std::time::Instant::now();
    let output = runner
        .run(
            background_intent(vec!["/bin/sleep".to_owned(), "30".to_owned()]),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("background sleep should be retained");

    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(output.status(), ProcessExitStatus::Running);
    assert!(output.ok());
    let pid = output.running_pid().expect("retained process has a pid");
    assert!(process_alive(pid));
    let id = output
        .background_process_id()
        .expect("retained process has an id");
    assert_eq!(id.as_str(), "bg-1");
    drop(runner);
    wait_until_dead(pid).await;
}

#[cfg(unix)]
#[tokio::test]
async fn background_command_that_exits_immediately_is_not_retained() {
    let started = std::time::Instant::now();
    let output = TokioProcessRunner::new()
        .run(
            background_intent(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "echo hi".to_owned(),
            ]),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("fast background command should finish");

    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(output.status(), ProcessExitStatus::Exited(0));
    assert_eq!(output.stdout_text(), "hi\n");
    assert!(output.running_pid().is_none());
    assert!(output.background_process_id().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_the_runner_kills_background_children() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pidfile = dir.path().join("child.pid");
    let script = format!("sleep 300 & echo $! > '{}'; wait", pidfile.display());
    let runner = TokioProcessRunner::new();
    let output = runner
        .run(
            background_intent(vec!["/bin/sh".to_owned(), "-c".to_owned(), script]),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("background shell should be retained");
    let shell_pid = output.running_pid().expect("shell pid");
    let child_pid = std::fs::read_to_string(&pidfile)
        .expect("grandchild pid file")
        .trim()
        .parse::<u32>()
        .expect("grandchild pid");
    assert!(process_alive(shell_pid));
    assert!(process_alive(child_pid));
    drop(runner);
    wait_until_dead(shell_pid).await;
    wait_until_dead(child_pid).await;
}

#[cfg(unix)]
#[tokio::test]
async fn stop_background_process_kills_the_retained_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pidfile = dir.path().join("child.pid");
    let script = format!(
        "echo ready; sleep 300 & echo $! > '{}'; wait",
        pidfile.display()
    );
    let runner = TokioProcessRunner::new();
    let output = runner
        .run(
            background_intent(vec!["/bin/sh".to_owned(), "-c".to_owned(), script]),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("background shell should be retained");
    let id = output.background_process_id().expect("id").clone();
    let shell_pid = output.running_pid().expect("shell pid");
    let child_pid = std::fs::read_to_string(&pidfile)
        .expect("grandchild pid file")
        .trim()
        .parse::<u32>()
        .expect("grandchild pid");
    let control = runner.background_control().expect("background control");
    let read = control.read(&id).expect("read retained output");
    assert!(read.stdout().contains("ready"));
    let stopped = control.stop(&id).expect("stop");
    assert!(stopped.changed());
    assert_eq!(stopped.summary().id(), &id);
    wait_until_dead(shell_pid).await;
    wait_until_dead(child_pid).await;
    let again = control.stop(&id).expect("second stop");
    assert!(!again.changed());
    assert!(
        control
            .read(&merry_runtime::BackgroundProcessId::new("bg-9").expect("id"))
            .is_err()
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn background_process_outlives_the_permissioned_runner() {
    let workspace = tempfile::tempdir().expect("workspace");
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root(workspace.path());
    let request = permission_request(json!({
        "requested": { "network": true },
        "for_action": { "command": "sleep 30", "cwd": null }
    }));
    let runner = factory.runner_for(&request);
    let started = std::time::Instant::now();
    let output = runner
        .run(
            background_intent(vec!["/bin/sleep".to_owned(), "30".to_owned()]),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("sandboxed background sleep should be retained");

    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(output.status(), ProcessExitStatus::Running);
    let pid = output.running_pid().expect("sandboxed pid");
    assert!(process_alive(pid));
    drop(runner);
    assert!(
        process_alive(pid),
        "dropping the per-request runner must not kill a background process"
    );
    drop(factory);
    wait_until_dead(pid).await;
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn sandboxed_command_can_read_and_write_only_the_memory_tree() {
    let root = tempfile::tempdir_in(fixture_root()).expect("fixture root");
    let workspace = root.path().join("workspace");
    let memory = root.path().join("data/merry/memory");
    let secret = root.path().join("data/merry/secret.txt");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(memory.join("topics")).expect("memory topics");
    std::fs::write(memory.join("topics/hello.md"), "# 你好\n").expect("note");
    std::fs::write(&secret, "secret\n").expect("secret");

    let runner = WorkspaceProcessRunner::new_at_workspace_root(&workspace)
        .read_only_workspace()
        .with_path_rules([PathAccessRule::new(
            memory.clone(),
            PathAccess::ReadWrite,
            PathAccessRuleSource::DurableMemory,
        )]);
    let context = ProcessRunnerContext::new(CancellationToken::new());
    let memory_display = memory.display();
    let listed = run_shell(
        &runner,
        &format!("ls '{memory_display}/topics' && cat '{memory_display}/topics/hello.md'"),
        &context,
    )
    .await;
    assert!(
        listed.ok(),
        "memory should be readable: {}",
        listed.stderr_text()
    );
    assert!(listed.stdout_text().contains("你好"));

    let write = run_shell(
        &runner,
        &format!("printf 'x\\n' > '{memory_display}/topics/written.md'"),
        &context,
    )
    .await;
    assert!(
        write.ok(),
        "memory should be writable: {}",
        write.stderr_text()
    );
    assert_eq!(
        std::fs::read_to_string(memory.join("topics/written.md")).expect("written note"),
        "x\n"
    );

    let workspace_write = run_shell(&runner, "touch denied.txt", &context).await;
    assert!(
        !workspace_write.ok(),
        "a read-only workspace stays read-only"
    );

    let denied = run_shell(&runner, &format!("cat '{}'", secret.display()), &context).await;
    assert!(!denied.ok(), "a sibling outside memory stays unreadable");
    assert!(!denied.stdout_text().contains("secret"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn network_denied_sandbox_allows_granted_unix_sockets_and_blocks_inet_and_ungranted_sockets()
{
    let root = tempfile::tempdir_in(fixture_root()).expect("fixture root");
    let workspace = root.path().join("workspace");
    let granted_dir = root.path().join("granted");
    let denied_dir = root.path().join("denied");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&granted_dir).expect("granted");
    std::fs::create_dir_all(&denied_dir).expect("denied");

    let granted_sock = granted_dir.join("agent.sock");
    let denied_sock = denied_dir.join("secret.sock");
    let _granted_listener = std::os::unix::net::UnixListener::bind(&granted_sock)
        .expect("granted unix listener should bind");
    let _denied_listener = std::os::unix::net::UnixListener::bind(&denied_sock)
        .expect("denied unix listener should bind");

    let runner = WorkspaceProcessRunner::new_at_workspace_root(&workspace).with_path_rules([
        PathAccessRule::new(
            granted_sock.clone(),
            PathAccess::ReadWrite,
            PathAccessRuleSource::PermissionReview,
        ),
    ]);
    let context = ProcessRunnerContext::new(CancellationToken::new());
    let script = format!(
        r#"python3 -c '
import os, socket, sys

try:
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    s.close()
    print("INET=ALLOWED")
except OSError:
    print("INET=DENIED")

local = os.path.join(sys.argv[1], "local.sock")
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(local)
srv.listen(1)
cli = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
cli.connect(local)
cli.close()
srv.close()
print("LOCAL_UNIX=ALLOWED")

cli = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
cli.connect(sys.argv[2])
cli.close()
print("GRANTED_UNIX=ALLOWED")

try:
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(os.path.join(os.path.dirname(sys.argv[3]), "ungranted-bind.sock"))
    srv.close()
    print("DENIED_BIND=ALLOWED")
except OSError:
    print("DENIED_BIND=DENIED")

try:
    cli = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    cli.connect(sys.argv[3])
    cli.close()
    print("DENIED_UNIX=ALLOWED")
except OSError:
    print("DENIED_UNIX=DENIED")
' '{}' '{}' '{}'"#,
        workspace.display(),
        granted_sock.display(),
        denied_sock.display(),
    );

    let output = run_shell(&runner, &script, &context).await;
    assert!(
        output.ok(),
        "unix socket sandbox probe failed: stderr={}",
        output.stderr_text()
    );
    let stdout = output.stdout_text();
    assert!(stdout.contains("INET=DENIED"), "{stdout}");
    assert!(stdout.contains("LOCAL_UNIX=ALLOWED"), "{stdout}");
    assert!(stdout.contains("GRANTED_UNIX=ALLOWED"), "{stdout}");
    assert!(stdout.contains("DENIED_BIND=DENIED"), "{stdout}");
    // macOS Seatbelt scopes `remote unix-socket` to granted writable paths and
    // blocks connecting to ungranted sockets; Linux Landlock AccessFs mediates
    // socket creation (`MakeSock`, checked via DENIED_BIND above) rather than
    // `connect()` on already-bound pathname sockets.
    #[cfg(target_os = "macos")]
    assert!(stdout.contains("DENIED_UNIX=DENIED"), "{stdout}");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn fixture_root() -> PathBuf {
    for candidate in [
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("HOME"),
    ] {
        let Some(value) = candidate else {
            continue;
        };
        let path = PathBuf::from(value);
        let baseline = [
            "/tmp",
            "/private/tmp",
            "/var/tmp",
            "/private/var/tmp",
            "/var/folders",
            "/private/var/folders",
            "/dev",
            "/dev/shm",
            "/dev/pts",
        ]
        .iter()
        .any(|root| path == Path::new(root) || path.starts_with(root));
        if path.is_dir() && !baseline {
            return path;
        }
    }
    panic!("need a directory outside /tmp, /var/tmp, and /dev");
}
