use merry_runtime::{HostIntegration, ProcessActionIntent, ProcessRunnerError};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Component, Path, PathBuf},
};

pub(super) const BWRAP_PROGRAM: &str = "bwrap";
pub(super) const ACTION_SANDBOX_HOME_FALLBACK: &str = "/home/merry";
pub(super) const ACTION_SANDBOX_TMPDIR: &str = "/tmp";
pub(super) const ACTION_SANDBOX_PATH_FALLBACK: &str = "/usr/local/bin:/usr/bin:/bin";
pub(super) const ACTION_SANDBOX_ETC_READ_ONLY_FILE_PATHS: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
];
pub(super) const ACTION_SANDBOX_ETC_READ_ONLY_DIR_PATHS: &[&str] = &[
    "/etc/ld.so.conf.d",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
];

/// Host-derived paths used to construct one action sandbox.
///
/// The HOME, PATH, and non-policy environment variables preserve the caller's
/// process environment. The action namespace starts from a read-only view of
/// its parent filesystem; workspace, approved path, and host-integration mounts
/// then overlay the specific writable or temporarily exposed locations. The
/// source temporary directory is mounted at `/tmp` for every action, so an
/// outer session tmpfs remains shared while per-action bubblewrap namespaces
/// stay isolated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapProcessEnvironment {
    pub(super) path: OsString,
    pub(super) home: PathBuf,
    pub(super) tmp_source: PathBuf,
    pub(super) overrides: Vec<(OsString, OsString)>,
    pub(super) host_integrations: Vec<HostIntegration>,
    pub(super) ssh_agent_socket: Option<PathBuf>,
    pub(super) session_bus_address: Option<OsString>,
}

impl BwrapProcessEnvironment {
    /// Builds an environment layout from the current process environment.
    ///
    /// The resulting action plan intentionally does not use `--clearenv`; the
    /// caller's environment reaches the child and Merry overrides only the
    /// validated process defaults and configured assignments.
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
            host_integrations: Vec::new(),
            ssh_agent_socket: env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            session_bus_address: env::var_os("DBUS_SESSION_BUS_ADDRESS"),
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
            host_integrations: Vec::new(),
            ssh_agent_socket: env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            session_bus_address: env::var_os("DBUS_SESSION_BUS_ADDRESS"),
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

    /// Enables selected host IPC integrations for child process actions.
    #[must_use]
    pub fn with_host_integrations(
        mut self,
        integrations: impl IntoIterator<Item = HostIntegration>,
    ) -> Self {
        self.host_integrations.extend(integrations);
        self.host_integrations.sort_unstable();
        self.host_integrations.dedup();
        self
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

    pub(super) fn validate_requested_host_integrations(
        &self,
        integrations: &[HostIntegration],
    ) -> Result<(), ProcessRunnerError> {
        for integration in integrations {
            let available = match integration {
                HostIntegration::SshAgent => self
                    .ssh_agent_socket
                    .as_ref()
                    .is_some_and(|path| is_clean_absolute_path(path)),
                HostIntegration::SessionBus => self
                    .session_bus_address
                    .as_ref()
                    .and_then(|address| session_bus_socket_path(address))
                    .is_some_and(|path| is_clean_absolute_path(&path)),
            };
            if !available {
                return Err(ProcessRunnerError::infrastructure(format!(
                    "host integration `{}` is not available in the current sandbox environment",
                    integration.as_str()
                )));
            }
        }
        Ok(())
    }

    pub(super) fn host_integration_candidates(
        &self,
    ) -> Vec<(HostIntegration, PathBuf, Option<OsString>)> {
        let mut candidates = Vec::new();
        if let Some(path) = self
            .ssh_agent_socket
            .as_ref()
            .filter(|path| is_clean_absolute_path(path))
        {
            candidates.push((HostIntegration::SshAgent, path.clone(), None));
        }
        if let Some((socket, address)) = self
            .session_bus_address
            .as_ref()
            .and_then(|address| session_bus_socket_path(address).map(|socket| (socket, address)))
            .filter(|(socket, _)| is_clean_absolute_path(socket))
        {
            candidates.push((HostIntegration::SessionBus, socket, Some(address.clone())));
        }
        candidates
    }

    pub(super) fn host_integration_hidden_paths(&self) -> Vec<PathBuf> {
        let mut hidden = Vec::new();
        for (_, socket, _) in self.host_integration_candidates() {
            let Some(parent) = socket.parent() else {
                continue;
            };
            if parent.starts_with(&self.tmp_source) || self.tmp_source.starts_with(parent) {
                continue;
            }
            if hidden.iter().any(|path: &PathBuf| parent.starts_with(path)) {
                continue;
            }
            hidden.retain(|path| !path.starts_with(parent));
            hidden.push(parent.to_path_buf());
        }
        hidden
    }

    pub(super) fn host_integration_bindings(
        &self,
    ) -> Vec<(HostIntegration, PathBuf, Option<OsString>)> {
        self.host_integration_candidates()
            .into_iter()
            .filter(|(integration, _, _)| self.host_integrations.contains(integration))
            .collect()
    }
}

pub(super) fn validate_os_string(value: &OsStr, label: &str) -> Result<(), ProcessRunnerError> {
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

pub(super) fn validate_environment_name(name: &OsStr) -> Result<(), ProcessRunnerError> {
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

fn is_clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn session_bus_socket_path(address: &OsStr) -> Option<PathBuf> {
    let address = address.to_str()?;
    address.split(';').find_map(|candidate| {
        let options = candidate.strip_prefix("unix:")?;
        options.split(',').find_map(|option| {
            let (name, value) = option.split_once('=')?;
            (name == "path").then(|| PathBuf::from(value))
        })
    })
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

pub(crate) fn process_current_dir(
    cwd_root: Option<&Path>,
    intent: &ProcessActionIntent,
) -> PathBuf {
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
