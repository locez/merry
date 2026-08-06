use super::*;
use crate::config::{EffectiveLogSettings, LogFormat, LogLevel, XdgPaths};
use crate::provider_config::MERRY_OPENAI_DEBUG_ENV;
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

#[derive(Default)]
struct FakeHostProbe {
    metadata: BTreeMap<PathBuf, HostPathMetadata>,
}

impl FakeHostProbe {
    fn socket(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::UnixSocket, owner_uid),
        );
        self
    }

    fn regular_file(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::RegularFile, owner_uid),
        );
        self
    }

    fn other(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::Other, owner_uid),
        );
        self
    }
}

impl HostPathProbe for FakeHostProbe {
    fn file_exists(&self, path: &Path) -> bool {
        path_is_fake_bwrap(path) || self.metadata.contains_key(path)
    }

    fn metadata(&self, path: &Path) -> Option<HostPathMetadata> {
        self.metadata.get(path).copied()
    }
}

fn sandbox_host() -> Host {
    Host {
        cwd: PathBuf::from("/workspace/merry"),
        current_exe: PathBuf::from("/workspace/merry/target/debug/merry"),
        args: vec![
            os("--with-sandbox"),
            os("debug"),
            os("--session-id"),
            os("custom-session"),
        ],
        path: Some(os("/custom/bin:/usr/bin")),
        openai_debug: None,
        inside_sandbox: false,
        xdg_paths: XdgPaths::from_parts(
            PathBuf::from("/home/alice"),
            Some(PathBuf::from("/host/config")),
            Some(PathBuf::from("/host/state")),
        ),
        log_settings: None,
        trusted_path_rules: Vec::new(),
        graphical_environment: GraphicalEnvironment::default(),
        current_uid: 1_000,
    }
}

fn path_is_fake_bwrap(path: &Path) -> bool {
    path == Path::new("/custom/bin/bwrap")
}

fn plan_sandbox(with_sandbox: bool, host: &Host) -> Result<Bootstrap, Error> {
    plan_bootstrap_with_file_exists(with_sandbox, host, path_is_fake_bwrap)
}

fn plan_sandbox_with_clipboard(
    host: &Host,
    probe: &impl HostPathProbe,
) -> Result<Bootstrap, Error> {
    plan_bootstrap_with_probe(true, ClipboardAccess::Tui, host, probe)
}

fn plan_args(plan: &Plan) -> Vec<String> {
    plan.args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn runtime_profile_requires_tmpfs_home_tmp_and_expected_env() {
    let mountinfo = "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
";

    assert_eq!(
        runtime_profile_from_evidence(
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        Some(RuntimeProfile::CliBwrapV1)
    );
    assert_eq!(
        runtime_profile_from_evidence(
            Some(OsStr::new("/home/locez")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        Some(RuntimeProfile::CliBwrapV1)
    );

    let custom_home_mountinfo = "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /root rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
";
    assert_eq!(
        runtime_profile_from_evidence(
            Some(OsStr::new("/root")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(custom_home_mountinfo),
        ),
        Some(RuntimeProfile::CliBwrapV1)
    );

    for (home, tmpdir, mountinfo) in [
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new("/var/tmp")),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(
                "\
26 24 0:22 / / rw,relatime - overlay overlay rw
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
            ),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(
                "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,relatime - ext4 /dev/sda1 rw
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
            ),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(
                "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
            ),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            None,
        ),
        (
            Some(OsStr::new("/")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME_ROOT)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new("/tmp/merry-home")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new("/home/../root")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new("home/alice")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
    ] {
        assert_eq!(runtime_profile_from_evidence(home, tmpdir, mountinfo), None);
    }
}

#[test]
fn planning_skips_when_disabled() {
    let host = sandbox_host();

    let bootstrap = plan_sandbox(false, &host).expect("disabled sandbox planning should succeed");

    assert_eq!(bootstrap, Bootstrap::Disabled);
}

#[test]
fn planning_skips_when_already_inside() {
    let mut host = sandbox_host();
    host.inside_sandbox = true;

    let bootstrap =
        plan_sandbox(true, &host).expect("already-inside sandbox planning should succeed");

    assert_eq!(bootstrap, Bootstrap::AlreadyInside);
}

#[test]
fn plan_uses_bwrap_and_required_namespace_args() {
    let host = sandbox_host();
    let bootstrap = plan_sandbox(true, &host).expect("sandbox planning should succeed");
    let Bootstrap::Reexec(plan) = bootstrap else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert_eq!(plan.program, OsString::from("/custom/bin/bwrap"));
    for expected in [
        "--unshare-user",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
    ] {
        assert!(args.iter().any(|arg| arg == expected), "missing {expected}");
    }
    assert!(!args.iter().any(|arg| arg == "--disable-userns"));
    assert!(!args.iter().any(|arg| arg == "--unshare-net"));
}

#[test]
fn plan_mounts_runtime_paths_and_workspace() {
    let host = sandbox_host();
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(&args, &["--proc", "/proc"]));
    assert!(contains_sequence(&args, &["--dev", "/dev"]));
    assert!(contains_sequence(
        &args,
        &["--perms", "01777", "--tmpfs", SANDBOX_TMPDIR]
    ));
    assert!(contains_sequence(&args, &["--tmpfs", "/home"]));
    assert!(contains_sequence(
        &args,
        &["--perms", "0700", "--dir", SANDBOX_HOME]
    ));
    assert!(contains_sequence(&args, &["--dir", "/etc"]));
    assert!(contains_sequence(&args, &["--ro-bind", "/usr", "/usr"]));
    assert!(contains_sequence(&args, &["--ro-bind-try", "/bin", "/bin"]));
    assert!(contains_sequence(&args, &["--ro-bind-try", "/lib", "/lib"]));
    assert!(contains_sequence(
        &args,
        &["--ro-bind-try", "/lib64", "/lib64"]
    ));
    assert!(contains_sequence(&args, &["--ro-bind-try", "/opt", "/opt"]));
    assert!(contains_sequence(
        &args,
        &[
            "--dir",
            "/etc",
            "--ro-bind",
            "/etc/ld.so.conf",
            "/etc/ld.so.conf"
        ]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--dir",
            "/etc",
            "--ro-bind",
            "/etc/resolv.conf",
            "/etc/resolv.conf"
        ]
    ));
    assert!(contains_sequence(
        &args,
        &["--dir", "/etc", "--ro-bind", "/etc/hosts", "/etc/hosts"]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--dir",
            "/etc",
            "--ro-bind",
            "/etc/nsswitch.conf",
            "/etc/nsswitch.conf"
        ]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--dir",
            "/etc",
            "--ro-bind",
            "/etc/ld.so.conf.d",
            "/etc/ld.so.conf.d"
        ]
    ));
    assert!(contains_sequence(
        &args,
        &["--dir", "/etc", "--ro-bind", "/etc/ssl", "/etc/ssl"]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--dir",
            "/etc",
            "--ro-bind",
            "/etc/ld.so.cache",
            "/etc/ld.so.cache"
        ]
    ));
    assert!(!contains_sequence(
        &args,
        &["--ro-bind-try", "/etc/ld.so.cache", "/etc/ld.so.cache"]
    ));
    assert!(!contains_sequence(
        &args,
        &["--ro-bind-try", "/etc/resolv.conf", "/etc/resolv.conf"]
    ));
    assert!(!contains_sequence(
        &args,
        &["--ro-bind-try", "/etc", "/etc"]
    ));
    assert!(contains_sequence(
        &args,
        &["--bind", "/workspace/merry", "/workspace/merry"]
    ));
    assert!(contains_sequence(&args, &["--chdir", "/workspace/merry"]));
}

#[test]
fn plan_isolates_custom_home_before_applying_home_permissions() {
    let mut host = sandbox_host();
    host.xdg_paths = XdgPaths::from_parts(
        PathBuf::from("/srv/alice"),
        Some(PathBuf::from("/host/config")),
        Some(PathBuf::from("/host/state")),
    );
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &[
            "--dir",
            "/srv",
            "--tmpfs",
            "/srv/alice",
            "--perms",
            "0700",
            "--dir",
            "/srv/alice"
        ]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "HOME", "/srv/alice"]
    ));
}

#[test]
fn planning_rejects_workspace_that_contains_home() {
    let mut host = sandbox_host();
    host.cwd = PathBuf::from("/home");

    let error = plan_sandbox(true, &host).expect_err("HOME overlap must fail closed");
    assert!(matches!(error, Error::InvalidMountLayout(reason) if reason.contains("HOME")));
}

#[test]
fn plan_mounts_merry_config_dir_read_only_and_sets_xdg_config_home() {
    let host = sandbox_host();
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind-try",
            "/host/config/merry",
            SANDBOX_MERRY_CONFIG_DIR
        ]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "XDG_CONFIG_HOME", SANDBOX_XDG_CONFIG_HOME]
    ));
}

#[test]
fn plan_mounts_only_managed_provider_config_read_write() {
    let host = sandbox_host();
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind-try",
            "/host/config/merry",
            SANDBOX_MERRY_CONFIG_DIR,
        ]
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--bind",
            "/host/config/merry/managed",
            SANDBOX_MERRY_MANAGED_CONFIG_DIR,
        ]
    ));
    assert!(!contains_sequence(
        &args,
        &["--bind", "/host/config/merry", SANDBOX_MERRY_CONFIG_DIR]
    ));
    assert!(!contains_sequence(
        &args,
        &["--bind-try", "/host/config/merry", SANDBOX_MERRY_CONFIG_DIR,]
    ));
}

#[test]
fn plan_mounts_merry_state_dir_read_write_for_persistent_sessions() {
    let host = sandbox_host();
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &["--bind", "/host/state/merry", SANDBOX_MERRY_STATE_DIR]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "XDG_STATE_HOME", SANDBOX_XDG_STATE_HOME]
    ));
}

#[test]
fn plan_applies_trusted_global_path_rules_as_outer_guard() {
    let mut host = sandbox_host();
    host.trusted_path_rules = vec![
        PathAccessRule::new(
            PathBuf::from("/var/log"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            PathBuf::from("/workspace/shared"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            PathBuf::from("/home/alice/.ssh"),
            PathAccess::Deny,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
    ];
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &["--ro-bind-try", "/var/log", "/var/log"]
    ));
    assert!(contains_sequence(
        &args,
        &["--bind-try", "/workspace/shared", "/workspace/shared"]
    ));
    assert!(contains_sequence(&args, &["--tmpfs", "/home/alice/.ssh"]));
}

#[test]
fn plan_does_not_mount_log_dir_when_logging_is_disabled() {
    let host = sandbox_host();
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(!contains_sequence(
        &args,
        &["--bind", "/host/state/merry/logs", SANDBOX_MERRY_LOG_DIR]
    ));
}

#[test]
fn plan_mounts_log_dir_read_write_when_file_logging_is_enabled() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let host_log_dir = temp.path().join("state/merry/logs");
    let host_log_dir_string = host_log_dir.to_string_lossy().into_owned();
    let mut host = sandbox_host();
    host.log_settings = Some(EffectiveLogSettings {
        level: LogLevel::Info,
        format: LogFormat::Json,
        path: host_log_dir.join("merry.jsonl"),
    });
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &["--bind", &host_log_dir_string, &host_log_dir_string]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "XDG_STATE_HOME", SANDBOX_XDG_STATE_HOME]
    ));
    assert!(host_log_dir.exists());
}

#[test]
fn plan_clears_environment_and_allowlists_path_only_for_bwrap() {
    let host = sandbox_host();
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert_eq!(plan.env, vec![(os("PATH"), os("/custom/bin:/usr/bin"))]);
    assert!(contains_sequence(
        &args,
        &["--setenv", "PATH", "/custom/bin:/usr/bin"]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "HOME", SANDBOX_HOME]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "TMPDIR", SANDBOX_TMPDIR]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "PWD", "/workspace/merry"]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", MERRY_SANDBOX_ENV, "1"]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", MERRY_SANDBOX_VERSION_ENV, MERRY_SANDBOX_VERSION]
    ));
    assert!(!contains_sequence(
        &args,
        &["--setenv", MERRY_OPENAI_DEBUG_ENV, "1"]
    ));
    assert!(!args.iter().any(|arg| arg.contains("OPENAI_API_KEY")));
    assert!(!args.iter().any(|arg| arg.contains("MERRY_OPENAI_API_KEY")));
}

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

#[test]
fn plan_preserves_openai_debug_opt_in_without_secret_env() {
    let mut host = sandbox_host();
    host.openai_debug = Some(os("1"));
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &["--setenv", MERRY_OPENAI_DEBUG_ENV, "1"]
    ));
    assert!(!args.iter().any(|arg| arg.contains("OPENAI_API_KEY")));
    assert!(!args.iter().any(|arg| arg.contains("MERRY_OPENAI_API_KEY")));
}

#[test]
fn plan_does_not_preserve_non_opt_in_openai_debug_values() {
    for value in ["0", "true", ""] {
        let mut host = sandbox_host();
        host.openai_debug = Some(os(value));
        let Bootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("sandbox planning should succeed")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);

        assert!(!contains_sequence(
            &args,
            &["--setenv", MERRY_OPENAI_DEBUG_ENV, "1"]
        ));
    }
}

#[test]
fn plan_reexecs_current_exe_with_hidden_handoff_and_sandbox_flag_removed() {
    let host = sandbox_host();
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    let exe_index = args
        .iter()
        .position(|arg| arg == "/workspace/merry/target/debug/merry")
        .expect("current executable should be present");
    assert_eq!(
        &args[exe_index + 1..],
        [
            SANDBOX_CHILD_HANDOFF_ARG,
            SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
            "debug",
            "--session-id",
            "custom-session",
        ]
    );
}

#[test]
fn plan_strips_host_provided_hidden_handoff_before_injecting_its_own() {
    let mut host = sandbox_host();
    host.args = vec![
        os("--with-sandbox"),
        os(SANDBOX_CHILD_HANDOFF_ARG),
        os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1),
        os("debug"),
        os("--session-id"),
        os("custom-session"),
    ];
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);
    let handoff_positions = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == SANDBOX_CHILD_HANDOFF_ARG).then_some(index))
        .collect::<Vec<_>>();

    assert_eq!(handoff_positions.len(), 1);
    let handoff_index = handoff_positions[0];
    assert_eq!(args[handoff_index + 1], SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1);
    assert!(contains_sequence(
        &args,
        &[
            "/workspace/merry/target/debug/merry",
            SANDBOX_CHILD_HANDOFF_ARG,
            SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
            "debug",
            "--session-id",
            "custom-session",
        ],
    ));
}

#[test]
fn plan_strips_host_provided_hidden_handoff_assignment_before_injecting_its_own() {
    let mut host = sandbox_host();
    host.args = vec![
        os("--with-sandbox"),
        os("--merry-sandbox-child-handoff=cli-bwrap-v1"),
        os("debug"),
        os("--session-id"),
        os("custom-session"),
    ];
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert_eq!(
        args.iter()
            .filter(|arg| arg.as_str() == SANDBOX_CHILD_HANDOFF_ARG)
            .count(),
        1
    );
    assert!(
        !args
            .iter()
            .any(|arg| arg == "--merry-sandbox-child-handoff=cli-bwrap-v1")
    );
}

#[test]
fn find_bwrap_in_path_returns_first_existing_candidate() {
    let path = os("/missing/bin:/custom/bin:/later/bin");

    let found = find_bwrap_in_path(&path, |candidate| {
        candidate == Path::new("/custom/bin/bwrap") || candidate == Path::new("/later/bin/bwrap")
    });

    assert_eq!(found, Some(PathBuf::from("/custom/bin/bwrap")));
}

#[test]
fn planning_errors_when_bwrap_is_missing_from_path() {
    let host = sandbox_host();

    let error = plan_bootstrap_with_file_exists(true, &host, |_| false)
        .expect_err("missing bwrap should fail during planning");

    assert!(matches!(error, Error::MissingBubblewrap));
    assert_eq!(
        error.to_string(),
        "bubblewrap executable `bwrap` was not found in PATH; install bubblewrap to use TUI/run, or omit --with-sandbox for debug commands"
    );
}

#[test]
fn args_without_sandbox_bootstrap_flags_removes_only_first_sandbox_marker() {
    let args = vec![
        os("--with-sandbox"),
        os("debug"),
        os("--input"),
        os("--with-sandbox"),
    ];

    assert_eq!(
        args_without_sandbox_bootstrap_flags(&args),
        vec![os("debug"), os("--input"), os("--with-sandbox")]
    );
}

#[test]
fn args_without_sandbox_bootstrap_flags_preserves_shell_trailing_argv() {
    let args = vec![
        os("--with-sandbox"),
        os("shell"),
        os("--"),
        os("--with-sandbox"),
        os(SANDBOX_CHILD_HANDOFF_ARG),
        os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1),
    ];

    assert_eq!(
        args_without_sandbox_bootstrap_flags(&args),
        vec![
            os("shell"),
            os("--"),
            os("--with-sandbox"),
            os(SANDBOX_CHILD_HANDOFF_ARG),
            os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1),
        ]
    );
}

fn contains_sequence(args: &[String], expected: &[&str]) -> bool {
    args.windows(expected.len()).any(|window| {
        window
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
    })
}
