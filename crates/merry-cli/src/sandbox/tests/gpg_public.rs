use super::sandbox_host;
use crate::{
    coding::ProcessExecutionMode,
    config::{MerryConfig, XdgPaths},
    runtime_config::prepared_action_process_backend_options,
    sandbox::{Bootstrap, ClipboardAccess, host::current_process_uid, os, plan_bootstrap},
};
use merry_process::{GpgAgentSockets, LocalProcessBackend, ProcessBackend, ProcessBackendMode};
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource, ProcessActionIntent,
    ProcessEnvPolicy, ProcessRunnerContext,
};
use std::{env, ffi::OsStr, fs, process::Command};
use tokio_util::sync::CancellationToken;

#[path = "../../../../merry-process/tests/support/gpg_fixture.rs"]
mod gpg_fixture;

const CHILD_ENV: &str = "MERRY_GPG_PUBLIC_TEST_CHILD";

#[test]
fn gpg_agent_config_provides_public_keys_through_outer_and_inner_sandboxes() {
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
    gpg_fixture::generate(&workspace);
    fs::write(keyring.join("common.conf"), "").unwrap();
    let prepared = Command::new("gpg")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .args(["--no-options", "--batch", "--no-autostart", "--homedir"])
        .arg(&keyring)
        .arg("--import")
        .arg(workspace.join("public.asc"))
        .output()
        .expect("GnuPG test dependency");
    assert!(prepared.status.success(), "{prepared:?}");
    fs::write(keyring.join("trustdb.gpg"), "host trust database sentinel").unwrap();
    fs::write(keyring.join("gpg.conf"), "invalid-host-only-option\n").unwrap();
    fs::create_dir(keyring.join("private-keys-v1.d")).unwrap();
    fs::write(
        keyring.join("private-keys-v1.d/sentinel"),
        "not a private key",
    )
    .unwrap();
    let original = fs::read(keyring.join("pubring.kbx")).unwrap();

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
        Some(GpgAgentSockets::new(&keyring, keyring.join("S.gpg-agent")).unwrap());
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
        os("sandbox::tests::gpg_public::gpg_agent_config_provides_public_keys_through_outer_and_inner_sandboxes"),
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
    let fingerprint = fs::read_to_string("fingerprint.txt").unwrap();
    let fingerprint = fingerprint.trim();
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
    let runner = backend.new_session().runner();
    for (script, evidence) in [
        (
            "gpg --batch --no-autostart --with-colons --list-keys",
            fingerprint.to_owned(),
        ),
        (
            "gpg --batch --no-autostart --status-fd 1 --verify message.asc message.txt",
            format!("VALIDSIG {fingerprint}"),
        ),
    ] {
        let intent = ProcessActionIntent::new(
            vec!["/bin/sh".into(), "-eu".into(), "-c".into(), script.into()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            16384,
            16384,
        )
        .unwrap();
        let output = runner
            .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
            .await
            .unwrap();
        assert!(output.ok(), "{output:?}");
        assert!(output.stdout_text().contains(&evidence), "{output:?}");
    }
    assert!(!keyring.join("trustdb.gpg").exists());
}
