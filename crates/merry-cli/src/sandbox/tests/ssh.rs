use super::sandbox_host;
use crate::{
    coding::ProcessExecutionMode,
    config::{MerryConfig, XdgPaths},
    runtime_config::prepared_action_process_backend_options,
    sandbox::{Bootstrap, ClipboardAccess, host::current_process_uid, os, plan_bootstrap},
};
use merry_process::{LocalProcessBackend, ProcessBackend, ProcessBackendMode};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, ProcessActionIntent, ProcessEnvPolicy,
    ProcessRunnerContext,
};
use std::{env, ffi::OsStr, fs, process::Command};
use tokio_util::sync::CancellationToken;

const CHILD_ENV: &str = "MERRY_SSH_CONFIG_TEST_CHILD";
const HOST_KEY: &str = "sandbox.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";

#[test]
fn configured_etc_and_known_hosts_work_through_both_sandboxes() {
    if env::var_os(CHILD_ENV).as_deref() == Some(OsStr::new("1")) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(assert_ssh_client());
        return;
    }
    let directory = tempfile::Builder::new()
        .prefix("merry-ssh-test-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let home = directory.path().join("home");
    let ssh = home.join(".ssh");
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&ssh).unwrap();
    fs::create_dir(&workspace).unwrap();
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
        "[permissions]\nssh_agent = true\nreadonly_paths = [\"/etc\"]\n",
    )
    .unwrap();
    let config = MerryConfig::load_optional(&host.xdg_paths)
        .unwrap()
        .unwrap();
    host.host_integrations = config.host_integrations();
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
    plan.args.extend([
        os("--setenv"),
        os(CHILD_ENV),
        os("1"),
        host.current_exe.into_os_string(),
        os("--exact"),
        os("sandbox::tests::ssh::configured_etc_and_known_hosts_work_through_both_sandboxes"),
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
    let runner = backend.new_session().runner();
    let intent = ProcessActionIntent::new(vec!["/bin/sh".into(), "-eu".into(), "-c".into(), r#"
        test ! -w /etc/ssh/ssh_config
        test ! -w "$HOME/.ssh/known_hosts"
        test ! -w "$HOME/.ssh/known_hosts2"
        ssh-keygen -F sandbox.example -f "$HOME/.ssh/known_hosts"
        ssh-keygen -F sandbox.example -f "$HOME/.ssh/known_hosts2"
        ssh -G -F /etc/ssh/ssh_config -o StrictHostKeyChecking=yes -o Hostname=127.0.0.1 sandbox.example
    "#.into()], None, ProcessEnvPolicy::empty(), None, 16384, 16384).unwrap();
    let output = runner
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
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
}
