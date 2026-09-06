use crate::sandbox::{
    mounts::{MountOrigin, MountPlan, MountPlanError},
    os,
};
use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
use merry_process::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, BwrapSessionPermissions,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedAction,
    PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessEnvPolicy, ProcessRunner,
    ProcessRunnerContext,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tokio_util::sync::CancellationToken;

const RESOLVER_CONTENT: &str = "nameserver 192.0.2.1\n";

struct ResolverFixture {
    root: tempfile::TempDir,
    config: PathBuf,
    target: PathBuf,
    logical: PathBuf,
}

impl ResolverFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("resolver fixture");
        let config = root.path().join("host-etc");
        let target = root.path().join("resolver-data/resolv.data");
        let logical = config.join("resolv.conf");
        fs::create_dir_all(&config).expect("config directory");
        if Path::new("/etc/ld.so.cache").is_file() {
            fs::copy("/etc/ld.so.cache", config.join("ld.so.cache")).expect("loader cache fixture");
        }
        fs::create_dir_all(target.parent().unwrap()).expect("resolver directory");
        fs::write(&target, RESOLVER_CONTENT).expect("resolver file");
        fs::write(target.with_file_name("unexposed"), "private neighbour").expect("neighbour");
        fs::write(config.join("ordinary"), "ordinary config").expect("ordinary config");
        Self {
            root,
            config,
            target,
            logical,
        }
    }

    fn mounts(&self) -> MountPlan {
        let mut mounts = system_mounts();
        mounts.bind(
            &self.logical,
            Path::new("/etc/resolv.conf"),
            PathAccess::ReadOnly,
            false,
            MountOrigin::System,
        );
        mounts.bind(
            &self.config,
            Path::new("/etc"),
            PathAccess::ReadOnly,
            false,
            MountOrigin::Trusted,
        );
        mounts
    }

    fn assert_visible_and_readonly(&self) {
        let script = r#"
            test "$(cat /etc/resolv.conf)" = 'nameserver 192.0.2.1'
            test "$(cat /etc/ordinary)" = 'ordinary config'
            test ! -e "$1"
            if printf changed > /etc/resolv.conf; then exit 1; fi
        "#;
        let mut command = shell_command(script);
        command.push(self.target.with_file_name("unexposed").into_os_string());
        assert_success(execute(self.mounts(), &command));
        assert_eq!(fs::read_to_string(&self.target).unwrap(), RESOLVER_CONTENT);
    }
}

fn system_mounts() -> MountPlan {
    let mut mounts = MountPlan::default();
    for path in ["/usr", "/bin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            mounts.bind(
                Path::new(path),
                Path::new(path),
                PathAccess::ReadOnly,
                false,
                MountOrigin::System,
            );
        }
    }
    mounts
}

fn shell_command(script: &str) -> Vec<OsString> {
    vec![
        os("/bin/sh"),
        os("-eu"),
        os("-c"),
        os(script),
        os("mount-probe"),
    ]
}

fn outer_command(mounts: MountPlan, command: &[OsString]) -> Command {
    let mut args = vec![
        os("--unshare-user"),
        os("--unshare-ipc"),
        os("--unshare-pid"),
        os("--die-with-parent"),
        os("--proc"),
        os("/proc"),
        os("--dev"),
        os("/dev"),
        os("--tmpfs"),
        os("/tmp"),
        os("--tmpfs"),
        os("/home"),
        os("--dir"),
        os("/home/merry"),
    ];
    mounts.append_args(&mut args).expect("mount plan");
    args.push(os("--"));
    args.extend_from_slice(command);
    let mut process = Command::new("bwrap");
    process
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/home/merry");
    process
}

fn execute(mounts: MountPlan, command: &[OsString]) -> Output {
    outer_command(mounts, command)
        .output()
        .expect("bubblewrap for Linux sandbox tests")
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "sandbox failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn parent_bind_preserves_an_absolute_resolver_link_without_exposing_its_directory() {
    let fixture = ResolverFixture::new();
    symlink(&fixture.target, &fixture.logical).expect("resolver link");
    fixture.assert_visible_and_readonly();
}

#[test]
fn parent_bind_preserves_relative_and_multihop_resolver_links() {
    let fixture = ResolverFixture::new();
    let links = fixture.config.join("links");
    fs::create_dir(&links).expect("link directory");
    symlink(&fixture.target, links.join("next.conf")).expect("absolute target link");
    symlink("links/next.conf", &fixture.logical).expect("relative resolver link");
    fixture.assert_visible_and_readonly();
}

#[test]
fn parent_bind_preserves_resolver_links_through_a_directory_alias() {
    let fixture = ResolverFixture::new();
    symlink(
        fixture.target.parent().unwrap(),
        fixture.config.join("links"),
    )
    .expect("directory link");
    symlink("links/resolv.data", &fixture.logical).expect("relative resolver link");
    fixture.assert_visible_and_readonly();
}

#[test]
fn separately_imported_directory_alias_keeps_its_linked_child_readable() {
    let fixture = ResolverFixture::new();
    let alias = fixture.config.join("links");
    symlink(fixture.target.parent().unwrap(), &alias).expect("directory link");
    symlink("links/resolv.data", &fixture.logical).expect("relative resolver link");
    let mut mounts = fixture.mounts();
    mounts.bind(
        &alias,
        Path::new("/etc/links"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::System,
    );
    assert_success(execute(
        mounts,
        &shell_command("test \"$(cat /etc/resolv.conf)\" = 'nameserver 192.0.2.1'"),
    ));
}

#[test]
fn directory_alias_projection_follows_links_inside_the_imported_directory() {
    let fixture = ResolverFixture::new();
    let alias = fixture.config.join("links");
    let target = fixture.root.path().join("external/dns.conf");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, RESOLVER_CONTENT).unwrap();
    symlink(&target, fixture.target.with_file_name("next.conf")).unwrap();
    symlink(fixture.target.parent().unwrap(), &alias).expect("directory link");
    symlink("links/next.conf", &fixture.logical).expect("relative resolver link");
    let mut mounts = fixture.mounts();
    mounts.bind(
        &alias,
        Path::new("/etc/links"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::System,
    );
    assert_success(execute(
        mounts,
        &shell_command("test \"$(cat /etc/resolv.conf)\" = 'nameserver 192.0.2.1'"),
    ));
}

#[test]
fn resolver_target_under_a_second_parent_mount_is_not_overwritten() {
    let fixture = ResolverFixture::new();
    symlink(&fixture.target, &fixture.logical).expect("resolver link");
    let mut mounts = fixture.mounts();
    let parent = fixture.target.parent().unwrap();
    mounts.bind(
        parent,
        parent,
        PathAccess::ReadOnly,
        false,
        MountOrigin::Trusted,
    );
    assert_success(execute(
        mounts,
        &shell_command("test \"$(cat /etc/resolv.conf)\" = 'nameserver 192.0.2.1'"),
    ));
}

#[test]
fn denied_logical_or_resolved_parent_cannot_be_reexposed_by_a_file_mount() {
    for deny_target in [false, true] {
        let fixture = ResolverFixture::new();
        symlink(&fixture.target, &fixture.logical).expect("resolver link");
        let mut mounts = fixture.mounts();
        let denied = if deny_target {
            fixture.target.parent().unwrap()
        } else {
            Path::new("/etc")
        };
        mounts.rule(
            &PathAccessRule::new(
                denied,
                PathAccess::Deny,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
            MountOrigin::Trusted,
        );
        let mut command = shell_command("test ! -r /etc/resolv.conf; test ! -r \"$1\"");
        command.push(fixture.target.clone().into_os_string());
        assert_success(execute(mounts, &command));
        assert_eq!(
            fs::read_to_string(&fixture.target).unwrap(),
            RESOLVER_CONTENT
        );
    }
}

#[test]
fn readonly_rule_on_a_resolved_target_limits_a_writable_logical_file_mount() {
    let fixture = ResolverFixture::new();
    symlink(&fixture.target, &fixture.logical).expect("resolver link");
    let mut mounts = fixture.mounts();
    mounts.bind(
        &fixture.logical,
        Path::new("/etc/resolv.conf"),
        PathAccess::ReadWrite,
        false,
        MountOrigin::Workspace,
    );
    mounts.rule(
        &PathAccessRule::new(
            fixture.target.parent().unwrap(),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        MountOrigin::Trusted,
    );
    assert_success(execute(
        mounts,
        &shell_command(
            "test \"$(cat /etc/resolv.conf)\" = 'nameserver 192.0.2.1'; if printf changed > /etc/resolv.conf; then exit 1; fi",
        ),
    ));
    assert_eq!(
        fs::read_to_string(&fixture.target).unwrap(),
        RESOLVER_CONTENT
    );
}

#[test]
fn parent_ordering_preserves_product_write_exceptions_and_narrower_restrictions() {
    let root = tempfile::tempdir().expect("product fixture");
    let config = root.path().join("config");
    let managed = config.join("managed");
    let secrets = managed.join("secrets");
    fs::create_dir_all(&secrets).expect("product directories");
    fs::write(config.join("settings"), "settings").unwrap();
    fs::write(secrets.join("token"), "test secret").unwrap();
    let mut mounts = system_mounts();
    mounts.bind(
        &config,
        Path::new("/config"),
        PathAccess::ReadWrite,
        false,
        MountOrigin::Workspace,
    );
    mounts.bind(
        &config,
        Path::new("/config"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::Trusted,
    );
    mounts.bind(
        &managed,
        Path::new("/config/managed"),
        PathAccess::ReadWrite,
        false,
        MountOrigin::Product,
    );
    mounts.bind(
        &secrets,
        Path::new("/config/managed/secrets"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::ProductRestriction,
    );
    assert_success(execute(
        mounts,
        &shell_command(
            r#"
        printf created > /config/managed/created
        if printf changed > /config/settings; then exit 1; fi
        if printf changed > /config/managed/secrets/token; then exit 1; fi
    "#,
        ),
    ));
    assert_eq!(
        fs::read_to_string(managed.join("created")).unwrap(),
        "created"
    );
    assert_eq!(
        fs::read_to_string(secrets.join("token")).unwrap(),
        "test secret"
    );
}

#[test]
fn cyclic_links_in_an_imported_parent_fail_during_planning() {
    let fixture = ResolverFixture::new();
    symlink("next.conf", &fixture.logical).unwrap();
    symlink("resolv.conf", fixture.config.join("next.conf")).unwrap();
    let mut mounts = system_mounts();
    mounts.bind(
        &fixture.target,
        Path::new("/etc/resolv.conf"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::System,
    );
    mounts.bind(
        &fixture.config,
        Path::new("/etc"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::Trusted,
    );
    assert!(matches!(
        mounts.append_args(&mut Vec::new()),
        Err(MountPlanError::DestinationLoop { .. })
    ));
}

#[test]
fn outer_and_inner_preserve_system_reads_and_git_admission() {
    const CHILD_ENV: &str = "MERRY_BWRAP_MOUNT_TEST_CHILD";
    if std::env::var_os(CHILD_ENV).as_deref() == Some(std::ffi::OsStr::new("1")) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(assert_inner_admission());
        return;
    }
    let fixture = ResolverFixture::new();
    symlink(&fixture.target, &fixture.logical).unwrap();
    let workspace = fixture.root.path().join("workspace");
    fs::create_dir_all(workspace.join(".git")).unwrap();
    fs::create_dir_all(workspace.join("secrets")).unwrap();
    fs::write(workspace.join(".git/config"), "initial metadata").unwrap();
    fs::write(
        workspace.join("secrets/token"),
        "not visible to inner actions",
    )
    .unwrap();
    let executable = std::env::current_exe().expect("test executable");
    let mut mounts = fixture.mounts();
    mounts.bind(
        &executable,
        &executable,
        PathAccess::ReadOnly,
        false,
        MountOrigin::System,
    );
    mounts.bind(
        &workspace,
        Path::new("/workspace"),
        PathAccess::ReadWrite,
        false,
        MountOrigin::Workspace,
    );
    let command = vec![
        executable.into_os_string(),
        os("--exact"),
        os(
            "sandbox::tests::mount_execution::outer_and_inner_preserve_system_reads_and_git_admission",
        ),
        os("--nocapture"),
    ];
    assert_success(
        outer_command(mounts, &command)
            .env(CHILD_ENV, "1")
            .output()
            .expect("outer sandbox"),
    );
    assert_eq!(
        fs::read_to_string(workspace.join(".git/config")).unwrap(),
        "approved"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("ordinary")).unwrap(),
        "workspace write"
    );
}

async fn assert_inner_admission() {
    let session_permissions = BwrapSessionPermissions::new();
    let rules = [PathAccessRule::new(
        "/workspace/secrets",
        PathAccess::Deny,
        PathAccessRuleSource::TrustedGlobalConfig,
    )];
    let runner = BwrapProcessRunner::new_at_workspace_root("/workspace")
        .with_session_permissions(session_permissions.clone())
        .with_path_rules(rules.clone());
    let ordinary = process_intent(
        r#"
        test "$(cat /etc/resolv.conf)" = 'nameserver 192.0.2.1'
        test "$(cat .git/config)" = 'initial metadata'
        test ! -r secrets/token
        printf 'workspace write' > ordinary
        if printf changed > .git/config; then exit 1; fi
        if printf changed > /etc/resolv.conf; then exit 1; fi
    "#,
    );
    let output = runner
        .run(
            ordinary,
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("inner action");
    assert!(output.ok(), "inner admission failed: {output:?}");
    let factory = BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace")
        .with_session_permissions(session_permissions)
        .with_path_rules(rules);
    let call = PendingToolCall::new(
        ToolCallId::new("review-git-write").unwrap(),
        ToolName::new("request_permissions").unwrap(),
        ToolCallArguments::try_from(serde_json::json!({
            "requested": {"paths": [{"path": ".git", "access": "rw"}]},
            "for_action": {"command": "printf approved > .git/config", "cwd": null}
        }))
        .unwrap(),
    );
    let request = merry_runtime::parse_permission_request(&call).expect("permission request");
    factory
        .validate_request(&request)
        .expect("approved request validates");
    let PermissionedAction::Process(intent) = request.action();
    let approved = factory
        .runner_for(&request)
        .run(
            intent.clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("approved action");
    assert!(approved.ok(), "approved git action failed: {approved:?}");
    let repeated = runner
        .run(
            process_intent("printf unreviewed > .git/config"),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .expect("later action");
    assert!(
        !repeated.ok(),
        "git approval must not become a permanent write grant"
    );
}

fn process_intent(script: &str) -> ProcessActionIntent {
    ProcessActionIntent::new(
        vec!["/bin/sh".into(), "-eu".into(), "-c".into(), script.into()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        4096,
        4096,
    )
    .unwrap()
}

#[test]
fn readonly_parent_does_not_become_writable_through_an_earlier_workspace_child() {
    let root = tempfile::tempdir().expect("workspace fixture");
    let child = root.path().join("workspace");
    fs::create_dir(&child).unwrap();
    fs::write(child.join("existing"), "unchanged").unwrap();
    let mut mounts = system_mounts();
    mounts.bind(
        &child,
        Path::new("/data/workspace"),
        PathAccess::ReadWrite,
        false,
        MountOrigin::Workspace,
    );
    mounts.bind(
        root.path(),
        Path::new("/data"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::Trusted,
    );
    assert_success(execute(
        mounts,
        &shell_command(
            r#"
        test "$(cat /data/workspace/existing)" = unchanged
        if printf changed > /data/workspace/existing; then exit 1; fi
        if printf new > /data/workspace/new; then exit 1; fi
    "#,
        ),
    ));
    assert!(!child.join("new").exists());
    assert_eq!(
        fs::read_to_string(child.join("existing")).unwrap(),
        "unchanged"
    );
}

#[test]
fn missing_optional_parent_does_not_replace_an_existing_child_mount() {
    let root = tempfile::tempdir().expect("optional mount fixture");
    let child = root.path().join("file");
    fs::write(&child, "retained").unwrap();
    let mut mounts = system_mounts();
    mounts.bind(
        &child,
        Path::new("/optional/file"),
        PathAccess::ReadWrite,
        false,
        MountOrigin::Workspace,
    );
    mounts.bind(
        &root.path().join("missing"),
        Path::new("/optional"),
        PathAccess::ReadOnly,
        true,
        MountOrigin::Trusted,
    );
    assert_success(execute(
        mounts,
        &shell_command(
            "test \"$(cat /optional/file)\" = retained; printf written > /optional/file",
        ),
    ));
    assert_eq!(fs::read_to_string(child).unwrap(), "written");
}
