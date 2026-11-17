//! Trusted-config host integration preauthorization.

use crate::process_runner::tests::{permission_request, request_process_intent};
use crate::process_runner::{
    ProcessSessionPermissions, WorkspaceProcessEnvironment, WorkspaceProcessRunner,
    WorkspaceProcessRunnerFactory,
};
use crate::resolve_host_path;
use merry_runtime::{HostIntegration, PermissionedProcessRunnerFactory};
use serde_json::json;

#[test]
#[cfg(unix)]
fn permissioned_factory_preauthorizes_configured_host_integrations() {
    let directory = tempfile::tempdir().unwrap();
    let ssh = directory.path().join("ssh.sock");
    let bus = directory.path().join("bus.sock");
    let _ssh_listener = std::os::unix::net::UnixListener::bind(&ssh).unwrap();
    let _bus_listener = std::os::unix::net::UnixListener::bind(&bus).unwrap();
    let ssh_path = ssh.to_str().unwrap().to_owned();
    let ssh_grant = resolve_host_path(&ssh);
    let ssh_grant = ssh_grant.to_str().unwrap();
    let bus_path = bus.to_str().unwrap().to_owned();
    let bus_grant = resolve_host_path(&bus);
    let bus_grant = bus_grant.to_str().unwrap();
    let mut environment =
        WorkspaceProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
            .expect("environment layout should validate");
    environment.ssh_agent_socket = Some(ssh.clone());
    environment.session_bus_address = Some(format!("unix:path={bus_path}").into());
    let ssh_environment = environment
        .clone()
        .with_host_integrations([HostIntegration::SshAgent]);
    let bus_environment = environment.with_host_integrations([HostIntegration::SessionBus]);
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_environment(ssh_environment);

    let configured = permission_request(json!({
        "requested": { "host_integrations": ["ssh-agent"] },
        "for_action": { "command": "ssh -T git@example.test", "cwd": null }
    }));
    let unconfigured = permission_request(json!({
        "requested": { "host_integrations": ["dbus"] },
        "for_action": { "command": "secret-tool lookup key value", "cwd": null }
    }));
    assert!(
        factory
            .request_capabilities_are_satisfied(&configured)
            .expect("configured integration should be evaluated"),
        "an integration enabled by trusted config must not require another review"
    );
    assert!(
        !factory
            .request_capabilities_are_satisfied(&unconfigured)
            .expect("unconfigured integration should be evaluated"),
        "an integration trusted config did not enable still requires review"
    );

    let unrelated = permission_request(json!({
        "requested": { "paths": [{ "path": ".", "access": "rw" }] },
        "for_action": { "command": "true", "cwd": null }
    }));
    let launch = factory
        .build_runner(&unrelated)
        .plan_for(request_process_intent(&unrelated))
        .expect("preauthorized integration should build a launch");
    assert!(launch.read_write().iter().any(|path| path == ssh_grant));
    assert!(
        launch
            .env()
            .iter()
            .any(|(name, value)| name == "SSH_AUTH_SOCK" && value == &ssh_path)
    );
    assert!(!launch.read_write().iter().any(|path| path == bus_grant));
    assert!(
        launch
            .remove_env()
            .iter()
            .any(|name| name == "DBUS_SESSION_BUS_ADDRESS")
    );

    let launch = bus_factory_launch(&bus_environment, &unrelated);
    let address = format!("unix:path={bus_path}");
    assert!(launch.read_write().iter().any(|path| path == bus_grant));
    assert!(
        launch
            .env()
            .iter()
            .any(|(name, value)| name == "DBUS_SESSION_BUS_ADDRESS" && value == &address)
    );
    assert!(!launch.read_write().iter().any(|path| path == ssh_grant));
    assert!(
        launch
            .remove_env()
            .iter()
            .any(|name| name == "SSH_AUTH_SOCK")
    );
}

fn bus_factory_launch(
    environment: &WorkspaceProcessEnvironment,
    request: &merry_runtime::PermissionRequest,
) -> crate::confinement::SandboxLaunch {
    WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_environment(environment.clone())
        .build_runner(request)
        .plan_for(request_process_intent(request))
        .expect("preauthorized session bus should build a launch")
}

#[test]
#[cfg(unix)]
fn permissioned_factory_keeps_approved_host_integrations_for_later_actions() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("agent.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let socket_path = socket.to_str().unwrap().to_owned();
    let socket_grant = resolve_host_path(&socket);
    let socket_grant = socket_grant.to_str().unwrap().to_owned();
    let mut environment =
        WorkspaceProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
            .expect("environment layout should validate");
    environment.ssh_agent_socket = Some(socket.clone());
    let session_permissions = ProcessSessionPermissions::new();
    let factory = WorkspaceProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_environment(environment.clone())
        .with_session_permissions(session_permissions.clone());
    let request = permission_request(json!({
        "requested": { "host_integrations": ["ssh-agent"] },
        "for_action": { "command": "ssh -T git@example.test", "cwd": null }
    }));

    factory
        .validate_request(&request)
        .expect("configured host integration should be materializable");
    let _ = factory.runner_for(&request);

    let later_request = permission_request(json!({
        "requested": { "paths": [{ "path": ".", "access": "rw" }] },
        "for_action": { "command": "ssh -T git@example.test", "cwd": null }
    }));
    let launch = WorkspaceProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_environment(environment)
        .with_session_permissions(session_permissions)
        .plan_for(request_process_intent(&later_request))
        .expect("later process plan should build");

    assert!(launch.read_write().iter().any(|path| path == &socket_grant));
    assert!(
        launch
            .env()
            .iter()
            .any(|(name, value)| name == "SSH_AUTH_SOCK" && value == &socket_path)
    );
}
