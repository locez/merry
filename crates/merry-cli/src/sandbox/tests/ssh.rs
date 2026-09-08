use super::sandbox_host;
use crate::{
    coding::ProcessExecutionMode,
    config::{MerryConfig, XdgPaths},
    runtime_config::prepared_action_process_backend_options,
    sandbox::{Bootstrap, ClipboardAccess, host::current_process_uid, os, plan_bootstrap},
};
use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
use merry_process::{LocalProcessBackend, ProcessBackend, ProcessBackendMode};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedAction, ProcessRunnerContext,
    parse_permission_request,
};
use std::{env, ffi::OsStr, fs, os::unix::net::UnixListener, process::Command};
use tokio_util::sync::CancellationToken;

const CHILD_ENV: &str = "MERRY_SSH_CONFIG_TEST_CHILD";
const HOST_KEY: &str = "sandbox.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";

#[test]
fn ssh_agent_config_requires_review_through_both_sandboxes() {
    if env::var_os(CHILD_ENV).as_deref() == Some(OsStr::new("1")) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(assert_ssh_client());
        return;
    }
    for expose_etc in [false, true] {
        assert_ssh_sandboxes(expose_etc);
    }
}

fn assert_ssh_sandboxes(expose_etc: bool) {
    let directory = tempfile::Builder::new()
        .prefix("merry-ssh-test-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let home = directory.path().join("home");
    let ssh = home.join(".ssh");
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&ssh).unwrap();
    fs::create_dir(&workspace).unwrap();
    let agent = ssh.join("agent.sock");
    let _agent = UnixListener::bind(&agent).unwrap();
    fs::write(ssh.join("known_hosts"), HOST_KEY).unwrap();
    fs::write(ssh.join("known_hosts2"), HOST_KEY).unwrap();
    fs::write(ssh.join("id_ed25519"), "not a private key").unwrap();
    fs::write(ssh.join("config"), "invalid-host-only-config").unwrap();
    let original = fs::read("/etc/ssh/ssh_config").expect("openssh-client configuration");
    let original_metadata = fs::metadata("/etc/ssh/ssh_config").unwrap();
    let mut host = sandbox_host();
    host.cwd = workspace;
    host.current_exe = env::current_exe().unwrap();
    host.path = Some(os("/usr/bin:/bin"));
    host.args.clear();
    host.current_uid = current_process_uid().unwrap();
    host.xdg_paths = XdgPaths::from_parts(home, None, None);
    fs::create_dir_all(host.xdg_paths.config_dir()).unwrap();
    fs::write(
        host.xdg_paths.config_file(),
        if expose_etc {
            "[permissions]\nssh_agent = true\nreadonly_paths = [\"/etc\"]\n"
        } else {
            "[permissions]\nssh_agent = true\n"
        },
    )
    .unwrap();
    let config = MerryConfig::load_optional(&host.xdg_paths)
        .unwrap()
        .unwrap();
    host.host_integrations = config.host_integrations();
    host.host_integration_environment.ssh_agent_socket = Some(agent);
    host.trusted_path_rules = config.trusted_global_path_rules().unwrap();
    host.trusted_path_rules.push(PathAccessRule::new(
        &host.current_exe,
        PathAccess::ReadOnly,
        PathAccessRuleSource::TrustedGlobalConfig,
    ));
    let Bootstrap::Reexec(mut plan) =
        plan_bootstrap(true, ClipboardAccess::Disabled, &host).unwrap()
    else {
        panic!("outer sandbox plan");
    };
    let command_index = plan
        .args
        .iter()
        .rposition(|argument| argument == host.current_exe.as_os_str())
        .unwrap();
    plan.args.truncate(command_index);
    assert_ssh_bootstrap(&plan, expose_etc);
    if !expose_etc {
        assert_ssh_bootstrap_restrictions(&host);
    }
    plan.args.extend([
        os("--setenv"),
        os(CHILD_ENV),
        os("1"),
        host.current_exe.into_os_string(),
        os("--exact"),
        os("sandbox::tests::ssh::ssh_agent_config_requires_review_through_both_sandboxes"),
        os("--nocapture"),
    ]);
    let mut command = Command::new(plan.program);
    command.args(plan.args).env_clear().envs(plan.env);
    plan.ssh_config.configure_command(&mut command).unwrap();
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read("/etc/ssh/ssh_config").unwrap(), original);
    assert_eq!(
        fs::metadata("/etc/ssh/ssh_config").unwrap().permissions(),
        original_metadata.permissions()
    );
    assert_eq!(
        fs::read_to_string(ssh.join("known_hosts")).unwrap(),
        HOST_KEY
    );
    assert_eq!(
        fs::read_to_string(ssh.join("known_hosts2")).unwrap(),
        HOST_KEY
    );
}

fn assert_ssh_bootstrap(plan: &crate::sandbox::Plan, expose_etc: bool) {
    let output = bootstrap_output(
        plan,
        r#"
                cat /etc/ssh/ssh_config > /dev/null
                test -S "$SSH_AUTH_SOCK"
                test ! -w /etc/ssh/ssh_config
                test ! -w /etc/passwd
                test ! -w /etc/group
                if test "$1" = false; then
                    test ! -e /etc/ssh/sshd_config
                    test ! -e /etc/ssh/sshd_config.d
                    test ! -e /etc/ssh/ssh_host_ed25519_key
                    test ! -e /etc/shadow
                    test ! -e /etc/gshadow
                fi
                ssh -G -o StrictHostKeyChecking=yes -o Hostname=127.0.0.1 sandbox.example
        "#,
        &[if expose_etc { "true" } else { "false" }],
    );
    assert!(
        output.status.success(),
        "SSH bootstrap (expose_etc={expose_etc}) failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_ssh_bootstrap_restrictions(host: &crate::sandbox::host::Host) {
    for denied in [
        None,
        Some("/etc"),
        Some("/etc/ssh"),
        Some("/etc/ssh/ssh_config"),
    ] {
        let mut host = host.clone();
        if let Some(path) = denied {
            host.trusted_path_rules.push(PathAccessRule::new(
                path,
                PathAccess::Deny,
                PathAccessRuleSource::TrustedGlobalConfig,
            ));
        } else {
            host.host_integrations.clear();
        }
        let Bootstrap::Reexec(mut plan) =
            plan_bootstrap(true, ClipboardAccess::Disabled, &host).unwrap()
        else {
            panic!("expected sandbox reexec plan");
        };
        let command_index = plan
            .args
            .iter()
            .rposition(|argument| argument == host.current_exe.as_os_str())
            .unwrap();
        plan.args.truncate(command_index);
        let output = bootstrap_output(
            &plan,
            r#"
                test ! -s /etc/ssh/ssh_config
                if test "$1" = enabled; then
                    test -S "$SSH_AUTH_SOCK"
                else
                    test -z "${SSH_AUTH_SOCK:-}"
                    test ! -e /etc/ssh/ssh_config.d
                    test ! -e /etc/passwd
                    test ! -e /etc/group
                fi
            "#,
            &[if denied.is_some() {
                "enabled"
            } else {
                "disabled"
            }],
        );
        assert!(
            output.status.success(),
            "SSH bootstrap restrictions ({denied:?}) failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn bootstrap_output(
    plan: &crate::sandbox::Plan,
    script: &str,
    args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .args(["/bin/sh", "-eu", "-c", script, "ssh-bootstrap"])
        .args(args)
        .env_clear()
        .envs(plan.env.iter().cloned());
    plan.ssh_config.configure_command(&mut command).unwrap();
    command.output().unwrap()
}

async fn assert_ssh_client() {
    let paths = XdgPaths::from_env().unwrap();
    let ssh = paths.home().join(".ssh");
    assert!(!ssh.join("id_ed25519").exists());
    assert!(!ssh.join("config").exists());
    let config = MerryConfig::load_optional(&paths).unwrap().unwrap();
    let options =
        prepared_action_process_backend_options(Some(&config), ProcessExecutionMode::OuterAndInner)
            .await
            .unwrap();
    let backend = LocalProcessBackend::new(
        env::current_dir().unwrap(),
        ProcessBackendMode::Isolated,
        options,
    )
    .unwrap();
    let session = backend.new_session();
    let script = r#"
        set -eu
        test -S "$SSH_AUTH_SOCK"
        test ! -w /etc/ssh/ssh_config
        test ! -w "$HOME/.ssh/known_hosts"
        test ! -w "$HOME/.ssh/known_hosts2"
        ssh-keygen -F sandbox.example -f "$HOME/.ssh/known_hosts"
        ssh-keygen -F sandbox.example -f "$HOME/.ssh/known_hosts2"
        ssh -G -F /etc/ssh/ssh_config -o StrictHostKeyChecking=yes -o Hostname=127.0.0.1 sandbox.example
    "#;
    let request = parse_permission_request(&PendingToolCall::new(
        ToolCallId::new("ssh-client-review").unwrap(),
        ToolName::new("request_permissions").unwrap(),
        ToolCallArguments::try_from(serde_json::json!({
            "requested": {"host_integrations": ["ssh-agent"]},
            "for_action": {"command": script, "cwd": null}
        }))
        .unwrap(),
    ))
    .unwrap();
    let PermissionedAction::Process(intent) = request.action();
    let factory = session.permissioned_factory();
    assert!(
        !factory
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
    let before = session
        .runner()
        .run(
            intent.clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(!before.ok(), "{before:?}");
    factory.validate_request(&request).unwrap();
    let output = factory
        .runner_for(&request)
        .run(
            intent.clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(output.ok(), "{output:?}");
    assert!(
        output.stdout_text().contains("sandbox.example ssh-ed25519"),
        "{output:?}"
    );
    assert!(
        output.stdout_text().lines().any(|line| matches!(
            line,
            "stricthostkeychecking true" | "stricthostkeychecking yes"
        )),
        "{output:?}"
    );
    assert!(
        !backend
            .new_session()
            .permissioned_factory()
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
}
