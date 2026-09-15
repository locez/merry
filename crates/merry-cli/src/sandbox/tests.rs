#[cfg(target_os = "linux")]
use crate::{config::MerryConfig, sandbox::host::current_process_uid};
use crate::{
    config::XdgPaths,
    sandbox::{
        Bootstrap, ClipboardAccess, Error, Plan,
        host::{Host, HostPathKind, HostPathMetadata, HostPathProbe},
        integrations::{GraphicalEnvironment, HostIntegrationEnvironment},
        os, plan_bootstrap_with_file_exists, plan_bootstrap_with_probe,
    },
};
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[cfg(target_os = "linux")]
use std::{env, fs};

#[derive(Default)]
struct FakeHostProbe {
    metadata: BTreeMap<PathBuf, HostPathMetadata>,
}

impl FakeHostProbe {
    fn socket(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::UnixSocket, owner_uid, 0o600),
        );
        self
    }

    fn regular_file(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::RegularFile, owner_uid, 0o600),
        );
        self
    }

    fn other(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::Other, owner_uid, 0o600),
        );
        self
    }

    fn directory(mut self, path: &str, owner_uid: u32, mode: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::Directory, owner_uid, mode),
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
        host_integrations: Vec::new(),
        host_integration_environment: HostIntegrationEnvironment::default(),
        development_environment: Vec::new(),
        current_uid: 1_000,
        review_terminal_device: None,
    }
}

fn path_is_fake_bwrap(path: &Path) -> bool {
    path == Path::new("/custom/bin/bwrap")
}

/// Builds the host fixture an integration test re-enters with: a private home
/// holding `permissions`, the current test binary as the sandbox command, and
/// the same config the child half reloads through its own XDG paths.
#[cfg(target_os = "linux")]
fn integration_host(home: &Path, workspace: &Path, permissions: &str) -> Host {
    let mut host = sandbox_host();
    host.cwd = workspace.to_path_buf();
    host.current_exe = env::current_exe().unwrap();
    host.path = Some(os("/usr/bin:/bin"));
    host.args.clear();
    host.current_uid = current_process_uid().unwrap();
    host.xdg_paths = XdgPaths::from_parts(home.to_path_buf(), None, None);
    fs::create_dir_all(host.xdg_paths.config_dir()).unwrap();
    fs::write(host.xdg_paths.config_file(), permissions).unwrap();
    let config = MerryConfig::load_optional(&host.xdg_paths)
        .unwrap()
        .unwrap();
    host.host_integrations = config.host_integrations();
    host.trusted_path_rules = config.trusted_global_path_rules().unwrap();
    host.trusted_path_rules.push(PathAccessRule::new(
        &host.current_exe,
        PathAccess::ReadOnly,
        PathAccessRuleSource::TrustedGlobalConfig,
    ));
    host
}

/// Re-enters the test binary inside `plan`'s sandbox and asserts the named child
/// test reported exactly one passing test.
#[cfg(target_os = "linux")]
fn assert_sandbox_child_ran(plan: &mut Plan, host: &Host, marker: &str, test_path: &str) {
    plan.args.extend(reentry::sandboxed_reentry_arguments(
        marker,
        &host.current_exe,
        test_path,
    ));
    reentry::assert_child_passed(&reentry::run_plan(plan), test_path);
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

fn assert_ro_mount(args: &[String], flag: &str, source: &str, destination: &str) {
    let resolved_source = merry_process::resolve_bwrap_path(Path::new(source));
    assert!(
        contains_sequence(
            args,
            &[
                flag,
                resolved_source.to_str().expect("UTF-8 test path"),
                destination,
            ],
        ),
        "missing mount {flag} {} {destination}",
        resolved_source.display()
    );
}

fn contains_sequence(args: &[String], expected: &[&str]) -> bool {
    sequence_position(args, expected).is_some()
}

fn sequence_position(args: &[String], expected: &[&str]) -> Option<usize> {
    args.windows(expected.len()).position(|window| {
        window
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
    })
}

mod bootstrap;

mod environment;

mod host_integrations;

mod mount_rules;

mod runtime_evidence;

#[cfg(target_os = "linux")]
mod mount_execution;

#[cfg(target_os = "linux")]
mod reentry;

#[cfg(target_os = "linux")]
mod gpg_public;

#[cfg(target_os = "linux")]
mod ssh;

#[test]
fn nested_sandbox_diagnostic_names_the_cause_and_the_setup_document() {
    let error = Error::NestedSandboxUnavailable {
        stderr: "bwrap: No permissions to create new namespace".to_owned(),
        apparmor_profile: Some(PathBuf::from("/etc/apparmor.d/bwrap-userns-restrict")),
    };
    let message = error.to_string();

    assert!(message.starts_with("warning: bubblewrap cannot start inside Merry's outer sandbox"));
    assert!(message.contains("bwrap: No permissions to create new namespace"));
    assert!(message.contains("/etc/apparmor.d/bwrap-userns-restrict"));
    assert!(message.contains("SANDBOX.md"));
    assert!(message.contains("--inner-sandbox"));
}

#[test]
fn nested_sandbox_diagnostic_omits_apparmor_hint_without_the_profile() {
    let error = Error::NestedSandboxUnavailable {
        stderr: "bwrap: setting up uid map: Permission denied".to_owned(),
        apparmor_profile: None,
    };
    let message = error.to_string();

    assert!(!message.contains("AppArmor"));
    assert!(message.contains("SANDBOX.md"));
    assert!(message.contains("--inner-sandbox"));
}
