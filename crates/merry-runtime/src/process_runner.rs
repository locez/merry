//! Tokio-backed process runner adapter.
//!
//! This module is the concrete OS process adapter for Merry's runtime-owned
//! [`crate::ProcessRunner`] boundary. It does not decide whether a process is
//! admitted; callers must still opt in through runtime permission profiles.

use crate::{
    PathAccess, PathAccessRule, PermissionRequest, PermissionedProcessRunnerFactory,
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{Arc, RwLock},
};
use tokio::io::{AsyncRead, AsyncReadExt};

const BWRAP_PROGRAM: &str = "bwrap";
const ACTION_SANDBOX_HOME_FALLBACK: &str = "/home/merry";
const ACTION_SANDBOX_TMPDIR: &str = "/tmp";
const ACTION_SANDBOX_PATH_FALLBACK: &str = "/usr/local/bin:/usr/bin:/bin";
const ACTION_SANDBOX_ETC_READ_ONLY_FILE_PATHS: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
];
const ACTION_SANDBOX_ETC_READ_ONLY_DIR_PATHS: &[&str] = &[
    "/etc/ld.so.conf.d",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
];

/// Runtime-owned process runner backed by [`tokio::process::Command`].
///
/// The runner executes the exact validated argv supplied by
/// [`ProcessActionIntent`], inherits the current process environment, closes
/// stdin, captures stdout/stderr up to the intent limits, and cooperatively
/// cancels by killing the child process. Permission profiles and sandbox
/// constraints are enforced by the runtime construction path that selects this
/// runner, not by this type.
#[derive(Debug, Default, Clone)]
pub struct TokioProcessRunner {
    cwd_root: Option<PathBuf>,
}

impl TokioProcessRunner {
    /// Creates a Tokio-backed process runner.
    #[must_use]
    pub const fn new() -> Self {
        Self { cwd_root: None }
    }

    /// Creates a Tokio-backed process runner whose process cwd values are
    /// resolved under a stable workspace root.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: Some(root.into()),
        }
    }
}

/// Host-derived paths used to construct one action sandbox.
///
/// The HOME and PATH values preserve the caller's path namespace. The source
/// temporary directory is mounted at `/tmp` for every action, so an outer
/// session tmpfs remains shared while per-action bubblewrap namespaces stay
/// isolated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapProcessEnvironment {
    path: OsString,
    home: PathBuf,
    tmp_source: PathBuf,
    overrides: Vec<(OsString, OsString)>,
}

impl BwrapProcessEnvironment {
    /// Builds an environment layout from the current process environment.
    #[must_use]
    pub fn from_current_process() -> Self {
        let path = env::var_os("PATH")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from(ACTION_SANDBOX_PATH_FALLBACK));
        let home = absolute_env_path("HOME", ACTION_SANDBOX_HOME_FALLBACK);
        let tmp_source = absolute_env_path("TMPDIR", ACTION_SANDBOX_TMPDIR);
        Self {
            path,
            home,
            tmp_source,
            overrides: Vec::new(),
        }
    }

    /// Creates a validated environment layout for an action sandbox.
    pub fn new(
        path: impl Into<OsString>,
        home: impl Into<PathBuf>,
        tmp_source: impl Into<PathBuf>,
    ) -> Result<Self, ProcessRunnerError> {
        let path = path.into();
        let home = home.into();
        let tmp_source = tmp_source.into();
        if path.is_empty() {
            return Err(ProcessRunnerError::infrastructure(
                "sandbox process PATH must not be empty",
            ));
        }
        validate_os_string(&path, "sandbox process PATH")?;
        validate_clean_absolute_path(&home, "sandbox process HOME")?;
        validate_clean_absolute_path(&tmp_source, "sandbox process temporary directory")?;
        Ok(Self {
            path,
            home,
            tmp_source,
            overrides: Vec::new(),
        })
    }

    /// Validates and adds environment assignments after the sandbox defaults.
    pub fn with_overrides(
        mut self,
        overrides: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, ProcessRunnerError> {
        let mut names = std::collections::BTreeSet::new();
        let mut validated = Vec::new();
        for (name, value) in overrides {
            validate_environment_name(&name)?;
            validate_os_string(&value, "sandbox process environment value")?;
            if !names.insert(name.clone()) {
                return Err(ProcessRunnerError::infrastructure(
                    "sandbox process environment contains a duplicate variable",
                ));
            }
            validated.push((name, value));
        }
        self.overrides = validated;
        Ok(self)
    }

    /// Validates the host paths before constructing one action namespace.
    ///
    /// The temporary source remains the caller-selected host `TMPDIR` in
    /// `--no-sandbox` mode, but it must resolve to a real temporary directory.
    /// The returned environment uses its resolved path so a symlink cannot
    /// change the source after validation into a non-temporary tree.
    pub fn validate_for_workspace(
        &self,
        workspace_root: &Path,
    ) -> Result<Self, ProcessRunnerError> {
        validate_clean_absolute_path(workspace_root, "action workspace root")?;
        validate_clean_absolute_path(&self.home, "sandbox process HOME")?;
        if self.home == Path::new("/") || self.home == Path::new("/home") {
            return Err(ProcessRunnerError::infrastructure(
                "sandbox process HOME must identify a user directory",
            ));
        }
        validate_clean_absolute_path(&self.tmp_source, "sandbox process temporary directory")?;
        if self.tmp_source == Path::new("/") {
            return Err(ProcessRunnerError::infrastructure(
                "sandbox process temporary directory must not be the filesystem root",
            ));
        }
        let tmp_source = fs::canonicalize(&self.tmp_source).map_err(|source| {
            ProcessRunnerError::infrastructure(format!(
                "failed to resolve sandbox process temporary directory: {source}"
            ))
        })?;
        if !tmp_source.is_dir() {
            return Err(ProcessRunnerError::infrastructure(
                "sandbox process temporary directory must be a directory",
            ));
        }
        if !is_supported_temp_path(&tmp_source) {
            return Err(ProcessRunnerError::infrastructure(
                "sandbox process TMPDIR must resolve under /tmp, /var/tmp, /dev/shm, or a runtime temporary subdirectory",
            ));
        }

        let mut validated = self.clone();
        validated.tmp_source = tmp_source;
        Ok(validated)
    }
}

fn validate_os_string(value: &OsStr, label: &str) -> Result<(), ProcessRunnerError> {
    let Some(value) = value.to_str() else {
        return Err(ProcessRunnerError::infrastructure(format!(
            "{label} must be valid UTF-8"
        )));
    };
    if value.contains('\0') {
        return Err(ProcessRunnerError::infrastructure(format!(
            "{label} must not contain NUL"
        )));
    }
    Ok(())
}

fn validate_environment_name(name: &OsStr) -> Result<(), ProcessRunnerError> {
    validate_os_string(name, "sandbox process environment name")?;
    let name = name.to_str().ok_or_else(|| {
        ProcessRunnerError::infrastructure("sandbox process environment name must be valid UTF-8")
    })?;
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return Err(ProcessRunnerError::infrastructure(
            "sandbox process environment name must not be empty",
        ));
    };
    if !(first == '_' || first.is_ascii_alphabetic())
        || !characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return Err(ProcessRunnerError::infrastructure(
            "sandbox process environment names must use ASCII letters, digits, and underscores",
        ));
    }
    Ok(())
}

fn validate_clean_absolute_path(path: &Path, label: &str) -> Result<(), ProcessRunnerError> {
    let Some(value) = path.to_str() else {
        return Err(ProcessRunnerError::infrastructure(format!(
            "{label} must be valid UTF-8"
        )));
    };
    if value.contains('\0') {
        return Err(ProcessRunnerError::infrastructure(format!(
            "{label} must not contain NUL"
        )));
    }
    if !path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(ProcessRunnerError::infrastructure(format!(
            "{label} must be a clean absolute path"
        )));
    }
    Ok(())
}

fn is_supported_temp_path(path: &Path) -> bool {
    is_standard_temp_path(path)
        || env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|root| {
                root != path
                    && root.is_absolute()
                    && root.components().all(|component| {
                        matches!(component, Component::RootDir | Component::Normal(_))
                    })
            })
            .is_some_and(|root| path.starts_with(root))
}

fn is_standard_temp_path(path: &Path) -> bool {
    [
        Path::new("/tmp"),
        Path::new("/var/tmp"),
        Path::new("/dev/shm"),
    ]
    .into_iter()
    .any(|root| path == root || path.starts_with(root))
}

fn absolute_env_path(name: &str, fallback: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| PathBuf::from(fallback))
}

/// Runtime-owned process runner that executes each process inside bubblewrap.
///
/// This is Merry's per-action sandbox backend for Linux. It is intentionally
/// separate from the CLI outer sandbox: the outer sandbox protects the host
/// from the Merry process, while this runner protects each process action from
/// the runtime profile.
#[derive(Debug, Clone)]
pub struct BwrapProcessRunner {
    cwd_root: PathBuf,
    environment: BwrapProcessEnvironment,
    network_allowed: bool,
    path_rules: Vec<PathAccessRule>,
    session_permissions: Option<BwrapSessionPermissions>,
    bwrap_program: PathBuf,
    configuration_error: Option<String>,
}

impl BwrapProcessRunner {
    /// Creates a per-action bubblewrap runner rooted at a workspace path.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: root.into(),
            environment: BwrapProcessEnvironment::from_current_process(),
            network_allowed: false,
            path_rules: Vec::new(),
            session_permissions: None,
            bwrap_program: PathBuf::from(BWRAP_PROGRAM),
            configuration_error: None,
        }
    }

    /// Sets the environment layout visible to child processes.
    #[must_use]
    pub fn with_environment(mut self, environment: BwrapProcessEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Allows network access for child process actions.
    #[must_use]
    pub fn allow_network(mut self) -> Self {
        self.network_allowed = true;
        self
    }

    /// Installs trusted path rules for child process actions.
    #[must_use]
    pub fn with_path_rules(mut self, rules: impl IntoIterator<Item = PathAccessRule>) -> Self {
        self.path_rules = rules.into_iter().collect();
        self
    }

    /// Shares session-scoped approved capabilities with the permissioned runner factory.
    #[must_use]
    pub fn with_session_permissions(mut self, permissions: BwrapSessionPermissions) -> Self {
        self.session_permissions = Some(permissions);
        self
    }

    #[cfg(test)]
    fn with_bwrap_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.bwrap_program = program.into();
        self
    }

    fn plan_for(
        &self,
        intent: &ProcessActionIntent,
    ) -> Result<BwrapProcessPlan, ProcessRunnerError> {
        if let Some(message) = self.configuration_error.clone() {
            return Err(ProcessRunnerError::infrastructure(message));
        }
        let environment = self.environment.validate_for_workspace(&self.cwd_root)?;
        let session_snapshot = self
            .session_permissions
            .as_ref()
            .map(BwrapSessionPermissions::snapshot)
            .transpose()?;
        let mut path_rules = self.path_rules.clone();
        let mut network_allowed = self.network_allowed;
        if let Some(snapshot) = session_snapshot {
            path_rules.extend(snapshot.path_rules);
            network_allowed |= snapshot.network_allowed;
        }
        path_rules = normalize_path_rules(path_rules);
        Ok(bwrap_process_plan_with_environment(
            intent,
            &self.cwd_root,
            &environment,
            network_allowed,
            &path_rules,
            &self.bwrap_program,
        ))
    }
}

/// Capabilities approved for the lifetime of one runtime session.
///
/// Each process action still starts a fresh bubblewrap instance, but every
/// instance receives a snapshot of these approved capabilities. The store is
/// deliberately constructed and shared by one runtime backend; it is not
/// global and must not be reused across sessions.
#[derive(Debug, Clone, Default)]
pub struct BwrapSessionPermissions {
    state: Arc<RwLock<BwrapSessionPermissionState>>,
}

#[derive(Debug, Default)]
struct BwrapSessionPermissionState {
    network_allowed: bool,
    path_rules: Vec<PathAccessRule>,
}

#[derive(Debug, Clone)]
struct BwrapSessionPermissionSnapshot {
    network_allowed: bool,
    path_rules: Vec<PathAccessRule>,
}

impl BwrapSessionPermissions {
    /// Creates an empty session capability store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Result<BwrapSessionPermissionSnapshot, ProcessRunnerError> {
        let state = self.state.read().map_err(|_| {
            ProcessRunnerError::infrastructure(
                "session permission state lock was poisoned while building a process sandbox",
            )
        })?;
        Ok(BwrapSessionPermissionSnapshot {
            network_allowed: state.network_allowed,
            path_rules: state.path_rules.clone(),
        })
    }

    fn grant(
        &self,
        path_rules: Vec<PathAccessRule>,
        network_allowed: bool,
    ) -> Result<(), ProcessRunnerError> {
        let mut state = self.state.write().map_err(|_| {
            ProcessRunnerError::infrastructure(
                "session permission state lock was poisoned while recording an approved capability",
            )
        })?;
        let mut combined = state.path_rules.clone();
        combined.extend(path_rules);
        state.path_rules = normalize_session_path_rules(combined);
        state.network_allowed |= network_allowed;
        Ok(())
    }
}

/// Builds a bubblewrap runner from an approved permission request.
///
/// Filesystem access stays governed by the configured workspace/root path
/// rules and the session-scoped capabilities already approved by the runtime.
/// Approved `network=true` requests also remain active for later actions in
/// the same session; other sessions receive a fresh empty capability store.
#[derive(Debug, Clone)]
pub struct BwrapPermissionedProcessRunnerFactory {
    cwd_root: PathBuf,
    environment: BwrapProcessEnvironment,
    base_network_allowed: bool,
    path_rules: Vec<PathAccessRule>,
    session_permissions: Option<BwrapSessionPermissions>,
    bwrap_program: PathBuf,
}

impl BwrapPermissionedProcessRunnerFactory {
    /// Creates a bubblewrap permissioned runner factory rooted at a workspace path.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: root.into(),
            environment: BwrapProcessEnvironment::from_current_process(),
            base_network_allowed: false,
            path_rules: Vec::new(),
            session_permissions: None,
            bwrap_program: PathBuf::from(BWRAP_PROGRAM),
        }
    }

    /// Sets the environment layout used by every materialized child runner.
    #[must_use]
    pub fn with_environment(mut self, environment: BwrapProcessEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Allows network capability in the base profile before per-request grants.
    #[must_use]
    pub fn allow_base_network(mut self) -> Self {
        self.base_network_allowed = true;
        self
    }

    /// Installs trusted path rules shared by each materialized runner.
    #[must_use]
    pub fn with_path_rules(mut self, rules: impl IntoIterator<Item = PathAccessRule>) -> Self {
        self.path_rules = rules.into_iter().collect();
        self
    }

    /// Shares session-scoped approved capabilities with the ordinary process runner.
    #[must_use]
    pub fn with_session_permissions(mut self, permissions: BwrapSessionPermissions) -> Self {
        self.session_permissions = Some(permissions);
        self
    }

    #[cfg(test)]
    fn with_bwrap_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.bwrap_program = program.into();
        self
    }

    fn path_rules_for_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<Vec<PathAccessRule>, ProcessRunnerError> {
        let mut rules = self.path_rules.clone();
        if let Some(permissions) = &self.session_permissions {
            rules.extend(permissions.snapshot()?.path_rules);
        }
        rules.extend(self.requested_path_rules_for_request(request)?);
        Ok(normalize_path_rules(rules))
    }

    fn requested_path_rules_for_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<Vec<PathAccessRule>, ProcessRunnerError> {
        request
            .requested()
            .iter()
            .filter_map(|capability| {
                let crate::RequestedCapability::Path(requested) = capability else {
                    return None;
                };
                let path = materialize_requested_path(&self.cwd_root, requested.path());
                Some(
                    effective_requested_path_access(&path, requested.access(), &self.path_rules)
                        .map(|effective_access| {
                            PathAccessRule::new(
                                path,
                                effective_access,
                                crate::PathAccessRuleSource::PermissionReview,
                            )
                        }),
                )
            })
            .collect()
    }

    fn grant_approved_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<(), ProcessRunnerError> {
        let Some(permissions) = &self.session_permissions else {
            return Ok(());
        };
        permissions.grant(
            self.requested_path_rules_for_request(request)?,
            request.requests_network(),
        )
    }

    fn build_runner(&self, request: &PermissionRequest) -> BwrapProcessRunner {
        let (path_rules, configuration_error) = match self.path_rules_for_request(request) {
            Ok(path_rules) => (path_rules, None),
            Err(error) => (self.path_rules.clone(), Some(error.to_string())),
        };
        let mut runner = BwrapProcessRunner::new_at_workspace_root(self.cwd_root.clone())
            .with_environment(self.environment.clone())
            .with_path_rules(path_rules);
        let session_network_allowed = self
            .session_permissions
            .as_ref()
            .and_then(|permissions| permissions.snapshot().ok())
            .is_some_and(|snapshot| snapshot.network_allowed);
        if self.base_network_allowed || request.requests_network() || session_network_allowed {
            runner = runner.allow_network();
        }
        if let Some(permissions) = &self.session_permissions {
            runner = runner.with_session_permissions(permissions.clone());
        }
        runner.bwrap_program = self.bwrap_program.clone();
        runner.configuration_error = configuration_error;
        runner
    }
}

impl PermissionedProcessRunnerFactory for BwrapPermissionedProcessRunnerFactory {
    fn validate_request(&self, request: &PermissionRequest) -> Result<(), ProcessRunnerError> {
        self.environment
            .validate_for_workspace(&self.cwd_root)
            .map(|_| ())?;
        self.path_rules_for_request(request).map(|_| ())
    }

    fn runner_for(&self, request: &PermissionRequest) -> Arc<dyn ProcessRunner> {
        let grant_error = self.grant_approved_request(request).err();
        let mut runner = self.build_runner(request);
        if let Some(error) = grant_error {
            runner.configuration_error = Some(error.to_string());
        }
        Arc::new(runner)
    }
}

fn materialize_requested_path(root: &Path, requested: &str) -> PathBuf {
    let path = Path::new(requested);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn effective_requested_path_access(
    requested_path: &Path,
    requested_access: PathAccess,
    configured_rules: &[PathAccessRule],
) -> Result<PathAccess, ProcessRunnerError> {
    let mut nearest = None;
    for rule in configured_rules {
        if requested_path.starts_with(rule.path())
            && nearest.is_none_or(|current: &PathAccessRule| {
                path_depth(rule.path()) > path_depth(current.path())
            })
        {
            nearest = Some(rule);
        }
    }

    if let Some(rule) = nearest {
        if rule.access() == PathAccess::Deny {
            return Err(ProcessRunnerError::infrastructure(format!(
                "requested path `{}` is denied by configured path policy `{}`",
                requested_path.display(),
                rule.path().display()
            )));
        }
        if rule.access() == PathAccess::ReadOnly {
            return Ok(PathAccess::ReadOnly);
        }
    }
    Ok(requested_access)
}

fn normalize_path_rules(rules: Vec<PathAccessRule>) -> Vec<PathAccessRule> {
    let mut merged =
        std::collections::BTreeMap::<PathBuf, (PathAccess, crate::PathAccessRuleSource)>::new();
    for rule in rules {
        let entry = merged
            .entry(rule.path().to_path_buf())
            .or_insert((rule.access(), rule.source()));
        entry.0 = restrictive_path_access(entry.0, rule.access());
        if rule.access() == entry.0 {
            entry.1 = rule.source();
        }
    }
    let mut rules = merged
        .into_iter()
        .map(|(path, (access, source))| PathAccessRule::new(path, access, source))
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| {
        path_depth(left.path())
            .cmp(&path_depth(right.path()))
            .then_with(|| left.path().cmp(right.path()))
    });
    rules
}

fn normalize_session_path_rules(rules: Vec<PathAccessRule>) -> Vec<PathAccessRule> {
    let mut merged =
        std::collections::BTreeMap::<PathBuf, (PathAccess, crate::PathAccessRuleSource)>::new();
    for rule in rules {
        let entry = merged
            .entry(rule.path().to_path_buf())
            .or_insert((rule.access(), rule.source()));
        entry.0 = session_path_access(entry.0, rule.access());
        if rule.access() == entry.0 {
            entry.1 = rule.source();
        }
    }
    let mut rules = merged
        .into_iter()
        .map(|(path, (access, source))| PathAccessRule::new(path, access, source))
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| {
        path_depth(left.path())
            .cmp(&path_depth(right.path()))
            .then_with(|| left.path().cmp(right.path()))
    });

    let mut retained = Vec::with_capacity(rules.len());
    for rule in rules {
        let covered_by_ancestor = retained.iter().any(|ancestor: &PathAccessRule| {
            rule.path() != ancestor.path()
                && rule.path().starts_with(ancestor.path())
                && match ancestor.access() {
                    PathAccess::Deny => true,
                    PathAccess::ReadOnly => rule.access() == PathAccess::ReadOnly,
                    PathAccess::ReadWrite => rule.access() != PathAccess::Deny,
                }
        });
        if !covered_by_ancestor {
            retained.push(rule);
        }
    }
    retained
}

fn session_path_access(left: PathAccess, right: PathAccess) -> PathAccess {
    match (left, right) {
        (PathAccess::Deny, _) | (_, PathAccess::Deny) => PathAccess::Deny,
        (PathAccess::ReadWrite, _) | (_, PathAccess::ReadWrite) => PathAccess::ReadWrite,
        (PathAccess::ReadOnly, PathAccess::ReadOnly) => PathAccess::ReadOnly,
    }
}

fn restrictive_path_access(left: PathAccess, right: PathAccess) -> PathAccess {
    match (left, right) {
        (PathAccess::Deny, _) | (_, PathAccess::Deny) => PathAccess::Deny,
        (PathAccess::ReadOnly, _) | (_, PathAccess::ReadOnly) => PathAccess::ReadOnly,
        (PathAccess::ReadWrite, PathAccess::ReadWrite) => PathAccess::ReadWrite,
    }
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

impl ProcessRunner for BwrapProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        let plan = match self.plan_for(&intent) {
            Ok(plan) => plan,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        Box::pin(async move { run_process_plan(plan, intent, context).await })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BwrapProcessPlan {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
}

#[cfg(test)]
fn bwrap_process_plan(
    intent: &ProcessActionIntent,
    cwd_root: &Path,
    network_allowed: bool,
    path_rules: &[PathAccessRule],
    bwrap_program: &Path,
) -> BwrapProcessPlan {
    let environment = BwrapProcessEnvironment {
        path: OsString::from(ACTION_SANDBOX_PATH_FALLBACK),
        home: PathBuf::from(ACTION_SANDBOX_HOME_FALLBACK),
        tmp_source: PathBuf::from(ACTION_SANDBOX_TMPDIR),
        overrides: Vec::new(),
    };
    bwrap_process_plan_with_environment(
        intent,
        cwd_root,
        &environment,
        network_allowed,
        path_rules,
        bwrap_program,
    )
}

fn bwrap_process_plan_with_environment(
    intent: &ProcessActionIntent,
    cwd_root: &Path,
    environment: &BwrapProcessEnvironment,
    network_allowed: bool,
    path_rules: &[PathAccessRule],
    bwrap_program: &Path,
) -> BwrapProcessPlan {
    let cwd = process_current_dir(Some(cwd_root), intent);
    let mut args = vec![
        os("--unshare-user"),
        os("--unshare-ipc"),
        os("--unshare-pid"),
        os("--unshare-uts"),
        os("--unshare-cgroup-try"),
        os("--die-with-parent"),
        os("--new-session"),
        os("--proc"),
        os("/proc"),
        os("--dev"),
        os("/dev"),
        os("--bind"),
        environment.tmp_source.as_os_str().to_owned(),
        os(ACTION_SANDBOX_TMPDIR),
        os("--tmpfs"),
        os("/home"),
    ];
    if !environment.home.starts_with(Path::new("/home")) {
        append_bwrap_mount_parent_args(&mut args, &environment.home);
        args.extend([os("--tmpfs"), environment.home.as_os_str().to_owned()]);
    }
    args.extend([
        os("--perms"),
        os("0700"),
        os("--dir"),
        environment.home.as_os_str().to_owned(),
        os("--ro-bind"),
        os("/usr"),
        os("/usr"),
        os("--ro-bind-try"),
        os("/bin"),
        os("/bin"),
        os("--ro-bind-try"),
        os("/lib"),
        os("/lib"),
        os("--ro-bind-try"),
        os("/lib64"),
        os("/lib64"),
        os("--ro-bind-try"),
        os("/opt"),
        os("/opt"),
    ]);
    for path in ACTION_SANDBOX_ETC_READ_ONLY_FILE_PATHS {
        if Path::new(path).exists() {
            append_bwrap_file_bind_args(&mut args, Path::new(path), Path::new(path));
        }
    }
    for path in ACTION_SANDBOX_ETC_READ_ONLY_DIR_PATHS {
        if Path::new(path).exists() {
            append_bwrap_dir_bind_args(&mut args, Path::new(path), Path::new(path));
        }
    }
    if !network_allowed {
        args.push(os("--unshare-net"));
    }
    append_bwrap_required_path_rule(&mut args, cwd_root, PathAccess::ReadWrite);
    for rule in path_rules {
        append_bwrap_path_rule(&mut args, rule.path(), rule.access());
    }
    args.extend([
        os("--chdir"),
        cwd.as_os_str().to_owned(),
        os("--clearenv"),
        os("--setenv"),
        os("PATH"),
        environment.path.clone(),
        os("--setenv"),
        os("HOME"),
        environment.home.as_os_str().to_owned(),
        os("--setenv"),
        os("TMPDIR"),
        os(ACTION_SANDBOX_TMPDIR),
        os("--setenv"),
        os("PWD"),
        cwd.as_os_str().to_owned(),
    ]);
    for (name, value) in &environment.overrides {
        args.extend([os("--setenv"), name.clone(), value.clone()]);
    }
    args.push(os("--"));
    args.extend(intent.argv().iter().map(OsString::from));

    BwrapProcessPlan {
        program: bwrap_program.as_os_str().to_owned(),
        args,
        cwd,
    }
}

fn append_bwrap_file_bind_args(args: &mut Vec<OsString>, source: &Path, destination: &Path) {
    append_bwrap_mount_parent_args(args, destination);
    args.extend([
        os("--ro-bind"),
        source.as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ]);
}

fn append_bwrap_dir_bind_args(args: &mut Vec<OsString>, source: &Path, destination: &Path) {
    append_bwrap_mount_parent_args(args, destination);
    args.extend([
        os("--ro-bind"),
        source.as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ]);
}

fn append_bwrap_path_rule(args: &mut Vec<OsString>, path: &Path, access: PathAccess) {
    append_bwrap_mount_parent_args(args, path);
    match access {
        PathAccess::ReadOnly => args.extend([
            os("--ro-bind-try"),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::ReadWrite => args.extend([
            os("--bind-try"),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::Deny => {
            args.extend([os("--tmpfs"), path.as_os_str().to_owned()]);
        }
    }
}

fn append_bwrap_required_path_rule(args: &mut Vec<OsString>, path: &Path, access: PathAccess) {
    append_bwrap_mount_parent_args(args, path);
    match access {
        PathAccess::ReadOnly => args.extend([
            os("--ro-bind"),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::ReadWrite => args.extend([
            os("--bind"),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::Deny => args.extend([os("--tmpfs"), path.as_os_str().to_owned()]),
    }
}

fn append_bwrap_mount_parent_args(args: &mut Vec<OsString>, destination: &Path) {
    let Some(parent) = destination.parent() else {
        return;
    };
    let mut parents = parent
        .ancestors()
        .take_while(|path| *path != Path::new("/"))
        .collect::<Vec<_>>();
    parents.reverse();

    for parent in parents {
        args.extend([os("--dir"), parent.as_os_str().to_owned()]);
    }
}

async fn run_process_plan(
    plan: BwrapProcessPlan,
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    if context.cancellation_token().is_cancelled() {
        return Err(ProcessRunnerError::Cancelled);
    }

    let mut command = tokio::process::Command::new(&plan.program);
    command
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let backend_program = plan.program.to_string_lossy().into_owned();
    run_spawned_process(command, intent, context, backend_program).await
}

impl ProcessRunner for TokioProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        let cwd_root = self.cwd_root.clone();
        Box::pin(async move { run_tokio_process(intent, context, cwd_root.as_deref()).await })
    }
}

async fn run_tokio_process(
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    cwd_root: Option<&Path>,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    let Some((program, args)) = intent.argv().split_first() else {
        return Err(ProcessRunnerError::infrastructure(
            "validated process argv was unexpectedly empty",
        ));
    };
    let program = program.clone();
    let program_for_error = program.clone();
    let args = args.to_vec();

    if context.cancellation_token().is_cancelled() {
        return Err(ProcessRunnerError::Cancelled);
    }

    let mut command = tokio::process::Command::new(&program);
    command
        .args(&args)
        .current_dir(process_current_dir(cwd_root, &intent))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    run_spawned_process(command, intent, context, program_for_error).await
}

async fn run_spawned_process(
    mut command: tokio::process::Command,
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    program_for_error: String,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    let mut child = command.spawn().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProcessRunnerError::infrastructure(format!(
                "process executable `{program_for_error}` was not found"
            ))
        } else {
            ProcessRunnerError::infrastructure(format!(
                "failed to start process executable `{program_for_error}`: {source}"
            ))
        }
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ProcessRunnerError::infrastructure("process stdout pipe was not available")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ProcessRunnerError::infrastructure("process stderr pipe was not available")
    })?;
    let stdout_limit = intent.stdout_limit_bytes();
    let stderr_limit = intent.stderr_limit_bytes();

    let stdout_task = tokio::spawn(async move { read_bounded_output(stdout, stdout_limit).await });
    let stderr_task = tokio::spawn(async move { read_bounded_output(stderr, stderr_limit).await });

    let status = tokio::select! {
        biased;
        () = context.cancellation_token().cancelled() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(ProcessRunnerError::Cancelled);
        }
        status = child.wait() => status,
    };
    let status = status.map_err(|source| {
        ProcessRunnerError::infrastructure(format!(
            "failed to wait for process executable `{program_for_error}`: {source}"
        ))
    })?;
    let stdout = join_bounded_output(stdout_task, "stdout").await?;
    let stderr = join_bounded_output(stderr_task, "stderr").await?;
    let stdout_text = String::from_utf8(stdout.bytes).map_err(|source| {
        ProcessRunnerError::infrastructure(format!("process stdout was not UTF-8: {source}"))
    })?;
    let stderr_text = String::from_utf8(stderr.bytes).map_err(|source| {
        ProcessRunnerError::infrastructure(format!("process stderr was not UTF-8: {source}"))
    })?;
    let status = status
        .code()
        .map(ProcessExitStatus::Exited)
        .unwrap_or(ProcessExitStatus::DomainFailed);

    ProcessRunnerOutput::new(
        &intent,
        status,
        stdout_text,
        stdout.truncated,
        stderr_text,
        stderr.truncated,
    )
    .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
}

fn process_current_dir(cwd_root: Option<&Path>, intent: &ProcessActionIntent) -> PathBuf {
    let cwd = intent.cwd().unwrap_or(".");
    let Some(root) = cwd_root else {
        return PathBuf::from(cwd);
    };
    if cwd == "." {
        root.to_path_buf()
    } else {
        root.join(cwd)
    }
}

fn os(value: &str) -> OsString {
    OsString::from(value)
}

async fn join_bounded_output(
    task: tokio::task::JoinHandle<Result<BoundedOutput, ProcessRunnerError>>,
    stream_name: &'static str,
) -> Result<BoundedOutput, ProcessRunnerError> {
    task.await.map_err(|source| {
        ProcessRunnerError::infrastructure(format!(
            "process {stream_name} reader task failed: {source}"
        ))
    })?
}

struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded_output<R>(
    mut reader: R,
    limit: usize,
) -> Result<BoundedOutput, ProcessRunnerError>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut scratch = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let count = reader.read(&mut scratch).await.map_err(|source| {
            ProcessRunnerError::infrastructure(format!("failed to read process output: {source}"))
        })?;
        if count == 0 {
            break;
        }

        if bytes.len() < limit {
            let remaining = limit - bytes.len();
            let kept = count.min(remaining);
            bytes.extend_from_slice(&scratch[..kept]);
            truncated |= kept < count;
        } else {
            truncated = true;
        }
    }

    Ok(BoundedOutput { bytes, truncated })
}

#[cfg(test)]
mod tests {
    use super::{
        BwrapProcessEnvironment, BwrapProcessRunner, TokioProcessRunner, bwrap_process_plan,
        bwrap_process_plan_with_environment, process_current_dir,
    };
    use crate::{
        PathAccess, PathAccessRule, PathAccessRuleSource, PermissionRequest, PermissionedAction,
        PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus,
        ProcessRunner, ProcessRunnerContext, StaticPermissionedProcessRunnerFactory,
    };
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use serde_json::json;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn intent(cwd: Option<&str>) -> ProcessActionIntent {
        ProcessActionIntent::new(
            vec!["pwd".to_owned()],
            cwd.map(str::to_owned),
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("process intent should be valid")
    }

    fn permission_request(arguments: serde_json::Value) -> PermissionRequest {
        let call = PendingToolCall::new(
            ToolCallId::new("call-permission").expect("valid call id"),
            ToolName::new("request_permissions").expect("valid tool name"),
            ToolCallArguments::try_from(arguments).expect("valid tool arguments"),
        );
        crate::permission::permission_request_from_call(&call, Vec::new())
            .expect("permission request should parse")
    }

    fn request_process_intent(request: &PermissionRequest) -> &ProcessActionIntent {
        let PermissionedAction::Process(intent) = request.action();
        intent
    }

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
        if Path::new("/etc/ld.so.cache").exists() {
            assert!(contains_sequence(
                &args,
                &["--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache"]
            ));
        }
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
    }

    #[test]
    fn bwrap_process_environment_validates_temporary_boundary_and_overrides() {
        let environment =
            BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/home/alice", "/tmp")
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
    fn bwrap_process_plan_places_custom_home_permissions_on_home_directory() {
        let environment =
            BwrapProcessEnvironment::new("/custom/bin:/usr/bin", "/srv/alice", "/tmp")
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
        let runner =
            BwrapProcessRunner::new_at_workspace_root("/workspace/merry").with_path_rules([
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

    #[test]
    fn bwrap_permissioned_factory_allows_network_only_when_requested() {
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_bwrap_program("/custom/bin/bwrap");
        let request_without_network = permission_request(json!({
            "requested": {
                "paths": [{ "path": "/workspace/merry", "access": "rw" }]
            },
            "for_action": { "kind": "process", "argv": ["cargo", "test"], "cwd": "." }
        }));
        let request_with_network = permission_request(json!({
            "requested": { "network": true },
            "for_action": { "kind": "process", "argv": ["cargo", "test"], "cwd": "." }
        }));

        let runner_without_network = factory.build_runner(&request_without_network);
        let plan_without_network = bwrap_process_plan(
            request_process_intent(&request_without_network),
            &runner_without_network.cwd_root,
            runner_without_network.network_allowed,
            &runner_without_network.path_rules,
            &runner_without_network.bwrap_program,
        );
        let runner_with_network = factory.build_runner(&request_with_network);
        let plan_with_network = bwrap_process_plan(
            request_process_intent(&request_with_network),
            &runner_with_network.cwd_root,
            runner_with_network.network_allowed,
            &runner_with_network.path_rules,
            &runner_with_network.bwrap_program,
        );

        assert!(os_args(&plan_without_network.args).contains(&"--unshare-net".to_owned()));
        assert!(!os_args(&plan_with_network.args).contains(&"--unshare-net".to_owned()));
    }

    #[test]
    fn bwrap_permissioned_factory_preserves_trusted_path_rules() {
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_path_rules([PathAccessRule::new(
                    PathBuf::from("/var/log"),
                    PathAccess::ReadOnly,
                    PathAccessRuleSource::TrustedGlobalConfig,
                )]);
        let request = permission_request(json!({
            "requested": { "network": true },
            "for_action": { "kind": "process", "argv": ["cargo", "test"], "cwd": "." }
        }));

        let runner = factory.build_runner(&request);
        let plan = bwrap_process_plan(
            request_process_intent(&request),
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
    }

    #[test]
    fn bwrap_permissioned_factory_materializes_requested_path_rules() {
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_bwrap_program("/custom/bin/bwrap");
        let request = permission_request(json!({
            "requested": {
                "paths": [{ "path": "deps/cache", "access": "rw" }]
            },
            "for_action": { "kind": "process", "argv": ["cargo", "test"], "cwd": "." }
        }));

        let runner = factory.build_runner(&request);
        let plan = bwrap_process_plan(
            request_process_intent(&request),
            &runner.cwd_root,
            runner.network_allowed,
            &runner.path_rules,
            &runner.bwrap_program,
        );
        let args = os_args(&plan.args);

        assert!(contains_sequence(
            &args,
            &[
                "--bind-try",
                "/workspace/merry/deps/cache",
                "/workspace/merry/deps/cache"
            ]
        ));
    }

    #[test]
    fn bwrap_permissioned_factory_keeps_approved_paths_for_later_actions() {
        let session_permissions = super::BwrapSessionPermissions::new();
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_bwrap_program("/custom/bin/bwrap")
                .with_session_permissions(session_permissions.clone());
        let base_runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
            .with_bwrap_program("/custom/bin/bwrap")
            .with_session_permissions(session_permissions);
        let first_request = permission_request(json!({
            "requested": {
                "paths": [
                    { "path": "/tmp", "access": "rw" },
                    { "path": "/var/lib/merry-demo.txt", "access": "ro" }
                ]
            },
            "for_action": { "kind": "process", "argv": ["touch", "/var/lib/merry-demo.txt"] }
        }));
        let later_request = permission_request(json!({
            "requested": { "network": true },
            "for_action": { "kind": "process", "argv": ["cat", "/var/lib/merry-demo.txt"] }
        }));
        let narrower_request = permission_request(json!({
            "requested": {
                "paths": [{ "path": "/tmp", "access": "ro" }]
            },
            "for_action": { "kind": "process", "argv": ["ls", "/tmp"] }
        }));

        let _ = factory.runner_for(&first_request);
        let _ = factory.runner_for(&narrower_request);
        let plan = base_runner
            .plan_for(request_process_intent(&later_request))
            .expect("later ordinary process plan should build");
        let args = os_args(&plan.args);

        assert!(contains_sequence(&args, &["--bind-try", "/tmp", "/tmp"]));
        assert!(!contains_sequence(
            &args,
            &["--ro-bind-try", "/tmp", "/tmp"]
        ));
        assert!(contains_sequence(
            &args,
            &[
                "--ro-bind-try",
                "/var/lib/merry-demo.txt",
                "/var/lib/merry-demo.txt"
            ]
        ));
        assert!(!contains_sequence(
            &args,
            &["--bind-try", "/var/tmp", "/var/tmp"]
        ));
    }

    #[test]
    fn bwrap_permissioned_factory_caps_requested_write_to_trusted_read_only_rule() {
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_path_rules([PathAccessRule::new(
                    PathBuf::from("/workspace/merry/deps"),
                    PathAccess::ReadOnly,
                    PathAccessRuleSource::TrustedGlobalConfig,
                )]);
        let request = permission_request(json!({
            "requested": {
                "paths": [{ "path": "deps", "access": "rw" }]
            },
            "for_action": { "kind": "process", "argv": ["cargo", "test"], "cwd": "." }
        }));

        factory
            .validate_request(&request)
            .expect("read-only policy should cap rather than reject a write request");
        let runner = factory.build_runner(&request);
        let plan = bwrap_process_plan(
            request_process_intent(&request),
            &runner.cwd_root,
            runner.network_allowed,
            &runner.path_rules,
            &runner.bwrap_program,
        );
        let args = os_args(&plan.args);

        assert!(contains_sequence(
            &args,
            &[
                "--ro-bind-try",
                "/workspace/merry/deps",
                "/workspace/merry/deps"
            ]
        ));
        assert!(!contains_sequence(
            &args,
            &[
                "--bind-try",
                "/workspace/merry/deps",
                "/workspace/merry/deps"
            ]
        ));
    }

    #[test]
    fn bwrap_permissioned_factory_rejects_requested_path_under_configured_deny() {
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_path_rules([PathAccessRule::new(
                    PathBuf::from("/workspace/merry/secrets"),
                    PathAccess::Deny,
                    PathAccessRuleSource::TrustedGlobalConfig,
                )]);
        let request = permission_request(json!({
            "requested": {
                "paths": [{ "path": "secrets/token", "access": "ro" }]
            },
            "for_action": { "kind": "process", "argv": ["cat", "secrets/token"] }
        }));

        let error = factory
            .validate_request(&request)
            .expect_err("configured deny must be a hard policy boundary");
        assert!(
            error
                .to_string()
                .contains("denied by configured path policy")
        );
    }

    #[test]
    fn static_permissioned_factory_rejects_requested_path_capabilities() {
        let factory = StaticPermissionedProcessRunnerFactory::new(Arc::new(
            BwrapProcessRunner::new_at_workspace_root("/workspace/merry"),
        ));
        let request = permission_request(json!({
            "requested": {
                "paths": [{ "path": "deps/cache", "access": "rw" }]
            },
            "for_action": { "kind": "process", "argv": ["cargo", "test"] }
        }));

        let error = factory
            .validate_request(&request)
            .expect_err("static runner must not silently ignore path capabilities");
        assert!(
            error
                .to_string()
                .contains("cannot enforce requested path capabilities")
        );
    }

    #[tokio::test]
    async fn bwrap_permissioned_factory_runner_for_invalid_request_fails_closed() {
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_path_rules([PathAccessRule::new(
                    PathBuf::from("/workspace/merry/secrets"),
                    PathAccess::Deny,
                    PathAccessRuleSource::TrustedGlobalConfig,
                )]);
        let request = permission_request(json!({
            "requested": {
                "paths": [{ "path": "secrets/token", "access": "ro" }]
            },
            "for_action": { "kind": "process", "argv": ["cat", "secrets/token"] }
        }));

        let runner = factory.runner_for(&request);
        let error = runner
            .run(
                request_process_intent(&request).clone(),
                ProcessRunnerContext::new(CancellationToken::new()),
            )
            .await
            .expect_err("invalid path request must not reach the process backend");
        assert!(
            error
                .to_string()
                .contains("denied by configured path policy")
        );
    }

    #[tokio::test]
    async fn tokio_process_runner_inherits_current_process_environment() {
        let Ok(path) = std::env::var("PATH") else {
            return;
        };
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let intent = ProcessActionIntent::new(
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf '%s\n%s' \"${PATH-}\" \"${HOME-}\"".to_owned(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            64 * 1024,
            1024,
        )
        .expect("process intent should be valid");

        let output = TokioProcessRunner::new()
            .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
            .await
            .expect("process should run");

        assert_eq!(output.status(), ProcessExitStatus::Exited(0));
        assert_eq!(output.stdout_text(), format!("{path}\n{home}"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bwrap_process_runner_reuses_tmp_and_writes_workspace() {
        let workspace = tempfile::tempdir().expect("workspace tempdir should be created");
        let marker = format!("merry-process-runner-{}", std::process::id());
        // This container cannot create a network namespace; filesystem and
        // temporary-directory behavior are independent of that capability.
        let runner = BwrapProcessRunner::new_at_workspace_root(workspace.path())
            .allow_network()
            .with_bwrap_program("/usr/bin/bwrap");

        let write_tmp = ProcessActionIntent::new(
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                format!("printf 'tmp-ok' > /tmp/{marker}"),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("temporary write intent should be valid");
        let first = runner
            .run(
                write_tmp,
                ProcessRunnerContext::new(CancellationToken::new()),
            )
            .await
            .expect("first bwrap action should run");
        assert!(first.ok(), "first bwrap action failed: {first:?}");

        let read_tmp = ProcessActionIntent::new(
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                format!("cat /tmp/{marker}"),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("temporary read intent should be valid");
        let second = runner
            .run(
                read_tmp,
                ProcessRunnerContext::new(CancellationToken::new()),
            )
            .await
            .expect("second bwrap action should run");
        assert!(second.ok(), "second bwrap action failed: {second:?}");
        assert_eq!(second.stdout_text(), "tmp-ok");

        let write_workspace = ProcessActionIntent::new(
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf 'workspace-ok' > workspace-write.txt".to_owned(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("workspace write intent should be valid");
        let third = runner
            .run(
                write_workspace,
                ProcessRunnerContext::new(CancellationToken::new()),
            )
            .await
            .expect("workspace write action should run");
        assert!(third.ok(), "workspace write action failed: {third:?}");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("workspace-write.txt"))
                .expect("workspace write should persist"),
            "workspace-ok"
        );

        let _ = std::fs::remove_file(Path::new("/tmp").join(marker));
    }

    fn os_args(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn contains_sequence(args: &[String], expected: &[&str]) -> bool {
        args.windows(expected.len()).any(|window| {
            window
                .iter()
                .map(String::as_str)
                .eq(expected.iter().copied())
        })
    }
}
