use super::{path_review::execute, permission_request};
use crate::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessEnvironment, BwrapProcessRunner,
    GpgAgentSockets,
};
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource,
    PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessEnvPolicy, ProcessRunner,
    ProcessRunnerContext,
};
use std::{
    fs,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

struct Fixture {
    directory: tempfile::TempDir,
    ssh: PathBuf,
    native: PathBuf,
    extra: PathBuf,
    _listeners: Vec<UnixListener>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let sockets = directory.path().join("shared");
        fs::create_dir(&sockets).unwrap();
        fs::create_dir(directory.path().join("workspace")).unwrap();
        fs::create_dir(directory.path().join("keyring")).unwrap();
        fs::write(sockets.join("unrelated"), "service metadata").unwrap();
        let ssh = sockets.join("S.gpg-agent.ssh");
        let native = sockets.join("S.gpg-agent");
        let extra = sockets.join("S.gpg-agent.extra");
        let listeners = [&ssh, &native, &extra]
            .into_iter()
            .map(|path| UnixListener::bind(path).unwrap())
            .collect();
        Self {
            directory,
            ssh,
            native,
            extra,
            _listeners: listeners,
        }
    }

    fn environment(&self, integrations: &[HostIntegration]) -> BwrapProcessEnvironment {
        self.environment_with_tmp(integrations, Path::new("/tmp"))
    }

    fn environment_with_tmp(
        &self,
        integrations: &[HostIntegration],
        tmp_source: &Path,
    ) -> BwrapProcessEnvironment {
        let sockets = GpgAgentSockets::new(self.directory.path().join("keyring"), &self.native)
            .unwrap()
            .with_auxiliary_sockets([self.ssh.clone(), self.extra.clone()])
            .unwrap();
        BwrapProcessEnvironment::new("/usr/bin:/bin", self.directory.path(), tmp_source)
            .unwrap()
            .with_ssh_agent_socket(&self.ssh)
            .unwrap()
            .with_gpg_agent_sockets(sockets)
            .with_host_integrations(integrations.iter().copied())
    }
}

const CLIENT: &str = r#"
import pathlib, socket, sys
for index in range(1, len(sys.argv), 2):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(2)
        try:
            connection.connect(sys.argv[index])
        except OSError:
            connected = False
        else:
            connected = True
        if connected != (sys.argv[index + 1] == 'allowed'):
            raise SystemExit('unexpected socket access: ' + sys.argv[index])
assert pathlib.Path.home().joinpath('shared/unrelated').read_text() == 'service metadata'
"#;

#[tokio::test]
async fn private_gpg_client_keeps_native_socket_but_not_keyboxd_or_private_files() {
    let fixture = Fixture::new();
    let home = fixture.directory.path().join("keyring");
    let native = home.join("S.gpg-agent");
    let keyboxd = home.join("S.keyboxd");
    let _native_listener = UnixListener::bind(&native).unwrap();
    let _keyboxd_listener = UnixListener::bind(&keyboxd).unwrap();
    fs::create_dir(home.join("private-keys-v1.d")).unwrap();
    fs::write(home.join("private-keys-v1.d/sentinel"), "not a private key").unwrap();
    let environment = fixture
        .environment(&[HostIntegration::GpgAgent])
        .with_gpg_agent_sockets(
            GpgAgentSockets::new(&home, &native)
                .unwrap()
                .with_auxiliary_sockets([keyboxd.clone()])
                .unwrap(),
        );
    let runner =
        BwrapProcessRunner::new_at_workspace_root(fixture.directory.path().join("workspace"))
            .with_environment(environment)
            .with_path_rules([PathAccessRule::new(
                home,
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            )]);
    let script = r#"
        set -eu
        test ! -e "$GNUPGHOME/private-keys-v1.d"
        /usr/bin/python3 -c 'import os, socket
home = os.environ["GNUPGHOME"]
with socket.socket(socket.AF_UNIX) as connection:
    connection.settimeout(2)
    connection.connect(home + "/S.gpg-agent")
with socket.socket(socket.AF_UNIX) as connection:
    connection.settimeout(2)
    try:
        connection.connect(home + "/S.keyboxd")
    except OSError:
        pass
    else:
        raise SystemExit("unexpected host keyboxd access")'
    "#;
    let output = execute(&runner, script).await;
    assert!(output.ok(), "{output:?}");
}

#[tokio::test]
async fn native_gpg_and_ssh_are_independent_even_under_tmp_and_readonly_parent_mounts() {
    let fixture = Fixture::new();
    for (integrations, ssh, native, remapped_tmp) in [
        (vec![], "blocked", "blocked", false),
        (vec![], "blocked", "blocked", true),
        (vec![HostIntegration::SshAgent], "allowed", "blocked", false),
        (vec![HostIntegration::GpgAgent], "blocked", "allowed", false),
        (
            vec![HostIntegration::SshAgent, HostIntegration::GpgAgent],
            "allowed",
            "allowed",
            false,
        ),
    ] {
        let environment = fixture.environment_with_tmp(
            &integrations,
            if remapped_tmp {
                fixture.directory.path()
            } else {
                Path::new("/tmp")
            },
        );
        let runner =
            BwrapProcessRunner::new_at_workspace_root(fixture.directory.path().join("workspace"))
                .with_environment(environment)
                .with_path_rules([PathAccessRule::new(
                    fixture.directory.path().join("shared"),
                    PathAccess::ReadOnly,
                    PathAccessRuleSource::TrustedGlobalConfig,
                )]);
        let intent = ProcessActionIntent::new(
            vec![
                "/usr/bin/python3".into(),
                "-c".into(),
                CLIENT.into(),
                fixture.ssh.to_str().unwrap().into(),
                ssh.into(),
                if remapped_tmp {
                    "/tmp/shared/S.gpg-agent".into()
                } else {
                    fixture.native.to_str().unwrap().into()
                },
                native.into(),
                fixture.extra.to_str().unwrap().into(),
                "blocked".into(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            4096,
        )
        .unwrap();
        let output = runner
            .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
            .await
            .unwrap();
        assert!(output.ok(), "{integrations:?}: {output:?}");
    }
}

#[tokio::test]
async fn configured_agent_rejects_a_regular_file_instead_of_a_socket() {
    let fixture = Fixture::new();
    let invalid = fixture.directory.path().join("shared/unrelated");
    let environment = fixture
        .environment(&[HostIntegration::SshAgent])
        .with_ssh_agent_socket(invalid)
        .unwrap();
    let runner =
        BwrapProcessRunner::new_at_workspace_root(fixture.directory.path().join("workspace"))
            .with_environment(environment.clone());
    assert!(
        execute(&runner, "test -z \"${SSH_AUTH_SOCK:-}\"")
            .await
            .ok()
    );
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(
        fixture.directory.path().join("workspace"),
    )
    .with_environment(environment);
    let request = permission_request(serde_json::json!({
        "requested": { "host_integrations": ["ssh-agent"] },
        "for_action": { "command": "true", "cwd": null }
    }));
    assert!(factory.validate_request(&request).is_err());
}

#[tokio::test]
async fn reviewed_socket_requires_both_path_and_native_agent_authorization() {
    let fixture = Fixture::new();
    let rules = vec![
        PathAccessRule::new(
            fixture.directory.path().join("shared"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            &fixture.native,
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )
        .with_review_required(),
    ];
    let script = "/usr/bin/python3 -c 'import os, socket; connection = socket.socket(socket.AF_UNIX); connection.settimeout(2); connection.connect(os.environ[\"HOME\"] + \"/shared/S.gpg-agent\")'";
    for (grant_path, grant_agent) in [(false, false), (true, false), (false, true), (true, true)] {
        let environment = fixture.environment(if grant_agent {
            &[HostIntegration::GpgAgent]
        } else {
            &[]
        });
        let runner: std::sync::Arc<dyn ProcessRunner> = if grant_path {
            let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(
                fixture.directory.path().join("workspace"),
            )
            .with_environment(environment)
            .with_path_rules(rules.clone());
            let request = permission_request(serde_json::json!({
                "requested": { "paths": [{ "path": fixture.native, "access": "ro" }] },
                "for_action": { "command": script, "cwd": null }
            }));
            factory.validate_request(&request).unwrap();
            factory.runner_for(&request)
        } else {
            std::sync::Arc::new(
                BwrapProcessRunner::new_at_workspace_root(
                    fixture.directory.path().join("workspace"),
                )
                .with_environment(environment)
                .with_path_rules(rules.clone()),
            )
        };
        let output = execute(runner.as_ref(), script).await;
        assert_eq!(output.ok(), grant_path && grant_agent, "{output:?}");
    }
}
