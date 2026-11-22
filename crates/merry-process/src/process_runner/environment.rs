use merry_runtime::{HostIntegration, ProcessActionIntent, ProcessRunnerError};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Component, Path, PathBuf},
};

pub(super) const ACTION_SANDBOX_HOME_FALLBACK: &str = "/home/merry";
pub(super) const ACTION_SANDBOX_TMPDIR: &str = "/tmp";
pub(super) const ACTION_SANDBOX_PATH_FALLBACK: &str = "/usr/local/bin:/usr/bin:/bin";

/// PATH value that Merry passes to sandboxed action commands.
///
/// The value is the current process `PATH` when it is set and non-empty,
/// otherwise the fallback used when an action environment is built. Callers
/// that describe the action environment, such as
/// a prompt naming the command-line tools a sandboxed command may use, resolve
/// the value through here so the description follows the PATH the sandbox
/// actually passes instead of guessing.
#[must_use]
pub fn action_process_path() -> OsString {
    env::var_os("PATH")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from(ACTION_SANDBOX_PATH_FALLBACK))
}

/// Host-derived paths used to construct one action sandbox.
///
/// HOME, PATH, and the rest of the caller's environment reach the child. The
/// confinement layer then grants the workspace, baseline directories, and any
/// enabled host-integration endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProcessEnvironment {
    pub(super) path: OsString,
    pub(super) home: PathBuf,
    pub(super) tmp_source: PathBuf,
    pub(super) overrides: Vec<(OsString, OsString)>,
    pub(super) host_integrations: Vec<HostIntegration>,
    pub(super) ssh_agent_socket: Option<PathBuf>,
    pub(super) session_bus_address: Option<OsString>,
    pub(super) gpg_agent_sockets: Option<crate::GpgAgentSockets>,
}

/// One configured host integration whose endpoint validated for this action.
///
/// The socket is the object a sandbox mounts read-only, and the environment
/// value is what the client needs next to it: the session bus address for
/// `dbus`, the private GnuPG home for `gpg-agent`, and nothing for the SSH
/// agent, whose client reads the socket path directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HostIntegrationBinding {
    pub(super) integration: HostIntegration,
    pub(super) socket: PathBuf,
    pub(super) environment_value: Option<OsString>,
}

impl WorkspaceProcessEnvironment {
    /// Builds an environment layout from the current process environment.
    ///
    /// The resulting action plan intentionally does not use `--clearenv`; the
    /// caller's environment reaches the child and Merry overrides only the
    /// validated process defaults and configured assignments.
    #[must_use]
    pub fn from_current_process() -> Self {
        let path = action_process_path();
        let home = merry_core::host_paths::user_home_dir()
            .unwrap_or_else(|| absolute_env_path("HOME", ACTION_SANDBOX_HOME_FALLBACK));
        let tmp_source = if cfg!(windows) {
            merry_core::host_paths::simplify_verbatim_path(&env::temp_dir())
        } else {
            absolute_env_path("TMPDIR", ACTION_SANDBOX_TMPDIR)
        };
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

    pub(crate) fn gpg_client(&self) -> Option<&crate::GpgAgentSockets> {
        self.gpg_agent_sockets
            .as_ref()
            .filter(|_| self.host_integrations.contains(&HostIntegration::GpgAgent))
    }

    pub(crate) fn path_value(&self) -> &OsStr {
        &self.path
    }

    pub(crate) fn home_path(&self) -> &Path {
        &self.home
    }

    pub(crate) fn temporary_directory(&self) -> &Path {
        &self.tmp_source
    }

    pub(crate) fn environment_overrides(&self) -> &[(OsString, OsString)] {
        &self.overrides
    }

    pub(crate) fn integration_enabled(&self, integration: HostIntegration) -> bool {
        self.host_integrations.contains(&integration)
    }

    /// Socket and public client files an enabled integration may open.
    pub(crate) fn sandbox_socket_paths(&self) -> Vec<PathBuf> {
        let mut paths = self
            .host_integration_bindings()
            .into_iter()
            .map(|binding| binding.socket)
            .collect::<Vec<_>>();
        if let Some(sockets) = self.gpg_client() {
            paths.extend(sockets.paths().map(Path::to_path_buf));
        }
        paths
    }

    /// Existing SSH trust files for an enabled agent. Private keys stay out.
    pub(crate) fn ssh_known_hosts_paths(&self) -> Vec<PathBuf> {
        if !self.integration_enabled(HostIntegration::SshAgent) {
            return Vec::new();
        }
        crate::ssh_known_hosts(&self.home)
            .into_iter()
            .filter(|path| path.exists())
            .collect()
    }

    /// Public keyring files for an enabled GnuPG agent.
    pub(crate) fn gpg_public_keyrings(&self) -> Vec<PathBuf> {
        self.gpg_client()
            .map(|sockets| {
                sockets
                    .public_keyrings()
                    .filter(|path| path.exists())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Environment assignments for integrations this action enabled.
    pub(crate) fn integration_environment(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        for binding in self.host_integration_bindings() {
            let Some(value) = integration_environment_value(&binding) else {
                continue;
            };
            pairs.push(value);
        }
        pairs
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

    /// Validates the host paths before constructing one action environment.
    ///
    /// The temporary directory must resolve to a real temporary directory.
    /// The returned environment uses its resolved path so a symlink cannot
    /// change the source after validation into a non-temporary tree.
    pub fn validate_for_workspace(
        &self,
        workspace_root: &Path,
    ) -> Result<Self, ProcessRunnerError> {
        validate_clean_absolute_path(workspace_root, "action workspace root")?;
        validate_clean_absolute_path(&self.home, "sandbox process HOME")?;
        if self.home == Path::new("/")
            || self.home == Path::new("/home")
            || self.home == Path::new("/Users")
        {
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
                "sandbox process TMPDIR must resolve under /tmp, /var/tmp, /dev/shm, a macOS per-user temporary directory, or a runtime temporary subdirectory",
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
        for binding in self
            .host_integration_candidates()
            .into_iter()
            .filter(|binding| integrations.contains(&binding.integration))
        {
            validate_host_socket(&binding.socket)?;
        }
        Ok(())
    }

    /// Returns every configured integration endpoint this environment can name,
    /// whether or not it is enabled or its socket currently validates.
    pub(super) fn host_integration_candidates(&self) -> Vec<HostIntegrationBinding> {
        let mut candidates = Vec::new();
        if let Some(path) = self
            .ssh_agent_socket
            .as_ref()
            .filter(|path| is_clean_absolute_path(path))
        {
            candidates.push(HostIntegrationBinding {
                integration: HostIntegration::SshAgent,
                socket: path.clone(),
                environment_value: None,
            });
        }
        if let Some((socket, address)) = self
            .session_bus_address
            .as_ref()
            .and_then(|address| session_bus_socket_path(address).map(|socket| (socket, address)))
            .filter(|(socket, _)| is_clean_absolute_path(socket))
        {
            candidates.push(HostIntegrationBinding {
                integration: HostIntegration::SessionBus,
                socket,
                environment_value: Some(address.clone()),
            });
        }
        if let Some(sockets) = &self.gpg_agent_sockets {
            candidates.push(HostIntegrationBinding {
                integration: HostIntegration::GpgAgent,
                socket: sockets.agent().to_path_buf(),
                environment_value: Some(sockets.home().as_os_str().to_owned()),
            });
        }
        candidates
    }

    /// Returns the enabled integrations whose endpoints validated, in a stable
    /// order. One plan resolves this once and shares it between mounts and the
    /// client environment.
    pub(super) fn host_integration_bindings(&self) -> Vec<HostIntegrationBinding> {
        self.host_integration_candidates()
            .into_iter()
            .filter(|binding| {
                self.host_integrations.contains(&binding.integration)
                    && validate_host_socket(&binding.socket).is_ok()
            })
            .collect()
    }
}

fn integration_environment_value(binding: &HostIntegrationBinding) -> Option<(String, String)> {
    match binding.integration {
        HostIntegration::SshAgent => binding
            .socket
            .to_str()
            .map(|path| ("SSH_AUTH_SOCK".to_owned(), path.to_owned())),
        HostIntegration::SessionBus => binding
            .environment_value
            .as_ref()
            .and_then(|value| value.to_str())
            .map(|value| ("DBUS_SESSION_BUS_ADDRESS".to_owned(), value.to_owned())),
        HostIntegration::GpgAgent => binding
            .environment_value
            .as_ref()
            .and_then(|value| value.to_str())
            .map(|value| ("GNUPGHOME".to_owned(), value.to_owned())),
    }
}

fn validate_host_socket(path: &Path) -> Result<(), ProcessRunnerError> {
    #[cfg(unix)]
    {
        let metadata = crate::HostPathMetadata::inspect(path).map_err(|error| {
            ProcessRunnerError::infrastructure(format!(
                "host integration socket `{}` is unavailable: {error}",
                path.display()
            ))
        })?;
        let current_uid = rustix::process::getuid().as_raw();
        if !metadata.is_owned_socket(current_uid) {
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
    if !merry_core::host_paths::is_clean_absolute_path(path) {
        return Err(ProcessRunnerError::infrastructure(format!(
            "{label} must be a clean absolute path"
        )));
    }
    Ok(())
}

fn is_clean_absolute_path(path: &Path) -> bool {
    merry_core::host_paths::is_clean_absolute_path(path)
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
    is_supported_temp_path_for(
        path,
        cfg!(target_os = "macos"),
        env::var_os("XDG_RUNTIME_DIR").as_deref(),
    )
}

fn is_supported_temp_path_for(
    path: &Path,
    allow_macos_temp_roots: bool,
    xdg_runtime_dir: Option<&OsStr>,
) -> bool {
    is_standard_temp_path(path)
        || (allow_macos_temp_roots && is_macos_temp_path(path))
        || xdg_runtime_dir
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

/// macOS canonicalizes `/tmp` to `/private/tmp`, `/var/tmp` to
/// `/private/var/tmp`, and per-user `TMPDIR` (`_CS_DARWIN_USER_TEMP_DIR`) to
/// `/private/var/folders/<bucket>/<token>/T` (or a subdirectory inside it).
/// Sibling directories under `/private/var/folders` such as `C` (caches) or
/// `0` (per-user root) are excluded so `TMPDIR` remains a temporary tree.
fn is_macos_temp_path(path: &Path) -> bool {
    if [Path::new("/private/tmp"), Path::new("/private/var/tmp")]
        .into_iter()
        .any(|root| path == root || path.starts_with(root))
    {
        return true;
    }
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::RootDir)) {
        return false;
    }
    let is_literal = |component: Option<Component<'_>>, expected: &str| matches!(component, Some(Component::Normal(value)) if value == OsStr::new(expected));
    let is_non_empty = |component: Option<Component<'_>>| matches!(component, Some(Component::Normal(value)) if !value.is_empty());
    if !is_literal(components.next(), "private")
        || !is_literal(components.next(), "var")
        || !is_literal(components.next(), "folders")
        || !is_non_empty(components.next())
        || !is_non_empty(components.next())
        || !is_literal(components.next(), "T")
    {
        return false;
    }
    components.all(|component| matches!(component, Component::Normal(_)))
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

#[cfg(test)]
mod tests {
    use super::{is_macos_temp_path, is_supported_temp_path_for};
    use std::{ffi::OsStr, path::Path};

    #[test]
    fn macos_temp_paths_accept_private_tmp_var_tmp_and_darwin_user_temp_dirs() {
        for valid in [
            "/private/tmp",
            "/private/tmp/merry-sub",
            "/private/var/tmp",
            "/private/var/tmp/nested/dir",
            "/private/var/folders/pp/tfktnmsx7dx1xcbx05v7rzn40000gn/T",
            "/private/var/folders/pp/tfktnmsx7dx1xcbx05v7rzn40000gn/T/merry-work",
        ] {
            let path = Path::new(valid);
            assert!(
                is_macos_temp_path(path),
                "expected macOS temp path: {valid}"
            );
            assert!(
                is_supported_temp_path_for(path, true, None),
                "expected supported on macOS: {valid}"
            );
            assert!(
                !is_supported_temp_path_for(path, false, None),
                "macOS-only root must not be accepted on non-macOS hosts: {valid}"
            );
        }
    }

    #[test]
    fn macos_temp_paths_reject_cache_user_root_and_arbitrary_directories() {
        for invalid in [
            "/private",
            "/private/var",
            "/private/var/folders",
            "/private/var/folders/pp",
            "/private/var/folders/pp/tfktnmsx7dx1xcbx05v7rzn40000gn",
            "/private/var/folders/pp/tfktnmsx7dx1xcbx05v7rzn40000gn/C",
            "/private/var/folders/pp/tfktnmsx7dx1xcbx05v7rzn40000gn/0",
            "/Users/alice",
            "/Users/alice/tmp",
        ] {
            let path = Path::new(invalid);
            assert!(
                !is_macos_temp_path(path),
                "unexpectedly accepted macOS temp path: {invalid}"
            );
            assert!(
                !is_supported_temp_path_for(path, true, Some(OsStr::new("/run/user/1000"))),
                "unexpectedly accepted path: {invalid}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn standard_and_xdg_runtime_temp_paths_remain_supported() {
        assert!(is_supported_temp_path_for(Path::new("/tmp"), false, None));
        assert!(is_supported_temp_path_for(
            Path::new("/var/tmp/work"),
            false,
            None
        ));
        assert!(is_supported_temp_path_for(
            Path::new("/run/user/1000/merry-tmp"),
            false,
            Some(OsStr::new("/run/user/1000"))
        ));
        assert!(!is_supported_temp_path_for(
            Path::new("/run/user/1000"),
            false,
            Some(OsStr::new("/run/user/1000"))
        ));
    }
}
