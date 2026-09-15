use super::{assert_sandbox_child_ran, integration_host, reentry};
use crate::{
    coding::ProcessExecutionMode,
    config::{MerryConfig, XdgPaths},
    runtime_config::prepared_action_process_backend_options,
    sandbox::{Bootstrap, ClipboardAccess, plan_bootstrap},
};
use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
use merry_process::{GpgAgentSockets, LocalProcessBackend, ProcessBackend, ProcessBackendMode};
use merry_runtime::{
    HostIntegration, PermissionedAction, ProcessRunnerContext, parse_permission_request,
};
use std::{env, fs, os::unix::net::UnixListener, process::Command};
use tokio_util::sync::CancellationToken;

const CHILD_MARKER: &str = "MERRY_GPG_PUBLIC_TEST_CHILD";
const CHILD_TEST: &str = "sandbox::tests::gpg_public::gpg_agent_config_preauthorizes_inner_actions_through_both_sandboxes";
const PUBLIC_IDENTITY: &str = "Merry Sandbox Fixture <fixture@example.invalid>";

#[test]
fn gpg_agent_config_preauthorizes_inner_actions_through_both_sandboxes() {
    if reentry::is_child(CHILD_MARKER) {
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
    let mut host = integration_host(&home, &workspace, "[permissions]\ngpg_agent = true\n");
    assert_eq!(host.host_integrations, vec![HostIntegration::GpgAgent]);
    host.host_integration_environment.gpg_agent_sockets =
        Some(GpgAgentSockets::new(&keyring, agent).unwrap());
    let Bootstrap::Reexec(mut plan) =
        plan_bootstrap(true, ClipboardAccess::Disabled, &host).unwrap()
    else {
        panic!("expected outer sandbox");
    };
    reentry::truncate_before_command(&mut plan, &host.current_exe);
    assert_sandbox_child_ran(&mut plan, &host, CHILD_MARKER, CHILD_TEST);
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
        factory
            .request_capabilities_are_satisfied(&request)
            .unwrap(),
        "trusted config must preauthorize the configured gpg agent"
    );
    // No permission request is needed: the configured agent and its public
    // keyring are already part of the inner action baseline.
    let output = session
        .runner()
        .run(
            intent.clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(output.ok(), "{output:?}");
    assert!(output.stdout_text().contains(PUBLIC_IDENTITY), "{output:?}");
    assert!(!keyring.join("trustdb.gpg").exists());
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
