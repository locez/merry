#![cfg(target_os = "linux")]

use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
use merry_process::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessEnvironment, BwrapProcessRunner,
    BwrapSshConfigFiles,
};
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource,
    PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessEnvPolicy, ProcessRunner,
    ProcessRunnerContext, ProcessRunnerOutput,
};
use std::{ffi::OsString, fs, os::unix::fs::symlink, path::Path, process::Command};
use tokio_util::sync::CancellationToken;

const HOST_KEY: &str = "sandbox.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";

#[test]
fn oversized_system_ssh_config_does_not_block_inner_actions() {
    const CHILD_ENV: &str = "MERRY_SSH_OVERSIZED_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ENV) {
        let root = Path::new(&root);
        let environment =
            BwrapProcessEnvironment::new("/usr/bin:/bin", root.join("home"), "/tmp").unwrap();
        let runner = BwrapProcessRunner::new_at_workspace_root(root.join("workspace"))
            .with_environment(environment);
        let output = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(execute(&runner, "printf unrelated-action-ran"));
        assert!(output.ok(), "{output:?}");
        assert!(output.stdout_text().contains("unrelated-action-ran"));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("home")).unwrap();
    fs::create_dir(root.join("workspace")).unwrap();
    let config = root.join("ssh_config");
    fs::write(&config, format!("# {}\n", "x".repeat(1024 * 1024))).unwrap();
    let output = Command::new("bwrap")
        .args([
            "--unshare-user",
            "--unshare-net",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--bind",
        ])
        .arg(root)
        .arg(root)
        .arg("--ro-bind")
        .arg(&config)
        .arg("/etc/ssh/ssh_config")
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "oversized_system_ssh_config_does_not_block_inner_actions",
            "--nocapture",
        ])
        .env(CHILD_ENV, root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn prepared_snapshots_supply_independent_readonly_files_to_each_command() {
    let source = Path::new("/etc/ssh/ssh_config");
    let original = fs::read(source).expect("openssh-client system configuration");
    let files = BwrapSshConfigFiles::prepare(source, |path| Ok(Some(path.to_path_buf()))).unwrap();
    let mut args = [
        "--unshare-user",
        "--unshare-net",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();
    files.append_args(&mut args);
    args.extend([
        OsString::from("/bin/sh"),
        OsString::from("-eu"),
        OsString::from("-c"),
        OsString::from(
            "test \"$(stat -c %u /etc/ssh/ssh_config)\" = \"$(id -u)\"; test ! -w /etc/ssh/ssh_config; cat /etc/ssh/ssh_config",
        ),
    ]);
    for _ in 0..2 {
        let mut command = Command::new("bwrap");
        command.args(&args).env_clear().env("PATH", "/usr/bin:/bin");
        files.configure_command(&mut command).unwrap();
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout == original, "snapshot contents changed");
    }
    assert!(
        fs::read(source).unwrap() == original,
        "host contents changed"
    );
}

async fn execute(runner: &dyn ProcessRunner, script: &str) -> ProcessRunnerOutput {
    let output = runner
        .run(
            ProcessActionIntent::new(
                vec![
                    "/bin/sh".into(),
                    "-eu".into(),
                    "-c".into(),
                    format!("printf 'sandbox-started\\n'; {script}"),
                ],
                None,
                ProcessEnvPolicy::empty(),
                None,
                16384,
                16384,
            )
            .unwrap(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(
        output.stdout_text().contains("sandbox-started"),
        "{output:?}"
    );
    output
}

#[tokio::test]
async fn system_ssh_configuration_is_readable_without_disabling_checks_or_an_agent() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&workspace).unwrap();
    let source = Path::new("/etc/ssh/ssh_config");
    let original = fs::read(source).expect("openssh-client system configuration");
    let original_metadata = fs::metadata(source).unwrap();
    let environment = BwrapProcessEnvironment::new("/usr/bin:/bin", &home, "/tmp").unwrap();
    let runner =
        BwrapProcessRunner::new_at_workspace_root(&workspace).with_environment(environment);
    let script = "ssh -G -F /etc/ssh/ssh_config -o StrictHostKeyChecking=yes -o Hostname=127.0.0.1 sandbox.example; test ! -w /etc/ssh/ssh_config; for descriptor in /proc/self/fd/*; do target=$(readlink \"$descriptor\" || true); case \"$target\" in *merry-ssh-config*) exit 1;; esac; done";
    for _ in 0..2 {
        let output = execute(&runner, script).await;
        assert!(output.ok(), "{output:?}");
        assert!(
            output.stdout_text().lines().any(|line| matches!(
                line,
                "stricthostkeychecking true" | "stricthostkeychecking yes"
            )),
            "{output:?}"
        );
    }
    assert_eq!(fs::read(source).unwrap(), original);
    assert_eq!(
        fs::metadata(source).unwrap().permissions(),
        original_metadata.permissions()
    );
}

#[tokio::test]
async fn known_hosts_are_readonly_and_still_require_path_review() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let ssh = root.join(".ssh");
    let workspace = root.join("workspace");
    fs::create_dir(&ssh).unwrap();
    fs::create_dir(&workspace).unwrap();
    let known = ssh.join("known_hosts");
    fs::write(&known, HOST_KEY).unwrap();
    fs::write(ssh.join("known_hosts2"), HOST_KEY).unwrap();
    let environment = BwrapProcessEnvironment::new("/usr/bin:/bin", root, "/tmp")
        .unwrap()
        .with_host_integrations([HostIntegration::SshAgent]);
    let runner =
        BwrapProcessRunner::new_at_workspace_root(&workspace).with_environment(environment.clone());
    let script = "ssh-keygen -F sandbox.example -f \"$HOME/.ssh/known_hosts\"; test ! -w \"$HOME/.ssh/known_hosts\"; ssh-keygen -F sandbox.example -f \"$HOME/.ssh/known_hosts2\"; test ! -w \"$HOME/.ssh/known_hosts2\"";
    assert!(execute(&runner, script).await.ok());
    let rules = vec![
        PathAccessRule::new(
            &ssh,
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            &ssh,
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )
        .with_review_required(),
    ];
    let runner = runner.with_path_rules(rules.clone());
    assert!(
        execute(&runner, "test ! -f \"$HOME/.ssh/known_hosts\"")
            .await
            .ok()
    );
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(&workspace)
        .with_environment(environment)
        .with_path_rules(rules);
    let script = "ssh-keygen -F sandbox.example -f \"$HOME/.ssh/known_hosts\"; test ! -f \"$HOME/.ssh/known_hosts2\"";
    let call = PendingToolCall::new(
        ToolCallId::new("known-host-review").unwrap(),
        ToolName::new("request_permissions").unwrap(),
        ToolCallArguments::try_from(serde_json::json!({
            "requested": {"paths": [{"path": known, "access": "ro"}]},
            "for_action": {"command": script, "cwd": null},
        }))
        .unwrap(),
    );
    let request = merry_runtime::parse_permission_request(&call).unwrap();
    factory.validate_request(&request).unwrap();
    assert!(
        execute(factory.runner_for(&request).as_ref(), script)
            .await
            .ok()
    );
    assert!(
        execute(&runner, "test ! -f \"$HOME/.ssh/known_hosts\"")
            .await
            .ok()
    );
    assert_eq!(fs::read_to_string(known).unwrap(), HOST_KEY);
}

#[tokio::test]
async fn symlinked_known_hosts_cannot_bypass_source_deny() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join(".ssh")).unwrap();
    fs::create_dir(root.join("workspace")).unwrap();
    let source = root.join("host-data");
    fs::write(&source, HOST_KEY).unwrap();
    symlink(&source, root.join(".ssh/known_hosts")).unwrap();
    let environment = BwrapProcessEnvironment::new("/usr/bin:/bin", root, "/tmp")
        .unwrap()
        .with_host_integrations([HostIntegration::SshAgent]);
    let runner = BwrapProcessRunner::new_at_workspace_root(root.join("workspace"))
        .with_environment(environment)
        .with_path_rules([PathAccessRule::new(
            source,
            PathAccess::Deny,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
    let output = execute(
        &runner,
        "if ssh-keygen -F sandbox.example -f \"$HOME/.ssh/known_hosts\"; then exit 1; fi",
    )
    .await;
    assert!(output.ok(), "{output:?}");
}

#[tokio::test]
async fn reviewed_system_ssh_config_is_not_restored_by_compatibility_mounts() {
    let directory = tempfile::tempdir().unwrap();
    let source = Path::new("/etc/ssh/ssh_config");
    let rules = vec![
        PathAccessRule::new(
            "/etc",
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            source,
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )
        .with_review_required(),
    ];
    let runner =
        BwrapProcessRunner::new_at_workspace_root(directory.path()).with_path_rules(rules.clone());
    let before = execute(&runner, "cat /etc/ssh/ssh_config").await;
    assert_eq!(before.stdout_text(), "sandbox-started\n");
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(directory.path())
        .with_path_rules(rules);
    let script = "cat /etc/ssh/ssh_config";
    let call = PendingToolCall::new(
        ToolCallId::new("ssh-config-review").unwrap(),
        ToolName::new("request_permissions").unwrap(),
        ToolCallArguments::try_from(serde_json::json!({
            "requested": {"paths": [{"path": source, "access": "ro"}]},
            "for_action": {"command": script, "cwd": null},
        }))
        .unwrap(),
    );
    let request = merry_runtime::parse_permission_request(&call).unwrap();
    factory.validate_request(&request).unwrap();
    let approved = execute(factory.runner_for(&request).as_ref(), script).await;
    assert!(approved.ok(), "{approved:?}");
    assert_eq!(
        approved.stdout_text(),
        format!("sandbox-started\n{}", fs::read_to_string(source).unwrap())
    );
    let after = execute(&runner, script).await;
    assert_eq!(after.stdout_text(), "sandbox-started\n");
}
