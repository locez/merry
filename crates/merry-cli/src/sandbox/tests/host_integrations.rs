use crate::sandbox::{
    Bootstrap, ClipboardAccess, SANDBOX_WAYLAND_DISPLAY, SANDBOX_WAYLAND_RUNTIME_DIR,
    SANDBOX_WAYLAND_SOCKET, SANDBOX_X11_AUTHORITY,
    integrations::{GraphicalEnvironment, HostIntegrationEnvironment},
    os, plan_bootstrap_with_probe,
    tests::{
        FakeHostProbe, contains_sequence, plan_args, plan_sandbox_with_clipboard, sandbox_host,
    },
};
use merry_runtime::HostIntegration;
use std::path::PathBuf;

#[test]
fn non_tui_sandbox_ignores_valid_graphical_endpoints() {
    let mut host = sandbox_host();
    host.graphical_environment = GraphicalEnvironment {
        xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
        wayland_display: Some(os("wayland-1")),
        display: Some(os(":7")),
        xauthority: Some(PathBuf::from("/home/alice/.Xauthority")),
        home: Some(PathBuf::from("/home/alice")),
    };
    let probe = FakeHostProbe::default()
        .socket("/run/user/1000/wayland-1", 1_000)
        .socket("/tmp/.X11-unix/X7", 1_000)
        .regular_file("/home/alice/.Xauthority", 1_000);

    let Bootstrap::Reexec(plan) =
        plan_bootstrap_with_probe(true, ClipboardAccess::Disabled, &host, &probe)
            .expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    for forbidden in [
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XAUTHORITY",
        SANDBOX_WAYLAND_SOCKET,
        SANDBOX_X11_AUTHORITY,
        "/tmp/.X11-unix/X7",
    ] {
        assert!(
            !args.iter().any(|arg| arg == forbidden),
            "leaked {forbidden}"
        );
    }
}

#[test]
fn sandbox_exposes_configured_host_integrations_as_outer_ceiling() {
    let mut host = sandbox_host();
    host.host_integrations = vec![HostIntegration::SshAgent, HostIntegration::SessionBus];
    host.host_integration_environment = HostIntegrationEnvironment {
        ssh_agent_socket: Some(PathBuf::from("/run/user/1000/ssh-agent.sock")),
        session_bus_address: Some(os("unix:path=/run/user/1000/bus")),
        gpg_agent_sockets: None,
    };
    let probe = FakeHostProbe::default()
        .socket("/run/user/1000/ssh-agent.sock", 1_000)
        .socket("/run/user/1000/bus", 1_000);

    let Bootstrap::Reexec(plan) =
        plan_bootstrap_with_probe(true, ClipboardAccess::Disabled, &host, &probe)
            .expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind",
            "/run/user/1000/ssh-agent.sock",
            "/run/user/1000/ssh-agent.sock"
        ]
    ));
    assert!(contains_sequence(
        &args,
        &["--ro-bind", "/run/user/1000/bus", "/run/user/1000/bus"]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--setenv",
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/run/user/1000/bus"
        ]
    ));
}

#[test]
fn native_gpg_mount_imports_only_public_keyring_files_and_native_socket() {
    let mut host = sandbox_host();
    host.host_integrations = vec![HostIntegration::GpgAgent];
    host.host_integration_environment.gpg_agent_sockets = Some(
        merry_process::GpgAgentSockets::new("/custom/keyring", "/custom/runtime/native")
            .unwrap()
            .with_auxiliary_sockets([PathBuf::from("/custom/runtime/ssh")])
            .unwrap(),
    );
    let probe = FakeHostProbe::default()
        .socket("/custom/runtime/native", 1_000)
        .socket("/custom/runtime/ssh", 1_000)
        .regular_file("/custom/keyring/pubring.kbx", 1_000)
        .regular_file("/custom/keyring/trustdb.gpg", 1_000)
        .regular_file("/custom/keyring/gpg.conf", 1_000)
        .regular_file("/custom/keyring/secring.gpg", 1_000)
        .directory("/custom", 1_000, 0o700)
        .directory("/custom/runtime", 1_000, 0o700);
    let Bootstrap::Reexec(plan) =
        plan_bootstrap_with_probe(true, ClipboardAccess::Disabled, &host, &probe).unwrap()
    else {
        panic!("expected outer sandbox");
    };
    let args = plan_args(&plan);
    assert!(contains_sequence(
        &args,
        &["--perms", "0700", "--dir", "/custom"]
    ));
    assert!(contains_sequence(
        &args,
        &["--perms", "0700", "--dir", "/custom/runtime"]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind",
            "/custom/runtime/native",
            "/custom/runtime/native"
        ]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "GNUPGHOME", "/custom/keyring"]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind",
            "/custom/keyring/pubring.kbx",
            "/custom/keyring/pubring.kbx"
        ]
    ));
    for path in [
        "/custom/runtime/ssh",
        "/custom/runtime",
        "/custom/keyring",
        "/custom/keyring/trustdb.gpg",
        "/custom/keyring/gpg.conf",
        "/custom/keyring/secring.gpg",
    ] {
        assert!(!contains_sequence(&args, &["--ro-bind", path, path]));
        assert!(!contains_sequence(&args, &["--bind", path, path]));
    }
}

#[test]
fn ssh_trust_files_are_opt_in_and_never_override_a_path_deny() {
    use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};
    let fixture = tempfile::Builder::new()
        .prefix("merry-ssh-trust-")
        .tempdir_in("/var/tmp")
        .unwrap();
    let home = fixture.path().join("home");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    let probe = FakeHostProbe::default()
        .regular_file(home.join(".ssh/known_hosts").to_str().unwrap(), 1_000)
        .regular_file(home.join(".ssh/known_hosts2").to_str().unwrap(), 1_000)
        .regular_file(home.join(".ssh/id_ed25519").to_str().unwrap(), 1_000)
        .regular_file(home.join(".ssh/config").to_str().unwrap(), 1_000);
    for (enabled, denied) in [(false, false), (true, false), (true, true)] {
        let mut host = sandbox_host();
        host.xdg_paths = crate::config::XdgPaths::from_parts(
            home.clone(),
            Some(fixture.path().join("config")),
            Some(fixture.path().join("state")),
        );
        if enabled {
            host.host_integrations = vec![HostIntegration::SshAgent];
        }
        if denied {
            host.trusted_path_rules.push(PathAccessRule::new(
                home.join(".ssh"),
                PathAccess::Deny,
                PathAccessRuleSource::TrustedGlobalConfig,
            ));
        }
        let Bootstrap::Reexec(plan) =
            plan_bootstrap_with_probe(true, ClipboardAccess::Disabled, &host, &probe).unwrap()
        else {
            panic!("outer plan");
        };
        let args = plan_args(&plan);
        for path in [
            home.join(".ssh/known_hosts"),
            home.join(".ssh/known_hosts2"),
        ] {
            assert_eq!(
                contains_sequence(
                    &args,
                    &["--ro-bind", path.to_str().unwrap(), path.to_str().unwrap()]
                ),
                enabled && !denied
            );
        }
        for path in [home.join(".ssh/id_ed25519"), home.join(".ssh/config")] {
            assert!(
                !args
                    .iter()
                    .any(|argument| argument == path.to_str().unwrap())
            );
        }
    }
}

#[test]
fn tui_sandbox_mounts_only_the_valid_wayland_socket_and_rewrites_environment() {
    let mut host = sandbox_host();
    host.graphical_environment = GraphicalEnvironment {
        xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
        wayland_display: Some(os("wayland-1")),
        display: None,
        xauthority: None,
        home: Some(PathBuf::from("/home/alice")),
    };
    let probe = FakeHostProbe::default().socket("/run/user/1000/wayland-1", 1_000);

    let Bootstrap::Reexec(plan) =
        plan_sandbox_with_clipboard(&host, &probe).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind",
            "/run/user/1000/wayland-1",
            SANDBOX_WAYLAND_SOCKET,
        ]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "XDG_RUNTIME_DIR", SANDBOX_WAYLAND_RUNTIME_DIR]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "WAYLAND_DISPLAY", SANDBOX_WAYLAND_DISPLAY]
    ));
    assert!(!contains_sequence(
        &args,
        &["--ro-bind", "/run/user/1000", "/run/user/1000"]
    ));
}

#[test]
fn tui_sandbox_mounts_exact_x11_socket_and_owned_authority_file() {
    let mut host = sandbox_host();
    host.graphical_environment = GraphicalEnvironment {
        xdg_runtime_dir: None,
        wayland_display: None,
        display: Some(os("unix:7.1")),
        xauthority: Some(PathBuf::from("/home/alice/custom.Xauthority")),
        home: Some(PathBuf::from("/home/alice")),
    };
    let probe = FakeHostProbe::default()
        .socket("/tmp/.X11-unix/X7", 1_000)
        .regular_file("/home/alice/custom.Xauthority", 1_000);

    let Bootstrap::Reexec(plan) =
        plan_sandbox_with_clipboard(&host, &probe).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &["--ro-bind", "/tmp/.X11-unix/X7", "/tmp/.X11-unix/X7"]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind",
            "/home/alice/custom.Xauthority",
            SANDBOX_X11_AUTHORITY,
        ]
    ));
    assert!(contains_sequence(&args, &["--setenv", "DISPLAY", ":7.1"]));
    assert!(contains_sequence(
        &args,
        &["--setenv", "XAUTHORITY", SANDBOX_X11_AUTHORITY]
    ));
    assert!(!contains_sequence(
        &args,
        &["--ro-bind", "/tmp/.X11-unix", "/tmp/.X11-unix"]
    ));
}

#[test]
fn tui_sandbox_skips_malformed_wrong_kind_and_wrong_owner_endpoints() {
    let cases = [
        (
            Some(PathBuf::from("/run/user/1000")),
            Some(os("../wayland-0")),
            None,
            None,
            FakeHostProbe::default().socket("/run/user/wayland-0", 1_000),
        ),
        (
            Some(PathBuf::from("/run/user/1000")),
            Some(os("wayland-0")),
            None,
            None,
            FakeHostProbe::default().other("/run/user/1000/wayland-0", 1_000),
        ),
        (
            Some(PathBuf::from("/run/user/1000")),
            Some(os("wayland-0")),
            None,
            None,
            FakeHostProbe::default().socket("/run/user/1000/wayland-0", 2_000),
        ),
        (
            None,
            None,
            Some(os("remote.example:0")),
            Some(PathBuf::from("/home/alice/.Xauthority")),
            FakeHostProbe::default()
                .socket("/tmp/.X11-unix/X0", 1_000)
                .regular_file("/home/alice/.Xauthority", 1_000),
        ),
        (
            None,
            None,
            Some(os(":0")),
            Some(PathBuf::from("/home/alice/.Xauthority")),
            FakeHostProbe::default()
                .other("/tmp/.X11-unix/X0", 1_000)
                .regular_file("/home/alice/.Xauthority", 1_000),
        ),
        (
            None,
            None,
            Some(os(":0")),
            Some(PathBuf::from("/home/alice/.Xauthority")),
            FakeHostProbe::default()
                .socket("/tmp/.X11-unix/X0", 1_000)
                .regular_file("/home/alice/.Xauthority", 2_000),
        ),
    ];

    for (xdg_runtime_dir, wayland_display, display, xauthority, probe) in cases {
        let mut host = sandbox_host();
        host.graphical_environment = GraphicalEnvironment {
            xdg_runtime_dir,
            wayland_display,
            display,
            xauthority,
            home: Some(PathBuf::from("/home/alice")),
        };
        let Bootstrap::Reexec(plan) = plan_sandbox_with_clipboard(&host, &probe)
            .expect("invalid graphical endpoints should be ignored")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        for forbidden in [
            "WAYLAND_DISPLAY",
            "DISPLAY",
            "XAUTHORITY",
            SANDBOX_WAYLAND_SOCKET,
            SANDBOX_X11_AUTHORITY,
        ] {
            assert!(
                !args.iter().any(|arg| arg == forbidden),
                "invalid endpoint leaked {forbidden}: {args:?}"
            );
        }
    }
}

#[test]
fn tui_sandbox_exposes_wayland_first_and_keeps_x11_as_fallback() {
    let mut host = sandbox_host();
    host.graphical_environment = GraphicalEnvironment {
        xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
        wayland_display: Some(os("wayland-0")),
        display: Some(os(":2")),
        xauthority: None,
        home: Some(PathBuf::from("/home/alice")),
    };
    let probe = FakeHostProbe::default()
        .socket("/run/user/1000/wayland-0", 1_000)
        .socket("/tmp/.X11-unix/X2", 1_000)
        .regular_file("/home/alice/.Xauthority", 1_000);

    let Bootstrap::Reexec(plan) =
        plan_sandbox_with_clipboard(&host, &probe).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);
    let wayland = args
        .iter()
        .position(|arg| arg == SANDBOX_WAYLAND_SOCKET)
        .expect("Wayland socket should be present");
    let x11 = args
        .iter()
        .position(|arg| arg == "/tmp/.X11-unix/X2")
        .expect("X11 fallback should be present");

    assert!(wayland < x11);
    assert!(contains_sequence(&args, &["--setenv", "DISPLAY", ":2"]));
}
