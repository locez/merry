//! Trusted-config host integration preauthorization inside the inner sandbox.

use crate::process_runner::tests::{
    contains_sequence, os_args, permission_request, request_process_intent,
};
use crate::process_runner::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessEnvironment, BwrapProcessRunner,
    BwrapSessionPermissions,
};
use merry_runtime::{HostIntegration, PermissionedProcessRunnerFactory};
use serde_json::json;

#[test]
#[cfg(unix)]
fn bwrap_permissioned_factory_preauthorizes_configured_host_integrations() {
    let directory = tempfile::tempdir().unwrap();
    let ssh = directory.path().join("ssh.sock");
    let bus = directory.path().join("bus.sock");
    let _ssh_listener = std::os::unix::net::UnixListener::bind(&ssh).unwrap();
    let _bus_listener = std::os::unix::net::UnixListener::bind(&bus).unwrap();
    let ssh_path = ssh.to_str().unwrap();
    let bus_path = bus.to_str().unwrap();
    let mut environment =
        BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
            .expect("environment layout should validate");
    environment.ssh_agent_socket = Some(ssh.clone());
    environment.session_bus_address = Some(format!("unix:path={bus_path}").into());
    // This mirrors the CLI mapping of trusted global configuration into the
    // inner backend options.
    let ssh_environment = environment
        .clone()
        .with_host_integrations([HostIntegration::SshAgent]);
    let bus_environment = environment.with_host_integrations([HostIntegration::SessionBus]);
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
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
    let plan = factory
        .build_runner(&unrelated)
        .plan_for(request_process_intent(&unrelated))
        .expect("preauthorized integration should build a sandbox plan");
    let args = os_args(&plan.args);

    assert!(contains_sequence(&args, &["--ro-bind", ssh_path, ssh_path]));
    assert!(contains_sequence(
        &args,
        &["--setenv", "SSH_AUTH_SOCK", ssh_path]
    ));
    assert!(!contains_sequence(
        &args,
        &["--ro-bind", bus_path, bus_path]
    ));

    let bus_factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
            .with_bwrap_program("/custom/bin/bwrap")
            .with_environment(bus_environment);
    let plan = bus_factory
        .build_runner(&unrelated)
        .plan_for(request_process_intent(&unrelated))
        .expect("preauthorized session bus should build a sandbox plan");
    let args = os_args(&plan.args);
    let address = format!("unix:path={bus_path}");

    assert!(contains_sequence(&args, &["--ro-bind", bus_path, bus_path]));
    assert!(contains_sequence(
        &args,
        &["--setenv", "DBUS_SESSION_BUS_ADDRESS", &address]
    ));
    assert!(!contains_sequence(
        &args,
        &["--ro-bind", ssh_path, ssh_path]
    ));
}

#[test]
#[cfg(unix)]
fn bwrap_permissioned_factory_keeps_approved_host_integrations_for_later_actions() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("agent.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let socket_path = socket.to_str().unwrap();
    let mut environment =
        BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
            .expect("environment layout should validate");
    environment.ssh_agent_socket = Some(socket.clone());
    let session_permissions = BwrapSessionPermissions::new();
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
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

    let base_runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap")
        .with_environment(environment)
        .with_session_permissions(session_permissions);
    let later_request = permission_request(json!({
        "requested": { "paths": [{ "path": ".", "access": "rw" }] },
        "for_action": { "command": "ssh -T git@example.test", "cwd": null }
    }));
    let plan = base_runner
        .plan_for(request_process_intent(&later_request))
        .expect("later process plan should build");
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &["--ro-bind", socket_path, socket_path]
    ));
}
