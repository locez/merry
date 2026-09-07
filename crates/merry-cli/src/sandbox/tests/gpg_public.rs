use super::sandbox_host;
use crate::{
    coding::ProcessExecutionMode,
    config::{MerryConfig, XdgPaths},
    runtime_config::prepared_action_process_backend_options,
    sandbox::{Bootstrap, ClipboardAccess, host::current_process_uid, os, plan_bootstrap},
};
use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
use merry_process::{GpgAgentSockets, LocalProcessBackend, ProcessBackend, ProcessBackendMode};
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedAction,
    ProcessRunnerContext, parse_permission_request,
};
use std::{env, ffi::OsStr, fs, os::unix::net::UnixListener, process::Command};
use tokio_util::sync::CancellationToken;

const CHILD_ENV: &str = "MERRY_GPG_PUBLIC_TEST_CHILD";
const PUBLIC_IDENTITY: &str = "Merry Sandbox Fixture <fixture@example.invalid>";

#[test]
fn gpg_agent_config_requires_review_through_both_sandboxes() {
    if env::var_os(CHILD_ENV).as_deref() == Some(OsStr::new("1")) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(assert_public_key_client());
        return;
    }
    let directory = tempfile::Builder::new()
        .prefix("merry-gpg-public-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let root = directory.path();
    let home = root.join("home");
    let keyring = home.join(".gnupg");
    let workspace = root.join("workspace");
    fs::create_dir_all(&keyring).unwrap();
    fs::create_dir(&workspace).unwrap();
    let agent = keyring.join("S.gpg-agent");
    let _agent = UnixListener::bind(&agent).unwrap();
    let original = public_keybox();
    fs::write(keyring.join("pubring.kbx"), &original).unwrap();
    fs::write(keyring.join("trustdb.gpg"), "host trust database sentinel").unwrap();
    fs::write(keyring.join("gpg.conf"), "invalid-host-only-option\n").unwrap();
    fs::create_dir(keyring.join("private-keys-v1.d")).unwrap();
    fs::write(
        keyring.join("private-keys-v1.d/sentinel"),
        "not a private key",
    )
    .unwrap();
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
        "[permissions]\ngpg_agent = true\n",
    )
    .unwrap();
    let config = MerryConfig::load_optional(&host.xdg_paths)
        .unwrap()
        .unwrap();
    host.host_integrations = config.host_integrations();
    assert_eq!(host.host_integrations, vec![HostIntegration::GpgAgent]);
    host.host_integration_environment.gpg_agent_sockets =
        Some(GpgAgentSockets::new(&keyring, agent).unwrap());
    host.trusted_path_rules = vec![PathAccessRule::new(
        &host.current_exe,
        PathAccess::ReadOnly,
        PathAccessRuleSource::TrustedGlobalConfig,
    )];
    let Bootstrap::Reexec(mut plan) =
        plan_bootstrap(true, ClipboardAccess::Disabled, &host).unwrap()
    else {
        panic!("expected outer sandbox");
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
        os("sandbox::tests::gpg_public::gpg_agent_config_requires_review_through_both_sandboxes"),
        os("--nocapture"),
    ]);
    let output = Command::new(plan.program)
        .args(plan.args)
        .env_clear()
        .envs(plan.env)
        .output()
        .expect("bubblewrap test dependency");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(keyring.join("pubring.kbx")).unwrap(), original);
    assert_eq!(
        fs::read_to_string(keyring.join("trustdb.gpg")).unwrap(),
        "host trust database sentinel"
    );
}

async fn assert_public_key_client() {
    let paths = XdgPaths::from_env().unwrap();
    let keyring = paths.home().join(".gnupg");
    assert!(keyring.join("pubring.kbx").is_file());
    for excluded in ["trustdb.gpg", "gpg.conf", "private-keys-v1.d"] {
        assert!(!keyring.join(excluded).exists());
    }
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
    let request = parse_permission_request(&PendingToolCall::new(
        ToolCallId::new("gpg-public-review").unwrap(),
        ToolName::new("request_permissions").unwrap(),
        ToolCallArguments::try_from(serde_json::json!({
            "requested": {"host_integrations": ["gpg-agent"]},
            "for_action": {"command": "gpg --batch --no-autostart --with-colons --list-keys", "cwd": null}
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
    assert!(
        !before.stdout_text().contains(PUBLIC_IDENTITY),
        "{before:?}"
    );
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
    assert!(output.stdout_text().contains(PUBLIC_IDENTITY), "{output:?}");
    assert!(!keyring.join("trustdb.gpg").exists());
    assert!(
        !backend
            .new_session()
            .permissioned_factory()
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
}

/// Returns only an anonymous public keybox; private keys stay on disposable tmpfs.
fn public_keybox() -> Vec<u8> {
    let script = r#"
        mkdir -m 700 "$GNUPGHOME"
        gpg --no-options --batch --pinentry-mode loopback --passphrase '' \
            --quick-generate-key "$MERRY_GPG_TEST_IDENTITY" ed25519 sign 0
        cat "$GNUPGHOME/pubring.kbx"
    "#;
    let output = Command::new("timeout")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args([
            "--kill-after=2s",
            "20s",
            "bwrap",
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            "/run",
            "--tmpfs",
            "/etc",
            "--ro-bind-try",
            "/etc/ld.so.cache",
            "/etc/ld.so.cache",
            "--tmpfs",
            "/home",
            "--tmpfs",
            "/root",
            "--clearenv",
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "HOME",
            "/tmp",
            "--setenv",
            "GNUPGHOME",
            "/tmp/fixture-keyring",
            "--setenv",
            "MERRY_GPG_TEST_IDENTITY",
            PUBLIC_IDENTITY,
            "--",
            "/bin/sh",
            "-eu",
            "-c",
            script,
        ])
        .output()
        .expect("isolated GnuPG test dependency");
    assert!(
        output.status.success(),
        "public keybox generation failed: {output:?}"
    );
    output.stdout
}
