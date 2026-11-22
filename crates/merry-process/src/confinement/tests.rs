use super::*;

#[test]
fn exclusion_inside_a_grant_fails_closed() {
    let rules = [PathAccessRule::new(
        PathBuf::from("/workspace/secret"),
        PathAccess::Deny,
        merry_runtime::PathAccessRuleSource::TrustedGlobalConfig,
    )];
    let error = ensure_exclusions_are_enforceable(
        Path::new("/workspace"),
        Path::new("/tmp"),
        &[],
        &[PathBuf::from("/workspace")],
        &rules,
        true,
    )
    .expect_err("deny inside a grant");
    let message = error.to_string();
    assert!(message.contains("deny_paths"), "{message}");
    assert!(message.contains("/workspace/secret"), "{message}");
    assert!(message.contains("writable workspace"), "{message}");
}

#[test]
fn exclusion_outside_a_grant_is_enforceable() {
    let rules = [PathAccessRule::new(
        PathBuf::from("/home/alice/.ssh"),
        PathAccess::Deny,
        merry_runtime::PathAccessRuleSource::TrustedGlobalConfig,
    )];
    ensure_exclusions_are_enforceable(
        Path::new("/workspace"),
        Path::new("/tmp"),
        &[PathBuf::from("/usr")],
        &[PathBuf::from("/workspace")],
        &rules,
        true,
    )
    .expect("secrets outside the allow list need no hole");
}

#[test]
fn read_only_workspace_names_the_read_only_grant() {
    let rules = [PathAccessRule::new(
        PathBuf::from("/workspace/secret"),
        PathAccess::Deny,
        merry_runtime::PathAccessRuleSource::TrustedGlobalConfig,
    )];
    let error = ensure_exclusions_are_enforceable(
        Path::new("/workspace"),
        Path::new("/tmp"),
        &[PathBuf::from("/workspace")],
        &[PathBuf::from("/tmp")],
        &rules,
        false,
    )
    .expect_err("deny inside a read-only workspace");
    let message = error.to_string();
    assert!(message.contains("read-only workspace"), "{message}");
    assert!(message.contains("granted read-only"), "{message}");
}

#[test]
fn git_user_files_include_the_xdg_trio_and_skip_credentials() {
    let home = Path::new("/home/alice");
    let xdg = Path::new("/srv/xdg");
    let paths = git_user_file_candidates(home, Some(xdg));
    assert!(paths.iter().any(|path| path == &home.join(".gitconfig")));
    for name in ["config", "ignore", "attributes"] {
        assert!(
            paths
                .iter()
                .any(|path| path == &home.join(".config/git").join(name))
        );
        assert!(paths.iter().any(|path| path == &xdg.join("git").join(name)));
    }
    assert!(paths.iter().all(|path| !path.ends_with("git-credentials")));
    assert!(paths.iter().all(|path| !path.ends_with("credentials")));
}

#[test]
fn relative_xdg_config_home_is_not_a_git_grant() {
    assert!(absolute_xdg_config_home(Some(Path::new("relative"))).is_none());
    assert!(absolute_xdg_config_home(None).is_none());
}

#[test]
fn read_only_workspace_is_readable_and_tmp_stays_writable() {
    let launch = sample_launch(false, false);
    let resolved_tmp = crate::resolve_host_path(Path::new("/tmp"));
    assert!(contains_path(launch.read_only(), "/workspace/merry"));
    assert!(!contains_path(launch.read_write(), "/workspace/merry"));
    assert!(contains_path(
        launch.read_write(),
        resolved_tmp.to_str().expect("tmp path is utf-8")
    ));
    assert!(!launch.network());
}

#[test]
fn seatbelt_profile_writes_the_workspace_and_can_deny_network() {
    let closed = super::seatbelt::profile(&sample_launch(false, true));
    assert!(closed.contains("(deny default)"));
    assert!(closed.contains("(deny network*)"));
    assert!(closed.contains("(allow file-read-data (literal \"/\"))"));
    assert!(closed.contains("(allow file-write* (subpath \"/workspace/merry\"))"));
    assert!(
        closed.contains("(allow network-bind (local unix-socket (subpath \"/workspace/merry\")))")
    );
    assert!(
        closed.contains(
            "(allow network-outbound (remote unix-socket (subpath \"/workspace/merry\")))"
        )
    );
    assert!(!closed.contains("(allow network*)"));

    let open = super::seatbelt::profile(&sample_launch(true, true));
    assert!(open.contains("(allow network*)"));
    assert!(!open.contains("(deny network*)"));
    assert!(!open.contains("unix-socket"));
}

#[test]
fn seatbelt_profile_keeps_a_read_only_workspace_readable() {
    let profile = super::seatbelt::profile(&sample_launch(false, false));
    let resolved_tmp = crate::resolve_host_path(Path::new("/tmp"));
    assert!(profile.contains("(allow file-read* (subpath \"/workspace/merry\"))"));
    assert!(!profile.contains("file-write* (subpath \"/workspace/merry\")"));
    assert!(profile.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        resolved_tmp.display()
    )));
}

#[test]
fn existing_git_user_files_are_readable_and_credentials_are_not() {
    let home = fixture_root();
    let home = tempfile::tempdir_in(home).expect("fixture home");
    let home_path = home.path();
    for name in ["config", "ignore", "attributes"] {
        let path = home_path.join(".config/git").join(name);
        std::fs::create_dir_all(path.parent().expect("git config dir")).expect("mkdir");
        std::fs::write(&path, "sample\n").expect("write git file");
    }
    std::fs::write(home_path.join(".gitconfig"), "[user]\n").expect("gitconfig");
    let credentials = home_path.join(".git-credentials");
    std::fs::write(&credentials, "secret\n").expect("credentials");

    let environment =
        WorkspaceProcessEnvironment::new("/usr/bin", home_path, "/tmp").expect("environment");
    let launch = prepare_launch(
        &sample_intent(),
        Path::new("/workspace/merry"),
        &environment,
        false,
        &[],
        true,
    )
    .expect("launch");
    for name in ["config", "ignore", "attributes"] {
        let path = crate::resolve_host_path(&home_path.join(".config/git").join(name));
        assert!(
            contains_path(launch.read_only(), path.to_str().expect("utf-8")),
            "missing {path:?} in {:?}",
            launch.read_only()
        );
    }
    let gitconfig = crate::resolve_host_path(&home_path.join(".gitconfig"));
    assert!(contains_path(
        launch.read_only(),
        gitconfig.to_str().expect("utf-8")
    ));
    let credentials = crate::resolve_host_path(&credentials);
    assert!(!contains_path(
        launch.read_only(),
        credentials.to_str().expect("utf-8")
    ));

    let denied = PathAccessRule::new(
        home_path.join(".config/git/ignore"),
        PathAccess::Deny,
        merry_runtime::PathAccessRuleSource::TrustedGlobalConfig,
    );
    let blocked = prepare_launch(
        &sample_intent(),
        Path::new("/workspace/merry"),
        &environment,
        false,
        &[denied],
        true,
    )
    .expect("a denied ignore file outside other grants still launches");
    let ignore = crate::resolve_host_path(&home_path.join(".config/git/ignore"));
    assert!(!contains_path(
        blocked.read_only(),
        ignore.to_str().expect("utf-8")
    ));
}

fn sample_launch(network: bool, workspace_writable: bool) -> SandboxLaunch {
    let environment =
        WorkspaceProcessEnvironment::new("/usr/bin", "/home/alice", "/tmp").expect("environment");
    prepare_launch(
        &sample_intent(),
        Path::new("/workspace/merry"),
        &environment,
        network,
        &[],
        workspace_writable,
    )
    .expect("sample launch")
}

fn sample_intent() -> merry_runtime::ProcessActionIntent {
    merry_runtime::ProcessActionIntent::new(
        vec!["pwd".to_owned()],
        None,
        merry_runtime::ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("intent")
}

fn contains_path(paths: &[String], expected: &str) -> bool {
    paths.iter().any(|path| path == expected)
}

fn fixture_root() -> PathBuf {
    for candidate in [env::var_os("XDG_RUNTIME_DIR"), env::var_os("HOME")] {
        let Some(value) = candidate else {
            continue;
        };
        let path = PathBuf::from(value);
        if path.is_dir() && !is_baseline_write(&path) {
            return path;
        }
    }
    panic!("need a directory outside /tmp, /var/tmp, and /dev");
}

fn is_baseline_write(path: &Path) -> bool {
    [
        "/tmp",
        "/private/tmp",
        "/var/tmp",
        "/private/var/tmp",
        "/var/folders",
        "/private/var/folders",
        "/dev",
        "/dev/shm",
        "/dev/pts",
    ]
    .iter()
    .any(|root| path == Path::new(root) || path.starts_with(root))
}

#[test]
fn cargo_debug_helper_resolves_legacy_deps_and_build_dir_layouts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = temp.path().join("target/debug");
    let helper = profile.join("merry-sandbox-helper");
    let deps_bin = profile.join("deps/merry-123456");
    let build_bin = profile.join("build/merry-cli/abcdef01/out/merry-abcdef01");
    fs::create_dir_all(deps_bin.parent().expect("deps dir")).expect("create deps dir");
    fs::create_dir_all(build_bin.parent().expect("out dir")).expect("create out dir");
    fs::write(&helper, b"").expect("create helper stub");

    assert_eq!(
        cargo_debug_helper(&deps_bin).as_deref(),
        Some(helper.as_path())
    );
    assert_eq!(
        cargo_debug_helper(&build_bin).as_deref(),
        Some(helper.as_path())
    );
    assert_eq!(cargo_debug_helper(&profile.join("merry")), None);
}

#[test]
fn seatbelt_command_args_invokes_sandbox_exec_directly_with_profile_and_target() {
    let launch = sample_launch(false, true);
    let args = super::seatbelt::command_args(&launch);
    assert_eq!(args[0], "-p");
    assert_eq!(args[1], super::seatbelt::profile(&launch));
    assert_eq!(args[2], "--");
    assert_eq!(args[3], "pwd");
    assert_eq!(args.len(), 4);
}

#[test]
fn seatbelt_profile_emits_deny_rules_after_allows_for_product_private() {
    let environment =
        WorkspaceProcessEnvironment::new("/usr/bin", "/home/alice", "/tmp").expect("environment");
    let rules = [
        PathAccessRule::new(
            PathBuf::from("/home/alice/.local/share"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            PathBuf::from("/home/alice/.local/share/merry/providers"),
            PathAccess::Deny,
            PathAccessRuleSource::ProductPrivate,
        ),
    ];
    let launch = prepare_launch(
        &sample_intent(),
        Path::new("/workspace/merry"),
        &environment,
        false,
        &rules,
        true,
    )
    .expect("user grant covering ProductPrivate should still prepare a launch");
    let profile = super::seatbelt::profile(&launch);
    let allow_idx = profile
        .find("(allow file-write* (subpath \"/home/alice/.local/share\"))")
        .expect("allow rule for parent grant");
    let deny_read_idx = profile
        .find("(deny file-read* (subpath \"/home/alice/.local/share/merry/providers\"))")
        .expect("deny read rule for ProductPrivate");
    let deny_write_idx = profile
        .find("(deny file-write* (subpath \"/home/alice/.local/share/merry/providers\"))")
        .expect("deny write rule for ProductPrivate");
    assert!(deny_read_idx > allow_idx);
    assert!(deny_write_idx > allow_idx);
}

#[cfg(target_os = "linux")]
#[test]
fn proc_self_exe_remains_executable_after_unlinking_running_binary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sh_source = fs::canonicalize("/bin/sh").expect("canonical /bin/sh");
    let sh_copy = temp.path().join("ephemeral-sh");
    fs::copy(&sh_source, &sh_copy).expect("copy sh binary");

    let output = Command::new(&sh_copy)
        .arg("-c")
        .arg("rm -f \"$1\" && exec /proc/self/exe -c 'printf still-alive'")
        .arg("ephemeral-sh")
        .arg(&sh_copy)
        .output()
        .expect("spawn copied shell");

    assert!(
        output.status.success(),
        "re-exec via /proc/self/exe should survive binary unlink: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "still-alive");
    assert!(
        !sh_copy.exists(),
        "copied binary should have been unlinked before re-exec"
    );
}
