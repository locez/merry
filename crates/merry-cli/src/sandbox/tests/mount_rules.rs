use crate::{
    config::XdgPaths,
    sandbox::{
        Bootstrap, ClipboardAccess, Error, Plan, SANDBOX_MERRY_MANAGED_CONFIG_DIR,
        mounts::{MountOrigin, MountPlan},
        os,
        tests::{
            FakeHostProbe, contains_sequence, plan_args, plan_sandbox, sandbox_host,
            sequence_position,
        },
    },
};
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

#[test]
fn sandbox_allows_product_write_roots_under_trusted_readonly_parent_rules() {
    let mut host = sandbox_host();
    host.trusted_path_rules = vec![
        PathAccessRule::new(
            PathBuf::from("/host/config"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            PathBuf::from("/host/state"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
    ];
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("read-only parents should allow product child writes")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);
    let config_parent =
        sequence_position(&args, &["--ro-bind-try", "/host/config", "/host/config"])
            .expect("trusted config parent mount");
    let managed_write = sequence_position(
        &args,
        &[
            "--bind",
            "/host/config/merry/managed",
            SANDBOX_MERRY_MANAGED_CONFIG_DIR,
        ],
    )
    .expect("managed provider write mount");
    assert!(config_parent < managed_write);
}

#[test]
fn sandbox_retains_trusted_readonly_product_child_rules() {
    let mut host = sandbox_host();
    host.trusted_path_rules = vec![PathAccessRule::new(
        PathBuf::from("/host/config/merry/managed/secrets"),
        PathAccess::ReadOnly,
        PathAccessRuleSource::TrustedGlobalConfig,
    )];

    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("nested read-only rule should be representable")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);
    let managed_write = sequence_position(
        &args,
        &[
            "--bind",
            "/host/config/merry/managed",
            SANDBOX_MERRY_MANAGED_CONFIG_DIR,
        ],
    )
    .expect("managed provider write mount");
    let secrets_readonly = sequence_position(
        &args,
        &[
            "--ro-bind-try",
            "/host/config/merry/managed/secrets",
            "/host/config/merry/managed/secrets",
        ],
    )
    .expect("trusted secrets read-only mount");
    assert!(managed_write < secrets_readonly);
}

#[test]
fn sandbox_orders_nested_trusted_rules_from_parent_to_child() {
    let mut host = sandbox_host();
    host.trusted_path_rules = vec![
        PathAccessRule::new(
            PathBuf::from("/workspace/shared/cache"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            PathBuf::from("/workspace/shared"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
    ];

    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("nested trusted rules should be representable")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);
    let parent = sequence_position(
        &args,
        &["--ro-bind-try", "/workspace/shared", "/workspace/shared"],
    )
    .expect("trusted parent rule");
    let child = sequence_position(
        &args,
        &[
            "--bind-try",
            "/workspace/shared/cache",
            "/workspace/shared/cache",
        ],
    )
    .expect("trusted child rule");
    assert!(
        parent < child,
        "narrower trusted rules must be applied last"
    );
}

#[test]
fn sandbox_rejects_conflicting_rules_for_the_same_path() {
    let mut host = sandbox_host();
    host.trusted_path_rules = vec![
        PathAccessRule::new(
            PathBuf::from("/workspace/shared"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            PathBuf::from("/workspace/shared"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
    ];

    let error = plan_sandbox(true, &host).expect_err("same-path conflicts must fail closed");
    assert!(matches!(error, Error::ConflictingTrustedPathRules { .. }));
}

#[cfg(unix)]
#[test]
fn sandbox_supports_symlinked_product_write_roots() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary sandbox paths");
    let real_config = temp.path().join("real-config");
    let linked_config = temp.path().join("config-link");
    std::fs::create_dir_all(&real_config).expect("real config directory");
    symlink(&real_config, &linked_config).expect("config symlink");

    let mut host = sandbox_host();
    host.xdg_paths = XdgPaths::from_parts(
        PathBuf::from("/home/alice"),
        Some(linked_config.clone()),
        Some(temp.path().join("state")),
    );

    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("symlinked write root should be mountable")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);
    let resolved_config_dir = real_config.join("merry");
    let resolved_managed_config_dir = resolved_config_dir.join("managed");

    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind-try",
            resolved_config_dir.to_str().expect("UTF-8 test path"),
            resolved_config_dir.to_str().expect("UTF-8 test path"),
        ],
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--bind",
            resolved_managed_config_dir
                .to_str()
                .expect("UTF-8 test path"),
            resolved_managed_config_dir
                .to_str()
                .expect("UTF-8 test path"),
        ],
    ));
    assert!(contains_sequence(
        &args,
        &[
            "--symlink",
            real_config.to_str().unwrap(),
            linked_config.to_str().unwrap(),
        ]
    ));
}

#[cfg(unix)]
#[test]
fn sandbox_preserves_symlinked_file_sources_and_binds_their_targets() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary sandbox paths");
    let real_file = temp.path().join("real.conf");
    let linked_file = temp.path().join("resolv.conf");
    std::fs::write(&real_file, "nameserver 192.0.2.1\n").expect("real file");
    symlink(&real_file, &linked_file).expect("file symlink");

    let mut args = Vec::new();
    let mut mounts = MountPlan::default();
    mounts.bind(
        &linked_file,
        Path::new("/etc/resolv.conf"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::System,
    );
    mounts.append_args(&mut args).expect("file mount plan");
    let args = plan_args(&Plan {
        program: OsString::from("bwrap"),
        args,
        env: Vec::new(),
        #[cfg(target_os = "linux")]
        ssh_config: merry_process::BwrapSshConfigFiles::default(),
    });

    assert!(contains_sequence(
        &args,
        &[
            "--ro-bind",
            real_file.to_str().expect("UTF-8 test path"),
            real_file.to_str().expect("UTF-8 test path"),
        ],
    ));
    assert!(contains_sequence(
        &args,
        &["--symlink", real_file.to_str().unwrap(), "/etc/resolv.conf",]
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn bubblewrap_reads_a_symlinked_file_through_its_logical_mount_path() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary sandbox paths");
    let real_file = temp.path().join("real.conf");
    let linked_file = temp.path().join("resolv.conf");
    std::fs::write(&real_file, "nameserver 192.0.2.1\n").expect("real file");
    symlink(&real_file, &linked_file).expect("file symlink");

    let destination = Path::new("/etc/resolv.conf");
    let mut args = vec![os("--unshare-user"), os("--die-with-parent")];
    let mut mounts = MountPlan::default();
    for path in ["/usr", "/bin", "/lib", "/lib64"] {
        mounts.bind(
            Path::new(path),
            Path::new(path),
            PathAccess::ReadOnly,
            true,
            MountOrigin::System,
        );
    }
    mounts.bind(
        &linked_file,
        destination,
        PathAccess::ReadOnly,
        false,
        MountOrigin::System,
    );
    mounts.append_args(&mut args).unwrap();
    args.extend([
        os("--"),
        os("/bin/sh"),
        os("-eu"),
        os("-c"),
        os("test -L /etc/resolv.conf; test ! -w /etc/resolv.conf; cat /etc/resolv.conf"),
    ]);

    let output = match Command::new("bwrap").args(args).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("bubblewrap test could not start: {error}"),
    };

    assert!(
        output.status.success(),
        "bubblewrap symlink mount probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"nameserver 192.0.2.1\n");
}

#[test]
fn sandbox_rejects_merry_owned_write_roots_under_trusted_deny_rules() {
    let mut host = sandbox_host();
    host.trusted_path_rules = vec![PathAccessRule::new(
        PathBuf::from("/host/config/merry/managed/secrets"),
        PathAccess::Deny,
        PathAccessRuleSource::TrustedGlobalConfig,
    )];

    let error = plan_sandbox(true, &host).expect_err("conflicting outer rule must fail closed");
    assert!(matches!(
        error,
        Error::ProductPathConflictsWithTrustedRule {
            access: PathAccess::Deny,
            ..
        }
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn bubblewrap_keeps_child_write_bind_after_readonly_parent_mount() {
    let temp = tempfile::tempdir().expect("temporary bubblewrap paths");
    let parent = temp.path().join("config");
    let managed = parent.join("managed");
    std::fs::create_dir_all(&managed).expect("managed directory");

    let output = match Command::new("bwrap")
        .args(["--unshare-user", "--die-with-parent", "--ro-bind", "/", "/"])
        .arg("--ro-bind")
        .arg(&parent)
        .arg(&parent)
        .arg("--bind")
        .arg(&managed)
        .arg(&managed)
        .arg("--")
        .arg("/usr/bin/touch")
        .arg(managed.join("probe"))
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("bubblewrap test could not start: {error}"),
    };

    assert!(
        output.status.success(),
        "bubblewrap mount-order probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(managed.join("probe").is_file());
}

#[test]
fn sandbox_prepares_provider_and_state_write_roots_before_building_mounts() {
    let temp = tempfile::tempdir().expect("temporary sandbox paths");
    let mut host = sandbox_host();
    host.xdg_paths = XdgPaths::from_parts(
        PathBuf::from("/home/alice"),
        Some(temp.path().join("config")),
        Some(temp.path().join("state")),
    );

    let probe = FakeHostProbe::default();
    let bootstrap = crate::sandbox::plan_bootstrap_with_probe_inner(
        true,
        ClipboardAccess::Disabled,
        &host,
        &probe,
        true,
    )
    .expect("sandbox plan should prepare writable roots");
    assert!(matches!(bootstrap, Bootstrap::Reexec(_)));
    assert!(
        host.xdg_paths
            .managed_providers_file()
            .parent()
            .is_some_and(Path::exists)
    );
    assert!(host.xdg_paths.managed_secrets_dir().is_dir());
    assert!(host.xdg_paths.state_dir().is_dir());
}

#[test]
fn plan_applies_trusted_global_path_rules_as_outer_guard() {
    let fixture = tempfile::tempdir().unwrap();
    let denied = fixture.path().join("protected");
    std::fs::create_dir(&denied).unwrap();
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
            denied.clone(),
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
    assert!(contains_sequence(
        &args,
        &["--tmpfs", denied.to_str().unwrap()]
    ));
}
