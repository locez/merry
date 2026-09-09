use crate::{
    config::{EffectiveLogSettings, LogFormat, LogLevel, XdgPaths},
    provider_config::MERRY_OPENAI_DEBUG_ENV,
    sandbox::{
        Bootstrap, Error, MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION, MERRY_SANDBOX_VERSION_ENV,
        SANDBOX_ETC_READ_ONLY_DIR_PATHS, SANDBOX_ETC_READ_ONLY_FILE_PATHS, SANDBOX_HOME,
        SANDBOX_MERRY_CONFIG_DIR, SANDBOX_MERRY_LOG_DIR, SANDBOX_MERRY_MANAGED_CONFIG_DIR,
        SANDBOX_MERRY_STATE_DIR, SANDBOX_TMPDIR, SANDBOX_XDG_CONFIG_HOME, SANDBOX_XDG_STATE_HOME,
        os,
        tests::{
            assert_ro_mount, contains_sequence, plan_args, plan_sandbox, sandbox_host,
            sequence_position,
        },
    },
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

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
    assert_ro_mount(&args, "--ro-bind", "/usr", "/usr");
    for path in ["/bin", "/lib", "/lib64", "/opt"] {
        if Path::new(path).exists() {
            assert_runtime_readonly_path(&args, "--ro-bind-try", path);
        }
    }
    for path in SANDBOX_ETC_READ_ONLY_FILE_PATHS {
        if Path::new(path).exists() {
            assert_runtime_readonly_path(&args, "--ro-bind", path);
        }
    }
    for path in SANDBOX_ETC_READ_ONLY_DIR_PATHS {
        if Path::new(path).exists() {
            assert_runtime_readonly_path(&args, "--ro-bind", path);
        }
    }
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

fn assert_runtime_readonly_path(args: &[String], flag: &str, path: &str) {
    if let Ok(target) = std::fs::read_link(path) {
        assert!(contains_sequence(
            args,
            &["--symlink", target.to_str().unwrap(), path]
        ));
        let resolved = merry_process::resolve_bwrap_path(Path::new(path));
        if !resolved.starts_with("/usr") {
            let resolved = resolved.to_str().unwrap();
            assert_ro_mount(args, flag, resolved, resolved);
        }
    } else {
        assert_ro_mount(args, flag, path, path);
    }
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
fn planning_binds_explicit_workspace_path_without_home_relationship_rules() {
    for workspace in ["/home", "/etc", "/"] {
        let mut host = sandbox_host();
        host.cwd = PathBuf::from(workspace);
        let Bootstrap::Reexec(plan) =
            plan_sandbox(true, &host).expect("explicit workspace path should be accepted")
        else {
            panic!("expected sandbox reexec plan");
        };
        let args = plan_args(&plan);
        assert!(contains_sequence(&args, &["--bind", workspace, workspace]));
    }
}

#[test]
fn planning_rejects_unclean_workspace_path() {
    for workspace in ["relative", "/home/../etc"] {
        let mut host = sandbox_host();
        host.cwd = PathBuf::from(workspace);

        let error = plan_sandbox(true, &host).expect_err("unclean workspace path must fail");
        assert!(
            matches!(error, Error::InvalidWorkspacePath(reason) if reason.contains("clean absolute"))
        );
    }
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
    let config_mount = sequence_position(
        &args,
        &[
            "--ro-bind-try",
            "/host/config/merry",
            SANDBOX_MERRY_CONFIG_DIR,
        ],
    )
    .expect("read-only provider config mount");
    let managed_mount = sequence_position(
        &args,
        &[
            "--bind",
            "/host/config/merry/managed",
            SANDBOX_MERRY_MANAGED_CONFIG_DIR,
        ],
    )
    .expect("managed provider write mount");
    assert!(
        config_mount < managed_mount,
        "managed provider mount must override only its read-only parent"
    );
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
