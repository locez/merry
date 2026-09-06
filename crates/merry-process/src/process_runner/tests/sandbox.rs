use super::{contains_sequence, count_sequence, intent, os_args};
use crate::process_runner::{
    BwrapProcessEnvironment, BwrapProcessRunner, bwrap_process_plan,
    bwrap_process_plan_with_environment, process_current_dir,
};
use merry_runtime::{HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[test]
fn process_current_dir_uses_workspace_root_for_default_cwd() {
    let root = Path::new("/tmp/merry-workspace");

    assert_eq!(
        process_current_dir(Some(root), &intent(None)),
        PathBuf::from("/tmp/merry-workspace")
    );
    assert_eq!(
        process_current_dir(Some(root), &intent(Some("."))),
        PathBuf::from("/tmp/merry-workspace")
    );
}

#[test]
fn process_current_dir_joins_workspace_relative_cwd_under_root() {
    assert_eq!(
        process_current_dir(
            Some(Path::new("/tmp/merry-workspace")),
            &intent(Some("crates"))
        ),
        PathBuf::from("/tmp/merry-workspace/crates")
    );
}

#[test]
fn bwrap_process_plan_denies_network_by_default() {
    let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap");
    let plan = bwrap_process_plan(
        &intent(Some("crates")),
        &runner.cwd_root,
        runner.network_allowed,
        &runner.path_rules,
        &runner.bwrap_program,
    );
    let args = os_args(&plan.args);

    assert_eq!(plan.program, OsString::from("/custom/bin/bwrap"));
    assert!(args.iter().any(|arg| arg == "--unshare-net"));
    assert!(contains_sequence(
        &args,
        &["--bind", "/workspace/merry", "/workspace/merry"]
    ));
    assert!(contains_sequence(&args, &["--ro-bind", "/", "/"]));
    assert!(contains_sequence(
        &args,
        &["--chdir", "/workspace/merry/crates"]
    ));
    assert!(contains_sequence(&args, &["--", "pwd"]));
}

#[test]
fn bwrap_process_plan_applies_user_environment_overrides_after_defaults() {
    let environment = BwrapProcessEnvironment::new(
        "/custom/bin:/usr/bin",
        "/home/alice",
        "/run/merry/session-tmp",
    )
    .expect("environment layout should validate")
    .with_overrides([(OsString::from("RUSTUP_TOOLCHAIN"), OsString::from("stable"))])
    .expect("environment override should validate");
    let plan = bwrap_process_plan_with_environment(
        &intent(None),
        Path::new("/workspace/merry"),
        &environment,
        true,
        &[],
        Path::new("/custom/bin/bwrap"),
    );
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &["--bind", "/run/merry/session-tmp", "/tmp"]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "PATH", "/custom/bin:/usr/bin"]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "HOME", "/home/alice"]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "RUSTUP_TOOLCHAIN", "stable"]
    ));
    assert!(!args.iter().any(|arg| arg == "--clearenv"));
    assert!(contains_sequence(&args, &["--ro-bind", "/", "/"]));
    assert!(!contains_sequence(&args, &["--tmpfs", "/home"]));
}

#[test]
fn bwrap_process_plan_materializes_host_integrations_and_keeps_parent_environment() {
    let mut environment =
        BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
            .expect("environment layout should validate");
    environment.ssh_agent_socket = Some(PathBuf::from("/run/user/1000/ssh-agent.sock"));
    environment.session_bus_address = Some(OsString::from("unix:path=/run/user/1000/bus"));
    let environment = environment
        .with_host_integrations([HostIntegration::SshAgent, HostIntegration::SessionBus]);
    let plan = bwrap_process_plan_with_environment(
        &intent(None),
        Path::new("/workspace/merry"),
        &environment,
        true,
        &[],
        Path::new("/custom/bin/bwrap"),
    );
    let args = os_args(&plan.args);

    assert!(!args.iter().any(|arg| arg == "--clearenv"));
    assert!(contains_sequence(&args, &["--ro-bind", "/", "/"]));
    assert!(contains_sequence(&args, &["--tmpfs", "/run/user/1000"]));
    assert!(contains_sequence(
        &args,
        &[
            "--unsetenv",
            "SSH_AUTH_SOCK",
            "--unsetenv",
            "DBUS_SESSION_BUS_ADDRESS"
        ]
    ));
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
        &["--setenv", "SSH_AUTH_SOCK", "/run/user/1000/ssh-agent.sock"]
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
fn bwrap_process_plan_hides_unapproved_host_integrations() {
    let mut environment =
        BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
            .expect("environment layout should validate");
    environment.ssh_agent_socket = Some(PathBuf::from("/run/user/1000/gnupg/S.gpg-agent.ssh"));
    environment.session_bus_address = Some(OsString::from("unix:path=/run/user/1000/bus"));
    let plan = bwrap_process_plan_with_environment(
        &intent(None),
        Path::new("/workspace/merry"),
        &environment,
        true,
        &[],
        Path::new("/custom/bin/bwrap"),
    );
    let args = os_args(&plan.args);

    assert!(contains_sequence(&args, &["--tmpfs", "/run/user/1000"]));
    assert!(!contains_sequence(
        &args,
        &[
            "--ro-bind",
            "/run/user/1000/gnupg/S.gpg-agent.ssh",
            "/run/user/1000/gnupg/S.gpg-agent.ssh"
        ]
    ));
    assert!(!contains_sequence(
        &args,
        &[
            "--setenv",
            "SSH_AUTH_SOCK",
            "/run/user/1000/gnupg/S.gpg-agent.ssh"
        ]
    ));
}

#[test]
fn bwrap_process_plan_preserves_action_tmp_when_host_socket_is_under_it() {
    let mut environment =
        BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
            .expect("environment layout should validate");
    environment.ssh_agent_socket = Some(PathBuf::from("/tmp/ssh-agent.sock"));
    let plan = bwrap_process_plan_with_environment(
        &intent(None),
        Path::new("/workspace/merry"),
        &environment,
        true,
        &[],
        Path::new("/custom/bin/bwrap"),
    );
    let args = os_args(&plan.args);

    assert_eq!(count_sequence(&args, &["--tmpfs", "/tmp"]), 1);
    assert_eq!(count_sequence(&args, &["--bind", "/tmp", "/tmp"]), 1);
}

#[test]
fn bwrap_process_environment_validates_temporary_boundary_and_overrides() {
    let environment = BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
        .expect("environment layout should validate");
    let validated = environment
        .validate_for_workspace(Path::new("/workspace/merry"))
        .expect("standard temporary directory should be accepted");
    assert_eq!(validated.tmp_source, PathBuf::from("/tmp"));

    for workspace in ["/", "/home", "/home/alice", "/etc", "/tmp"] {
        environment
            .validate_for_workspace(Path::new(workspace))
            .expect("the explicit workspace path should be accepted");
    }

    let error = BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/etc")
        .expect("environment path shape should validate")
        .validate_for_workspace(Path::new("/workspace/merry"))
        .expect_err("non-temporary TMPDIR must be rejected");
    assert!(error.to_string().contains("TMPDIR"));

    let overridden = environment
        .clone()
        .with_overrides([
            (OsString::from("PATH"), OsString::from("/override/bin")),
            (OsString::from("HOME"), OsString::from("/override/home")),
            (OsString::from("TMPDIR"), OsString::from("/override/tmp")),
        ])
        .expect("supported environment names should override defaults");
    assert_eq!(overridden.overrides.len(), 3);

    for name in ["", "1INVALID", "INVALID-NAME", "INVALID=NAME"] {
        let error = environment
            .clone()
            .with_overrides([(OsString::from(name), OsString::from("value"))])
            .expect_err("invalid environment names must be rejected");
        assert!(error.to_string().contains("environment"), "{error}");
    }
    let error = environment
        .with_overrides([
            (OsString::from("DUPLICATE"), OsString::from("one")),
            (OsString::from("DUPLICATE"), OsString::from("two")),
        ])
        .expect_err("duplicate environment names must be rejected");
    assert!(error.to_string().contains("duplicate"));
}

#[test]
fn bwrap_process_plan_inherits_parent_filesystem_without_clearing_environment() {
    let environment = BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/srv/alice", "/tmp")
        .expect("environment layout should validate");
    let plan = bwrap_process_plan_with_environment(
        &intent(None),
        Path::new("/workspace/merry"),
        &environment,
        true,
        &[],
        Path::new("/custom/bin/bwrap"),
    );
    let args = os_args(&plan.args);

    assert!(contains_sequence(&args, &["--ro-bind", "/", "/"]));
    assert!(!contains_sequence(&args, &["--tmpfs", "/home"]));
    assert!(contains_sequence(
        &args,
        &["--setenv", "HOME", "/srv/alice"]
    ));
    assert!(!args.iter().any(|arg| arg == "--clearenv"));
}

#[test]
fn bwrap_process_plan_does_not_expose_outer_graphical_endpoints() {
    let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
        .with_bwrap_program("/custom/bin/bwrap");
    let plan = bwrap_process_plan(
        &intent(None),
        &runner.cwd_root,
        runner.network_allowed,
        &runner.path_rules,
        &runner.bwrap_program,
    );
    let args = os_args(&plan.args);

    for forbidden in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "XAUTHORITY",
        "/tmp/.X11-unix",
        "/run/merry-wayland",
        "/run/merry-x11",
    ] {
        assert!(
            !args.iter().any(|arg| arg.contains(forbidden)),
            "inner action sandbox leaked {forbidden}: {args:?}"
        );
    }
}

#[test]
fn bwrap_process_plan_allows_network_when_configured() {
    let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry").allow_network();
    let plan = bwrap_process_plan(
        &intent(None),
        &runner.cwd_root,
        runner.network_allowed,
        &runner.path_rules,
        &runner.bwrap_program,
    );
    let args = os_args(&plan.args);

    assert!(!args.iter().any(|arg| arg == "--unshare-net"));
}

#[test]
fn bwrap_process_plan_applies_path_rules() {
    let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry").with_path_rules([
        PathAccessRule::new(
            PathBuf::from("/var/log"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            PathBuf::from("/cache"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            PathBuf::from("/home/merry/.ssh"),
            PathAccess::Deny,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
    ]);
    let plan = bwrap_process_plan(
        &intent(None),
        &runner.cwd_root,
        runner.network_allowed,
        &runner.path_rules,
        &runner.bwrap_program,
    );
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &["--ro-bind-try", "/var/log", "/var/log"]
    ));
    assert!(contains_sequence(
        &args,
        &["--bind-try", "/cache", "/cache"]
    ));
    assert!(contains_sequence(&args, &["--tmpfs", "/home/merry/.ssh"]));
}

#[cfg(unix)]
#[test]
fn bwrap_process_plan_mounts_symlinked_path_rules_at_logical_paths() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary path");
    let real = temp.path().join("real");
    let link = temp.path().join("link");
    std::fs::create_dir_all(&real).expect("real directory");
    symlink(&real, &link).expect("directory symlink");
    let rule = PathAccessRule::new(
        link.clone(),
        PathAccess::ReadWrite,
        PathAccessRuleSource::PermissionReview,
    );

    let environment = BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
        .expect("environment layout should validate");
    let plan = bwrap_process_plan_with_environment(
        &intent(None),
        Path::new("/workspace/merry"),
        &environment,
        true,
        &[rule],
        Path::new("/custom/bin/bwrap"),
    );
    let args = os_args(&plan.args);

    assert!(contains_sequence(
        &args,
        &[
            "--bind",
            real.to_str().expect("UTF-8 test path"),
            link.to_str().expect("UTF-8 test path"),
        ],
    ));
}
