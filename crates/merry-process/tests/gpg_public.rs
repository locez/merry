#![cfg(target_os = "linux")]

#[path = "support/gpg_fixture.rs"]
mod gpg_fixture;

use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
use merry_process::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessEnvironment, BwrapProcessRunner,
    GpgAgentSockets,
};
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource,
    PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessEnvPolicy, ProcessRunner,
    ProcessRunnerContext, ProcessRunnerOutput,
};
use std::{fs, os::unix::fs::symlink, path::PathBuf, process::Command};
use tokio_util::sync::CancellationToken;

const LIST_KEYS: &str = "gpg --batch --no-autostart --with-colons --list-keys";
const VERIFY: &str = "gpg --batch --no-autostart --status-fd 1 --verify message.asc message.txt";

struct Fixture {
    directory: tempfile::TempDir,
    home: PathBuf,
    ring: PathBuf,
    original_ring: Vec<u8>,
    fingerprint: String,
}

impl Fixture {
    fn new(legacy: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("custom-keyring");
        let workspace = directory.path().join("workspace");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::write(home.join("common.conf"), "").unwrap();
        let public = workspace.join("public.asc");
        let fingerprint = gpg_fixture::generate(&workspace);
        let ring = home.join(if legacy { "pubring.gpg" } else { "pubring.kbx" });
        let mut command = Command::new("gpg");
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", directory.path())
            .args(["--no-options", "--batch", "--no-autostart", "--homedir"])
            .arg(&home);
        if legacy {
            command.arg("--output").arg(&ring).arg("--dearmor");
        } else {
            command.arg("--import");
        }
        let output = command.arg(public).output().expect("GnuPG test dependency");
        assert!(output.status.success(), "{output:?}");
        fs::write(home.join("trustdb.gpg"), "host trust database sentinel").unwrap();
        fs::write(home.join("gpg.conf"), "invalid-host-only-option\n").unwrap();
        fs::create_dir(home.join("private-keys-v1.d")).unwrap();
        fs::write(home.join("private-keys-v1.d/sentinel"), "not a private key").unwrap();
        fs::write(home.join("secring.gpg"), "not a private key").unwrap();
        let original_ring = fs::read(&ring).unwrap();
        Self {
            directory,
            home,
            ring,
            original_ring,
            fingerprint,
        }
    }

    fn environment(&self) -> BwrapProcessEnvironment {
        BwrapProcessEnvironment::new("/usr/bin:/bin", self.directory.path(), "/tmp")
            .unwrap()
            .with_gpg_agent_sockets(
                GpgAgentSockets::new(&self.home, self.home.join("S.gpg-agent")).unwrap(),
            )
            .with_host_integrations([HostIntegration::GpgAgent])
    }

    fn runner(&self, rules: Vec<PathAccessRule>) -> BwrapProcessRunner {
        BwrapProcessRunner::new_at_workspace_root(self.directory.path().join("workspace"))
            .with_environment(self.environment())
            .with_path_rules(rules)
    }

    fn assert_host_unchanged(&self) {
        assert_eq!(fs::read(&self.ring).unwrap(), self.original_ring);
        assert_eq!(
            fs::read_to_string(self.home.join("trustdb.gpg")).unwrap(),
            "host trust database sentinel"
        );
        assert!(!self.home.join("client-write").exists());
    }
}

async fn execute(runner: &dyn ProcessRunner, script: &str) -> ProcessRunnerOutput {
    let output = runner
        .run(
            intent(&format!("printf 'sandbox-started\\n'; {script}")),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("sandbox execution");
    assert!(
        output.stdout_text().contains("sandbox-started"),
        "{output:?}"
    );
    output
}

fn intent(script: &str) -> ProcessActionIntent {
    ProcessActionIntent::new(
        vec!["/bin/sh".into(), "-eu".into(), "-c".into(), script.into()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        16384,
        16384,
    )
    .unwrap()
}

#[tokio::test]
async fn gpg_public_keys_and_verification_work_without_agent_or_path_grants() {
    for legacy in [false, true] {
        let fixture = Fixture::new(legacy);
        for readonly_home in [false, true] {
            let rules = if readonly_home {
                vec![PathAccessRule::new(
                    &fixture.home,
                    PathAccess::ReadOnly,
                    PathAccessRuleSource::TrustedGlobalConfig,
                )]
            } else {
                Vec::new()
            };
            let runner = fixture.runner(rules);
            let listed = execute(&runner, LIST_KEYS).await;
            assert!(listed.ok(), "{listed:?}");
            assert!(
                listed.stdout_text().contains(&fixture.fingerprint),
                "{listed:?}"
            );
            let verified = execute(&runner, VERIFY).await;
            assert!(verified.ok(), "{verified:?}");
            assert!(
                verified
                    .stdout_text()
                    .contains(&format!("VALIDSIG {}", fixture.fingerprint)),
                "{verified:?}"
            );
            let output = execute(
                &runner,
                r#"
                test ! -e "$GNUPGHOME/trustdb.gpg"
                test ! -e "$GNUPGHOME/gpg.conf"
                test ! -e "$GNUPGHOME/private-keys-v1.d"
                test ! -e "$GNUPGHOME/secring.gpg"
                test ! -e "$GNUPGHOME/client-write"
                printf local > "$GNUPGHOME/client-write"
                for ring in "$GNUPGHOME"/pubring.*; do
                    if printf overwrite > "$ring"; then exit 1; fi
                done
                "#,
            )
            .await;
            assert!(output.ok(), "{output:?}");
            fixture.assert_host_unchanged();
        }
        fs::write(
            fixture.directory.path().join("workspace/message.txt"),
            "tampered",
        )
        .unwrap();
        let output = execute(&fixture.runner(Vec::new()), VERIFY).await;
        assert!(!output.ok(), "{output:?}");
        assert!(output.stdout_text().contains("BADSIG"), "{output:?}");
    }
}

#[tokio::test]
async fn gpg_public_key_import_respects_deny_and_per_action_review() {
    let fixture = Fixture::new(false);
    for target in [&fixture.home, &fixture.ring] {
        let denied = fixture.runner(vec![PathAccessRule::new(
            target,
            PathAccess::Deny,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
        let output = execute(&denied, LIST_KEYS).await;
        assert!(
            !output.stdout_text().contains(&fixture.fingerprint),
            "{output:?}"
        );
        let rules = vec![
            PathAccessRule::new(
                &fixture.home,
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
            PathAccessRule::new(
                target,
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            )
            .with_review_required(),
        ];
        let runner = fixture.runner(rules.clone());
        let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(
            fixture.directory.path().join("workspace"),
        )
        .with_environment(fixture.environment())
        .with_path_rules(rules);
        let call = PendingToolCall::new(
            ToolCallId::new("public-key-review").unwrap(),
            ToolName::new("request_permissions").unwrap(),
            ToolCallArguments::try_from(serde_json::json!({
                "requested": {"paths": [{"path": fixture.ring, "access": "ro"}]},
                "for_action": {"command": LIST_KEYS, "cwd": null}
            }))
            .unwrap(),
        );
        let request = merry_runtime::parse_permission_request(&call).unwrap();
        factory.validate_request(&request).unwrap();
        let before = execute(&runner, LIST_KEYS).await;
        assert!(
            !before.stdout_text().contains(&fixture.fingerprint),
            "{before:?}"
        );
        let approved = execute(factory.runner_for(&request).as_ref(), LIST_KEYS).await;
        assert!(approved.ok(), "{approved:?}");
        assert!(
            approved.stdout_text().contains(&fixture.fingerprint),
            "{approved:?}"
        );
        let after = execute(&runner, LIST_KEYS).await;
        assert!(
            !after.stdout_text().contains(&fixture.fingerprint),
            "{after:?}"
        );
    }
    fixture.assert_host_unchanged();
}

#[tokio::test]
async fn symlinked_public_key_source_cannot_bypass_a_path_deny() {
    let fixture = Fixture::new(false);
    let source = fixture.directory.path().join("protected.kbx");
    fs::rename(&fixture.ring, &source).unwrap();
    symlink(&source, &fixture.ring).unwrap();
    let runner = fixture.runner(vec![PathAccessRule::new(
        source,
        PathAccess::Deny,
        PathAccessRuleSource::TrustedGlobalConfig,
    )]);
    let output = execute(&runner, LIST_KEYS).await;
    assert!(
        !output.stdout_text().contains(&fixture.fingerprint),
        "{output:?}"
    );
    fixture.assert_host_unchanged();
}

#[tokio::test]
async fn keyboxd_is_reported_instead_of_importing_a_host_write_interface() {
    let fixture = Fixture::new(false);
    fs::create_dir(fixture.home.join("public-keys.d")).unwrap();
    fs::write(
        fixture.home.join("public-keys.d/pubring.db"),
        "database sentinel",
    )
    .unwrap();
    let error = fixture
        .runner(Vec::new())
        .run(
            intent(LIST_KEYS),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("keyboxd"), "{error}");
}
