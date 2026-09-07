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
    pub(super) gpg_agent_sockets: Option<crate::GpgAgentSockets>,
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
            gpg_agent_sockets: None,
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
            gpg_agent_sockets: None,
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

    /// Installs discovered GnuPG sockets without granting access to them.
    #[must_use]
    pub fn with_gpg_agent_sockets(mut self, sockets: crate::GpgAgentSockets) -> Self {
        self.gpg_agent_sockets = Some(sockets);
        self
    }

    pub(super) fn gpg_client(&self) -> Option<&crate::GpgAgentSockets> {
        self.gpg_agent_sockets
            .as_ref()
            .filter(|_| self.host_integrations.contains(&HostIntegration::GpgAgent))
    }

    /// Selects a validated SSH socket path without granting access to it.
    pub fn with_ssh_agent_socket(
        mut self,
        path: impl Into<PathBuf>,
    ) -> Result<Self, ProcessRunnerError> {
        let path = path.into();
        validate_clean_absolute_path(&path, "SSH agent socket")?;
        self.ssh_agent_socket = Some(path);
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
                HostIntegration::GpgAgent => self.gpg_agent_sockets.is_some(),
            };
            if !available {
                return Err(ProcessRunnerError::infrastructure(format!(
                    "host integration `{}` is not available in the current sandbox environment",
                    integration.as_str()
                )));
            }
        }
        for (_, socket, _) in self
            .host_integration_candidates()
            .into_iter()
            .filter(|(integration, _, _)| integrations.contains(integration))
        {
            validate_host_socket(&socket)?;
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
        if let Some(sockets) = &self.gpg_agent_sockets {
            candidates.push((
                HostIntegration::GpgAgent,
                sockets.agent().to_path_buf(),
                Some(sockets.home().as_os_str().to_owned()),
            ));
        }
        candidates
    }

    pub(super) fn host_integration_hidden_paths(&self) -> Vec<PathBuf> {
        let allowed = self
            .host_integration_bindings()
            .into_iter()
            .map(|(_, path, _)| crate::resolve_bwrap_path(&path))
            .collect::<Vec<_>>();
        let mut hidden = self
            .host_integration_candidates()
            .into_iter()
            .map(|(_, path, _)| path)
            .collect::<Vec<_>>();
        if let Some(sockets) = &self.gpg_agent_sockets {
            hidden.extend(sockets.paths().map(Path::to_path_buf));
        }
        hidden.retain(|path| !allowed.contains(&crate::resolve_bwrap_path(path)));
        hidden.sort();
        hidden.dedup();
        hidden
    }

    pub(super) fn host_integration_bindings(
        &self,
    ) -> Vec<(HostIntegration, PathBuf, Option<OsString>)> {
        self.host_integration_candidates()
            .into_iter()
            .filter(|(integration, socket, _)| {
                self.host_integrations.contains(integration) && validate_host_socket(socket).is_ok()
            })
            .collect()
    }
}

fn validate_host_socket(path: &Path) -> Result<(), ProcessRunnerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = crate::HostPathMetadata::inspect(path).map_err(|error| {
            ProcessRunnerError::infrastructure(format!(
                "host integration socket `{}` is unavailable: {error}",
                path.display()
            ))
        })?;
        let owner = fs::metadata("/proc/self").map_err(|error| {
            ProcessRunnerError::infrastructure(format!(
                "cannot validate host integration owner: {error}"
            ))
        })?;
        if !metadata.is_owned_socket(owner.uid()) {
            return Err(ProcessRunnerError::infrastructure(format!(
                "host integration `{}` must be a socket owned by the current user",
                path.display()
            )));
        }
    }
    Ok(())
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
