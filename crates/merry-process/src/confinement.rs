//! Per-command workspace confinement.
//!
//! Linux applies Landlock to the filesystem and, when that command was not
//! granted network, a seccomp filter that rejects IPv4 and IPv6 sockets.
//! macOS applies a Seatbelt profile through `sandbox-exec`. The Merry process
//! stays unrestricted; only the helper that execs the command is confined.
//!
//! Landlock is an allow list. A deny or review path that sits inside an
//! allowed directory cannot be subtracted, so preparing a launch fails instead
//! of silently exposing that path.

use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource, ProcessActionIntent,
    ProcessRunnerError,
};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::process_runner::{WorkspaceProcessEnvironment, process_current_dir};
use crate::resolve_host_path;

mod seatbelt;

const HELPER_ENV: &str = "MERRY_SANDBOX_HELPER";
const POLICY_ENV: &str = "MERRY_SANDBOX_POLICY";
const SETUP_EXIT_CODE: i32 = 126;

const READ_ONLY_DIRECTORIES: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/opt",
    "/proc",
    "/sys",
    "/etc/ld.so.conf.d",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
    "/etc/alternatives",
];

const READ_ONLY_FILES: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/passwd",
    "/etc/group",
    "/etc/localtime",
    "/etc/gitconfig",
];

const READ_WRITE_DIRECTORIES: &[&str] = &["/tmp", "/var/tmp", "/dev", "/dev/shm", "/dev/pts"];

/// One command plus the confinement the helper applies before `exec`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SandboxLaunch {
    program: String,
    args: Vec<String>,
    cwd: String,
    env: Vec<(String, String)>,
    remove_env: Vec<String>,
    network: bool,
    read_only: Vec<String>,
    read_write: Vec<String>,
    traverse: Vec<String>,
}

impl SandboxLaunch {
    pub(crate) fn network(&self) -> bool {
        self.network
    }

    #[cfg(test)]
    pub(crate) fn read_only(&self) -> &[String] {
        &self.read_only
    }

    #[cfg(test)]
    pub(crate) fn read_write(&self) -> &[String] {
        &self.read_write
    }

    #[cfg(test)]
    pub(crate) fn env(&self) -> &[(String, String)] {
        &self.env
    }

    #[cfg(test)]
    pub(crate) fn remove_env(&self) -> &[String] {
        &self.remove_env
    }
}

/// Reports whether this process was spawned as the sandbox helper.
#[must_use]
pub fn sandbox_helper_requested() -> bool {
    env::var_os(HELPER_ENV).is_some()
}

/// Applies the policy named by [`POLICY_ENV`](Self) and executes the command.
///
/// On success this function does not return. Setup failures print a
/// `merry-sandbox:` line and exit [`SETUP_EXIT_CODE`].
pub fn sandbox_helper_main() -> ! {
    if let Err(error) = apply_and_exec() {
        eprintln!("merry-sandbox: {error}");
        std::process::exit(SETUP_EXIT_CODE);
    }
    unreachable!("sandbox helper exec returns only on failure");
}

/// Checks that the kernel sandbox used for workspace mode can be created.
///
/// This does not confine the calling process.
pub fn probe_workspace_sandbox() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::probe()
    }
    #[cfg(target_os = "macos")]
    {
        seatbelt::probe()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(format!(
            "workspace sandbox is unavailable on {}",
            std::env::consts::OS
        ))
    }
}

pub(crate) fn prepare_launch(
    intent: &ProcessActionIntent,
    cwd_root: &Path,
    environment: &WorkspaceProcessEnvironment,
    network_allowed: bool,
    path_rules: &[PathAccessRule],
    workspace_writable: bool,
) -> Result<SandboxLaunch, ProcessRunnerError> {
    let Some((program, args)) = intent.argv().split_first() else {
        return Err(ProcessRunnerError::infrastructure(
            "validated process argv was unexpectedly empty",
        ));
    };
    let cwd = process_current_dir(Some(cwd_root), intent);
    let mut read_write = Vec::new();
    let mut read_only = Vec::new();
    if workspace_writable {
        push_directory(&mut read_write, cwd_root);
        push_directory(&mut read_write, &cwd);
    } else {
        push_directory(&mut read_only, cwd_root);
        push_directory(&mut read_only, &cwd);
    }
    for path in READ_WRITE_DIRECTORIES {
        push_directory(&mut read_write, Path::new(path));
    }
    push_directory(&mut read_write, environment.temporary_directory());
    for path in READ_ONLY_DIRECTORIES {
        push_if_exists(&mut read_only, Path::new(path));
    }
    for path in READ_ONLY_FILES {
        push_if_exists(&mut read_only, Path::new(path));
    }
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    push_git_user_files(
        &mut read_only,
        environment.home_path(),
        xdg_config_home.as_deref(),
        path_rules,
    );

    for rule in path_rules {
        if rule.review_required() || rule.access() == PathAccess::Deny {
            continue;
        }
        let path = resolve_host_path(rule.path());
        match rule.access() {
            PathAccess::ReadWrite => push_path(&mut read_write, &path),
            PathAccess::ReadOnly => push_path(&mut read_only, &path),
            PathAccess::Deny => {}
        }
    }
    for path in environment.sandbox_socket_paths() {
        let path = resolve_host_path(&path);
        if !blocked_by_policy(&path, path_rules) {
            push_path(&mut read_write, &path);
        }
    }
    for path in environment.ssh_known_hosts_paths() {
        if !blocked_by_policy(&path, path_rules) {
            push_if_exists(&mut read_only, &path);
        }
    }
    for path in environment.gpg_public_keyrings() {
        if !blocked_by_policy(&path, path_rules) {
            push_if_exists(&mut read_only, &path);
        }
    }

    ensure_exclusions_are_enforceable(
        cwd_root,
        environment.temporary_directory(),
        &read_only,
        &read_write,
        path_rules,
        workspace_writable,
    )?;
    prepare_writable_directories(&read_write);

    let mut remove_env = vec![HELPER_ENV.to_owned(), POLICY_ENV.to_owned()];
    let mut env = vec![
        (
            "PATH".to_owned(),
            os_to_string(environment.path_value(), "PATH")?,
        ),
        (
            "HOME".to_owned(),
            path_to_string(environment.home_path(), "HOME")?,
        ),
        (
            "TMPDIR".to_owned(),
            path_to_string(environment.temporary_directory(), "TMPDIR")?,
        ),
        ("PWD".to_owned(), path_to_string(&cwd, "process cwd")?),
    ];
    for (name, value) in environment.environment_overrides() {
        let name = os_to_string(name, "sandbox process environment name")?;
        let value = os_to_string(value, "sandbox process environment value")?;
        env.retain(|(existing, _)| existing != &name);
        env.push((name, value));
    }
    append_host_integration_env(environment, &mut env, &mut remove_env)?;

    let traverse = traverse_ancestors(&read_only, &read_write);
    Ok(SandboxLaunch {
        program: program.clone(),
        args: args.to_vec(),
        cwd: path_to_string(&cwd, "process cwd")?,
        env,
        remove_env,
        network: network_allowed,
        read_only: strings_from_paths(&read_only)?,
        read_write: strings_from_paths(&read_write)?,
        traverse: strings_from_paths(&traverse)?,
    })
}

pub(crate) fn helper_program() -> PathBuf {
    if let Some(path) = env::var_os(HELPER_ENV).filter(|value| Path::new(value).is_file()) {
        return PathBuf::from(path);
    }
    if let Ok(current) = env::current_exe() {
        if let Some(helper) = cargo_debug_helper(&current) {
            return helper;
        }
        return current;
    }
    PathBuf::from("merry")
}

pub(crate) fn write_policy(launch: &SandboxLaunch) -> Result<PathBuf, ProcessRunnerError> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = env::temp_dir().join(format!(
        "merry-sandbox-{}-{sequence}.json",
        std::process::id()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|source| {
        ProcessRunnerError::infrastructure(format!(
            "failed to create sandbox policy {}: {source}",
            path.display()
        ))
    })?;
    let bytes = serde_json::to_vec(launch).map_err(|source| {
        ProcessRunnerError::infrastructure(format!("failed to encode sandbox policy: {source}"))
    })?;
    file.write_all(&bytes).map_err(|source| {
        ProcessRunnerError::infrastructure(format!(
            "failed to write sandbox policy {}: {source}",
            path.display()
        ))
    })?;
    Ok(path)
}

pub(crate) fn policy_env_name() -> &'static str {
    POLICY_ENV
}

pub(crate) fn helper_env_name() -> &'static str {
    HELPER_ENV
}

pub(crate) fn setup_failure(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .find(|line| line.starts_with("merry-sandbox:"))
        .map(|line| line.trim().to_owned())
}

fn apply_and_exec() -> Result<(), String> {
    let path = env::var(POLICY_ENV).map_err(|_| "sandbox helper was started without a policy")?;
    let bytes =
        fs::read(&path).map_err(|source| format!("failed to read sandbox policy: {source}"))?;
    let _ = fs::remove_file(&path);
    let launch: SandboxLaunch = serde_json::from_slice(&bytes)
        .map_err(|source| format!("sandbox policy is invalid: {source}"))?;
    apply_confinement(&launch)?;
    let mut command = Command::new(&launch.program);
    command.args(&launch.args).current_dir(&launch.cwd);
    for (name, value) in &launch.env {
        command.env(name, value);
    }
    for name in &launch.remove_env {
        command.env_remove(name);
    }
    command.env_remove(HELPER_ENV);
    command.env_remove(POLICY_ENV);
    let error = command.exec_error();
    Err(format!("failed to execute `{}`: {error}", launch.program))
}

fn apply_confinement(launch: &SandboxLaunch) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::apply(launch)
    }
    #[cfg(target_os = "macos")]
    {
        seatbelt::apply(launch)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = launch;
        Err(format!(
            "workspace sandbox is unavailable on {}",
            std::env::consts::OS
        ))
    }
}

fn blocked_by_policy(path: &Path, rules: &[PathAccessRule]) -> bool {
    let path = resolve_host_path(path);
    rules.iter().any(|rule| {
        let blocked = rule.access() == PathAccess::Deny || rule.review_required();
        if !blocked {
            return false;
        }
        let rule_path = resolve_host_path(rule.path());
        path == rule_path || path.starts_with(&rule_path)
    })
}

fn ensure_exclusions_are_enforceable(
    workspace: &Path,
    temporary_directory: &Path,
    read_only: &[PathBuf],
    read_write: &[PathBuf],
    rules: &[PathAccessRule],
    workspace_writable: bool,
) -> Result<(), ProcessRunnerError> {
    let mut conflicts = Vec::new();
    for rule in rules {
        let excludes = rule.access() == PathAccess::Deny || rule.review_required();
        if !excludes {
            continue;
        }
        let path = resolve_host_path(rule.path());
        let covered = read_write
            .iter()
            .chain(read_only.iter())
            .find(|grant| path == **grant || path.starts_with(grant));
        if let Some(grant) = covered {
            conflicts.push(sandbox_rule_conflict(
                rule,
                &path,
                grant,
                workspace,
                temporary_directory,
                rules,
                workspace_writable,
            ));
        }
    }
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(ProcessRunnerError::infrastructure(conflicts.join(" ")))
    }
}

fn sandbox_rule_conflict(
    rule: &PathAccessRule,
    path: &Path,
    grant: &Path,
    workspace: &Path,
    temporary_directory: &Path,
    rules: &[PathAccessRule],
    workspace_writable: bool,
) -> String {
    format!(
        "conflicting sandbox rules: {} is inside {}. {}",
        blocked_rule_label(rule, path),
        allowed_directory_label(
            grant,
            workspace,
            temporary_directory,
            rules,
            workspace_writable
        ),
        conflict_fix(rule, path, grant, workspace, rules, workspace_writable)
    )
}

fn blocked_rule_label(rule: &PathAccessRule, path: &Path) -> String {
    if rule.review_required() {
        return format!("review_paths entry `{}`", path.display());
    }
    match rule.source() {
        PathAccessRuleSource::ProductPrivate => {
            format!("Merry's private directory `{}`", path.display())
        }
        PathAccessRuleSource::TrustedGlobalConfig => {
            format!("deny_paths entry `{}`", path.display())
        }
        _ => format!("denied path `{}`", path.display()),
    }
}

fn allowed_directory_label(
    grant: &Path,
    workspace: &Path,
    temporary_directory: &Path,
    rules: &[PathAccessRule],
    workspace_writable: bool,
) -> String {
    if grant == resolve_host_path(workspace) {
        return if workspace_writable {
            format!("the writable workspace `{}`", grant.display())
        } else {
            format!("the read-only workspace `{}`", grant.display())
        };
    }
    if READ_WRITE_DIRECTORIES
        .iter()
        .any(|directory| Path::new(directory) == grant)
    {
        return format!(
            "`{}`, which every sandboxed command can write",
            grant.display()
        );
    }
    if let Some(rule) = rules.iter().find(|rule| {
        !rule.review_required()
            && rule.access() != PathAccess::Deny
            && resolve_host_path(rule.path()) == grant
    }) {
        return match (rule.source(), rule.access()) {
            (PathAccessRuleSource::TrustedGlobalConfig, PathAccess::ReadWrite) => {
                format!("readwrite_paths entry `{}`", grant.display())
            }
            (PathAccessRuleSource::TrustedGlobalConfig, PathAccess::ReadOnly) => {
                format!("readonly_paths entry `{}`", grant.display())
            }
            (PathAccessRuleSource::DefaultDevelopmentBaseline, PathAccess::ReadWrite) => {
                format!("the writable build directory `{}`", grant.display())
            }
            (PathAccessRuleSource::DefaultDevelopmentBaseline, PathAccess::ReadOnly) => {
                format!("the readable toolchain directory `{}`", grant.display())
            }
            (PathAccessRuleSource::PermissionReview, PathAccess::ReadWrite) => {
                format!("the approved writable path `{}`", grant.display())
            }
            (PathAccessRuleSource::PermissionReview, PathAccess::ReadOnly) => {
                format!("the approved readable path `{}`", grant.display())
            }
            _ => format!("allowed directory `{}`", grant.display()),
        };
    }
    if grant == resolve_host_path(temporary_directory) {
        return format!(
            "TMPDIR `{}`, which every sandboxed command can write",
            grant.display()
        );
    }
    format!("allowed directory `{}`", grant.display())
}

fn conflict_fix(
    rule: &PathAccessRule,
    path: &Path,
    grant: &Path,
    workspace: &Path,
    rules: &[PathAccessRule],
    workspace_writable: bool,
) -> String {
    if rule.source() == PathAccessRuleSource::ProductPrivate && !rule.review_required() {
        return format!(
            "Move Merry's config and state directories out of `{}`, or pass --sandbox off.",
            grant.display()
        );
    }
    let key = if rule.review_required() {
        "review_paths"
    } else {
        "deny_paths"
    };
    if grant == resolve_host_path(workspace) {
        let reason = if workspace_writable {
            "The workspace stays writable, so a path inside it cannot be blocked."
        } else {
            "The workspace is granted read-only, so a path inside it cannot be blocked."
        };
        return format!("Remove `{}` from {key}. {reason}", path.display());
    }
    let parent_is_configured_write = rules.iter().any(|candidate| {
        candidate.source() == PathAccessRuleSource::TrustedGlobalConfig
            && candidate.access() == PathAccess::ReadWrite
            && !candidate.review_required()
            && resolve_host_path(candidate.path()) == grant
    });
    if rule.review_required() && parent_is_configured_write {
        return format!(
            "Remove `{}` from review_paths, or remove `{}` from readwrite_paths.",
            path.display(),
            grant.display()
        );
    }
    format!(
        "Remove `{}` from {key}. It has to be outside `{}`.",
        path.display(),
        grant.display()
    )
}

fn prepare_writable_directories(paths: &[PathBuf]) {
    for path in paths {
        if path.exists() || is_preexisting_system_path(path) {
            continue;
        }
        let _ = fs::create_dir_all(path);
    }
}

fn is_preexisting_system_path(path: &Path) -> bool {
    READ_WRITE_DIRECTORIES
        .iter()
        .chain(READ_ONLY_DIRECTORIES.iter())
        .any(|system| Path::new(system) == path)
}

fn traverse_ancestors(read_only: &[PathBuf], read_write: &[PathBuf]) -> Vec<PathBuf> {
    let mut ancestors = Vec::new();
    for path in read_only.iter().chain(read_write.iter()) {
        let mut parent = path.as_path();
        while let Some(next) = parent.parent() {
            if next.as_os_str().is_empty() {
                break;
            }
            if !read_only
                .iter()
                .chain(read_write.iter())
                .any(|grant| grant == next)
                && !ancestors.iter().any(|existing| existing == next)
            {
                ancestors.push(next.to_path_buf());
            }
            parent = next;
        }
    }
    ancestors
}

fn append_host_integration_env(
    environment: &WorkspaceProcessEnvironment,
    env: &mut Vec<(String, String)>,
    remove_env: &mut Vec<String>,
) -> Result<(), ProcessRunnerError> {
    if !environment.integration_enabled(HostIntegration::SshAgent) {
        remove_env.push("SSH_AUTH_SOCK".to_owned());
    }
    if !environment.integration_enabled(HostIntegration::SessionBus) {
        remove_env.push("DBUS_SESSION_BUS_ADDRESS".to_owned());
    }
    if !environment.integration_enabled(HostIntegration::GpgAgent) {
        remove_env.push("GNUPGHOME".to_owned());
    }
    for (name, value) in environment.integration_environment() {
        env.retain(|(existing, _)| existing != &name);
        remove_env.retain(|existing| existing != &name);
        env.push((name, value));
    }
    Ok(())
}

fn push_git_user_files(
    read_only: &mut Vec<PathBuf>,
    home: &Path,
    xdg_config_home: Option<&Path>,
    rules: &[PathAccessRule],
) {
    for path in git_user_file_candidates(home, absolute_xdg_config_home(xdg_config_home)) {
        if blocked_by_policy(&path, rules) {
            continue;
        }
        push_if_exists(read_only, &path);
    }
}

/// Git's default user files. A custom `core.excludesFile` is not followed.
///
/// `~/.git-credentials` and `git/credentials` stay off this list. Shell startup
/// files stay off it too: home remains traverse-only.
fn git_user_file_candidates(home: &Path, xdg_config_home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![home.join(".gitconfig")];
    for root in [
        Some(home.join(".config")),
        xdg_config_home.map(Path::to_path_buf),
    ]
    .into_iter()
    .flatten()
    {
        let git = root.join("git");
        for name in ["config", "ignore", "attributes"] {
            paths.push(git.join(name));
        }
    }
    paths
}

fn absolute_xdg_config_home(value: Option<&Path>) -> Option<&Path> {
    value.filter(|path| path.is_absolute())
}

fn push_directory(paths: &mut Vec<PathBuf>, path: &Path) {
    push_path(paths, &resolve_host_path(path));
}

fn push_if_exists(paths: &mut Vec<PathBuf>, path: &Path) {
    if path.exists() {
        push_path(paths, &resolve_host_path(path));
    }
}

fn push_path(paths: &mut Vec<PathBuf>, path: &Path) {
    if path.as_os_str().is_empty() || paths.iter().any(|existing| existing == path) {
        return;
    }
    paths.push(path.to_path_buf());
}

fn strings_from_paths(paths: &[PathBuf]) -> Result<Vec<String>, ProcessRunnerError> {
    paths
        .iter()
        .map(|path| path_to_string(path, "sandbox path"))
        .collect()
}

fn path_to_string(path: &Path, label: &str) -> Result<String, ProcessRunnerError> {
    path.to_str()
        .filter(|value| !value.contains('\0'))
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            ProcessRunnerError::infrastructure(format!(
                "{label} `{}` must be valid UTF-8",
                path.display()
            ))
        })
}

fn os_to_string(value: &std::ffi::OsStr, label: &str) -> Result<String, ProcessRunnerError> {
    value
        .to_str()
        .filter(|text| !text.contains('\0'))
        .map(ToOwned::to_owned)
        .ok_or_else(|| ProcessRunnerError::infrastructure(format!("{label} must be valid UTF-8")))
}

fn cargo_debug_helper(current: &Path) -> Option<PathBuf> {
    let parent = current.parent()?;
    if parent.file_name()? != "deps" {
        return None;
    }
    let helper = parent.parent()?.join("merry-sandbox-helper");
    helper.is_file().then_some(helper)
}

trait CommandExec {
    fn exec_error(&mut self) -> std::io::Error;
}

impl CommandExec for Command {
    fn exec_error(&mut self) -> std::io::Error {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            self.exec()
        }
        #[cfg(not(unix))]
        {
            std::io::Error::other("workspace sandbox helper requires a Unix host")
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::SandboxLaunch;
    use landlock::{
        ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus,
    };
    use seccompiler::{
        SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule,
    };
    use std::collections::BTreeMap;

    pub(super) fn probe() -> Result<(), String> {
        let ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(ABI::V1))
            .map_err(|error| error.to_string())?
            .create()
            .map_err(|error| error.to_string())?;
        drop(ruleset);
        Ok(())
    }

    pub(super) fn apply(launch: &SandboxLaunch) -> Result<(), String> {
        apply_landlock(launch)?;
        if !launch.network() {
            apply_network_seccomp()?;
        }
        Ok(())
    }

    fn apply_landlock(launch: &SandboxLaunch) -> Result<(), String> {
        let abi = ABI::V5;
        let ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .map_err(|error| error.to_string())?
            .create()
            .map_err(|error| error.to_string())?
            .set_compatibility(CompatLevel::BestEffort);
        let ruleset = add_paths(ruleset, &launch.read_only, AccessFs::from_read(abi))?;
        let ruleset = add_paths(ruleset, &launch.read_write, AccessFs::from_all(abi))?;
        let ruleset = add_paths(
            ruleset,
            &launch.traverse,
            landlock::BitFlags::from(AccessFs::Execute),
        )?;
        let status = ruleset
            .restrict_self()
            .map_err(|error| format!("Landlock restriction failed: {error}"))?;
        if status.ruleset == RulesetStatus::NotEnforced {
            return Err(
                "Landlock could not enforce the workspace sandbox. Pass --sandbox off to run without it."
                    .to_owned(),
            );
        }
        Ok(())
    }

    fn add_paths(
        ruleset: landlock::RulesetCreated,
        paths: &[String],
        access: landlock::BitFlags<AccessFs>,
    ) -> Result<landlock::RulesetCreated, String> {
        ruleset
            .add_rules(landlock::path_beneath_rules(paths, access))
            .map_err(|error| format!("failed to add Landlock rules: {error}"))
    }

    fn apply_network_seccomp() -> Result<(), String> {
        let deny = |domain| -> Result<SeccompRule, String> {
            SeccompRule::new(vec![
                SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, domain)
                    .map_err(|error| error.to_string())?,
            ])
            .map_err(|error| error.to_string())
        };
        let mut rules = BTreeMap::new();
        rules.insert(
            libc::SYS_socket,
            vec![
                deny(libc::AF_INET as u64)?,
                deny(libc::AF_INET6 as u64)?,
                deny(libc::AF_PACKET as u64)?,
            ],
        );
        let arch = seccompiler::TargetArch::try_from(std::env::consts::ARCH)
            .map_err(|error| error.to_string())?;
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(libc::EPERM as u32),
            arch,
        )
        .map_err(|error| error.to_string())?;
        let program = seccompiler::BpfProgram::try_from(filter)
            .map_err(|error: seccompiler::BackendError| error.to_string())?;
        seccompiler::apply_filter(&program).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests;
