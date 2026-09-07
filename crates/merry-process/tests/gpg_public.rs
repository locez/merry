#![cfg(target_os = "linux")]

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
use std::{fs, os::unix::fs::symlink, path::PathBuf};
use tokio_util::sync::CancellationToken;

const PUBLIC_KEYRING: &str = "public keyring sentinel\n";
const READ_KEYBOX: &str = "cat \"$GNUPGHOME/pubring.kbx\"";

struct Fixture {
    directory: tempfile::TempDir,
    home: PathBuf,
    ring: PathBuf,
}

impl Fixture {
    fn new(legacy: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("custom-keyring");
        let workspace = directory.path().join("workspace");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&workspace).unwrap();
        let ring = home.join(if legacy { "pubring.gpg" } else { "pubring.kbx" });
        fs::write(&ring, PUBLIC_KEYRING).unwrap();
        fs::write(home.join("trustdb.gpg"), "host trust database sentinel").unwrap();
        fs::write(home.join("gpg.conf"), "invalid-host-only-option\n").unwrap();
        fs::create_dir(home.join("private-keys-v1.d")).unwrap();
        fs::write(home.join("private-keys-v1.d/sentinel"), "not a private key").unwrap();
        fs::write(home.join("secring.gpg"), "not a private key").unwrap();
        Self {
            directory,
            home,
            ring,
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
        assert_eq!(fs::read_to_string(&self.ring).unwrap(), PUBLIC_KEYRING);
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
async fn gpg_keyrings_are_readonly_and_client_writes_stay_private() {
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
            let script = if legacy {
                "cat \"$GNUPGHOME/pubring.gpg\""
            } else {
                READ_KEYBOX
            };
            let visible = execute(&runner, script).await;
            assert!(visible.ok(), "{visible:?}");
            assert_eq!(
                visible.stdout_text(),
                format!("sandbox-started\n{PUBLIC_KEYRING}")
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
    }
}

#[tokio::test]
async fn gpg_keyring_visibility_respects_deny_and_per_action_review() {
    let fixture = Fixture::new(false);
    for target in [&fixture.home, &fixture.ring] {
        let denied = fixture.runner(vec![PathAccessRule::new(
            target,
            PathAccess::Deny,
            PathAccessRuleSource::TrustedGlobalConfig,
        )]);
        let output = execute(&denied, READ_KEYBOX).await;
        assert!(!output.stdout_text().contains(PUBLIC_KEYRING), "{output:?}");
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
                "for_action": {"command": READ_KEYBOX, "cwd": null}
            }))
            .unwrap(),
        );
        let request = merry_runtime::parse_permission_request(&call).unwrap();
        factory.validate_request(&request).unwrap();
        let before = execute(&runner, READ_KEYBOX).await;
        assert!(!before.stdout_text().contains(PUBLIC_KEYRING), "{before:?}");
        let approved = execute(factory.runner_for(&request).as_ref(), READ_KEYBOX).await;
        assert!(approved.ok(), "{approved:?}");
        assert!(
            approved.stdout_text().contains(PUBLIC_KEYRING),
            "{approved:?}"
        );
        let after = execute(&runner, READ_KEYBOX).await;
        assert!(!after.stdout_text().contains(PUBLIC_KEYRING), "{after:?}");
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
    let output = execute(&runner, READ_KEYBOX).await;
    assert!(!output.stdout_text().contains(PUBLIC_KEYRING), "{output:?}");
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
            intent(READ_KEYBOX),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("keyboxd"), "{error}");
}
