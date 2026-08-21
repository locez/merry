use crate::config::{self, EffectiveLogSettings, MerryConfig, XdgPaths};
use crate::provider_config::MERRY_OPENAI_DEBUG_ENV;
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, HostIntegration, PathAccess, PathAccessRule,
    PathAccessRuleSource,
};
use std::{
    env,
    ffi::{OsStr, OsString},
    fmt, fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileTypeExt, MetadataExt};

#[cfg(test)]
mod tests;

const BWRAP_PROGRAM: &str = "bwrap";
const DEFAULT_SANDBOX_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
const SANDBOX_ETC_READ_ONLY_FILE_PATHS: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
];
const SANDBOX_ETC_READ_ONLY_DIR_PATHS: &[&str] = &[
    "/etc/ld.so.conf.d",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
];

pub(crate) const MERRY_SANDBOX_ENV: &str = "MERRY_SANDBOX";
pub(crate) const MERRY_SANDBOX_VERSION_ENV: &str = "MERRY_SANDBOX_VERSION";
pub(crate) const MERRY_SANDBOX_VERSION: &str = "1";
pub(crate) const SANDBOX_CHILD_HANDOFF_ARG: &str = "--merry-sandbox-child-handoff";
pub(crate) const SANDBOX_CHILD_HANDOFF_CLI_BWRAP: &str = "cli-bwrap";
pub(crate) const SANDBOX_HOME_ROOT: &str = "/home";
pub(crate) const SANDBOX_TMPDIR: &str = "/tmp";
pub(crate) const SANDBOX_WAYLAND_RUNTIME_DIR: &str = "/run/merry-wayland";
pub(crate) const SANDBOX_WAYLAND_DISPLAY: &str = "wayland-0";
pub(crate) const SANDBOX_WAYLAND_SOCKET: &str = "/run/merry-wayland/wayland-0";
pub(crate) const SANDBOX_X11_AUTHORITY: &str = "/run/merry-x11/Xauthority";

/// Paths needed by ordinary local development commands. These paths are
/// visible to the inner action sandbox by default, but remain bounded by the
/// outer sandbox and do not include the user's home-level credentials file.
pub(crate) fn default_development_path_rules(home: &Path) -> Vec<PathAccessRule> {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".rustup"));
    let cache_home = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".cache"));

    [
        (home.join(".local/bin"), PathAccess::ReadOnly),
        (cargo_home.join("bin"), PathAccess::ReadOnly),
        (cargo_home.join("registry"), PathAccess::ReadWrite),
        (cargo_home.join("git"), PathAccess::ReadWrite),
        (rustup_home.join("toolchains"), PathAccess::ReadOnly),
        (cache_home, PathAccess::ReadWrite),
    ]
    .into_iter()
    .filter(|(path, _)| is_clean_absolute_path(path))
    .map(|(path, access)| {
        PathAccessRule::new(
            path,
            access,
            PathAccessRuleSource::DefaultDevelopmentBaseline,
        )
    })
    .collect()
}

/// Inner action baseline for common development paths. The outer sandbox may
/// expose these paths read-write as a capability ceiling, but each action starts
/// with them read-only until a session grant upgrades the exact path.
pub(crate) fn default_inner_development_path_rules(home: &Path) -> Vec<PathAccessRule> {
    default_development_path_rules(home)
        .into_iter()
        .map(|rule| {
            PathAccessRule::new(
                rule.path().to_path_buf(),
                PathAccess::ReadOnly,
                rule.source(),
            )
        })
        .collect()
}

#[cfg(test)]
pub(crate) const SANDBOX_HOME: &str = "/home/alice";
#[cfg(test)]
pub(crate) const SANDBOX_XDG_CONFIG_HOME: &str = "/host/config";
#[cfg(test)]
pub(crate) const SANDBOX_XDG_STATE_HOME: &str = "/host/state";
#[cfg(test)]
pub(crate) const SANDBOX_MERRY_CONFIG_DIR: &str = "/host/config/merry";
#[cfg(test)]
pub(crate) const SANDBOX_MERRY_MANAGED_CONFIG_DIR: &str = "/host/config/merry/managed";
#[cfg(test)]
pub(crate) const SANDBOX_MERRY_STATE_DIR: &str = "/host/state/merry";
#[cfg(test)]
pub(crate) const SANDBOX_MERRY_LOG_DIR: &str = "/host/state/merry/logs";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardAccess {
    Disabled,
    Tui,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GraphicalEnvironment {
    xdg_runtime_dir: Option<PathBuf>,
    wayland_display: Option<OsString>,
    display: Option<OsString>,
    xauthority: Option<PathBuf>,
    home: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HostIntegrationEnvironment {
    ssh_agent_socket: Option<PathBuf>,
    session_bus_address: Option<OsString>,
}

impl HostIntegrationEnvironment {
    fn from_env() -> Self {
        Self {
            ssh_agent_socket: env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            session_bus_address: env::var_os("DBUS_SESSION_BUS_ADDRESS"),
        }
    }
}

impl GraphicalEnvironment {
    fn from_env() -> Self {
        Self {
            xdg_runtime_dir: env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            wayland_display: env::var_os("WAYLAND_DISPLAY"),
            display: env::var_os("DISPLAY"),
            xauthority: env::var_os("XAUTHORITY").map(PathBuf::from),
            home: env::var_os("HOME").map(PathBuf::from),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostPathKind {
    RegularFile,
    UnixSocket,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostPathMetadata {
    kind: HostPathKind,
    owner_uid: u32,
}

impl HostPathMetadata {
    pub(crate) const fn new(kind: HostPathKind, owner_uid: u32) -> Self {
        Self { kind, owner_uid }
    }
}

pub(crate) trait HostPathProbe {
    fn file_exists(&self, path: &Path) -> bool;
    fn metadata(&self, path: &Path) -> Option<HostPathMetadata>;
}

struct FilesystemHostProbe;

impl HostPathProbe for FilesystemHostProbe {
    fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn metadata(&self, path: &Path) -> Option<HostPathMetadata> {
        filesystem_path_metadata(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ChildHandoff {
    #[value(name = "cli-bwrap")]
    CliBwrap,
}

impl ChildHandoff {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::CliBwrap => SANDBOX_CHILD_HANDOFF_CLI_BWRAP,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeProfile {
    CliBwrap,
}

/// Validates the outer sandbox handoff before admitting the local workspace
/// process profile to a product runtime.
pub(crate) fn local_workspace_process_admission(
    accept_local_workspace_process_risk: bool,
    sandbox_child_handoff: Option<ChildHandoff>,
    sandbox_runtime_profile: Option<RuntimeProfile>,
    sandbox: Option<&OsStr>,
    version: Option<&OsStr>,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
    if accept_local_workspace_process_risk
        && sandbox_child_handoff == Some(ChildHandoff::CliBwrap)
        && sandbox_runtime_profile == Some(RuntimeProfile::CliBwrap)
        && sandbox == Some(OsStr::new("1"))
        && version == Some(OsStr::new(MERRY_SANDBOX_VERSION))
    {
        Some(AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace())
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Host {
    pub(crate) cwd: PathBuf,
    pub(crate) current_exe: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) path: Option<OsString>,
    pub(crate) openai_debug: Option<OsString>,
    pub(crate) inside_sandbox: bool,
    pub(crate) xdg_paths: XdgPaths,
    pub(crate) log_settings: Option<EffectiveLogSettings>,
    pub(crate) trusted_path_rules: Vec<PathAccessRule>,
    pub(crate) graphical_environment: GraphicalEnvironment,
    pub(crate) host_integrations: Vec<HostIntegration>,
    pub(crate) host_integration_environment: HostIntegrationEnvironment,
    pub(crate) development_environment: Vec<(OsString, OsString)>,
    pub(crate) current_uid: u32,
}

impl Host {
    fn from_env(args: Vec<OsString>) -> Result<Self, Error> {
        let xdg_paths = XdgPaths::from_env().map_err(Error::Config)?;
        let merry_config = MerryConfig::load_optional(&xdg_paths).map_err(Error::Config)?;
        let log_settings = merry_config
            .as_ref()
            .map(|config| config.effective_log_settings(&xdg_paths))
            .transpose()
            .map_err(Error::Config)?
            .flatten();
        let trusted_path_rules = merry_config
            .as_ref()
            .map(MerryConfig::trusted_global_path_rules)
            .transpose()
            .map_err(Error::Config)?
            .unwrap_or_default();
        let host_integrations = merry_config
            .as_ref()
            .map(MerryConfig::host_integrations)
            .unwrap_or_default();
        let development_environment = ["CARGO_HOME", "RUSTUP_HOME", "XDG_CACHE_HOME"]
            .into_iter()
            .filter_map(|name| env::var_os(name).map(|value| (os(name), value)))
            .collect();
        Ok(Self {
            cwd: env::current_dir().map_err(Error::CurrentDir)?,
            current_exe: env::current_exe().map_err(Error::CurrentExe)?,
            args,
            path: env::var_os("PATH"),
            openai_debug: env::var_os(MERRY_OPENAI_DEBUG_ENV),
            // This marker is only a recursion guard for self-reexec. It is
            // not a security proof that the current process is confined.
            inside_sandbox: env::var_os(MERRY_SANDBOX_ENV).as_deref() == Some(OsStr::new("1")),
            xdg_paths,
            log_settings,
            trusted_path_rules,
            graphical_environment: GraphicalEnvironment::from_env(),
            host_integrations,
            host_integration_environment: HostIntegrationEnvironment::from_env(),
            development_environment,
            current_uid: current_process_uid()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
    pub(crate) env: Vec<(OsString, OsString)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Bootstrap {
    Disabled,
    AlreadyInside,
    Reexec(Plan),
}

pub(crate) fn maybe_reexec(
    with_sandbox: bool,
    clipboard_access: ClipboardAccess,
    args: Vec<OsString>,
) -> Result<(), Error> {
    let host = Host::from_env(args)?;
    match plan_bootstrap(with_sandbox, clipboard_access, &host)? {
        Bootstrap::Disabled | Bootstrap::AlreadyInside => Ok(()),
        Bootstrap::Reexec(plan) => exec(plan),
    }
}

pub(crate) fn ensure_bubblewrap_available() -> Result<(), Error> {
    #[cfg(not(target_os = "linux"))]
    {
        return Err(Error::UnsupportedPlatform);
    }
    #[cfg(target_os = "linux")]
    {
        let path = env::var_os("PATH")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| os(DEFAULT_SANDBOX_PATH));
        find_bwrap_in_path(&path, Path::exists)
            .map(|_| ())
            .ok_or(Error::MissingBubblewrap)
    }
}

pub(crate) fn plan_bootstrap(
    with_sandbox: bool,
    clipboard_access: ClipboardAccess,
    host: &Host,
) -> Result<Bootstrap, Error> {
    plan_bootstrap_with_probe_inner(
        with_sandbox,
        clipboard_access,
        host,
        &FilesystemHostProbe,
        true,
    )
}

#[cfg(test)]
pub(crate) fn plan_bootstrap_with_file_exists(
    with_sandbox: bool,
    host: &Host,
    file_exists: impl Fn(&Path) -> bool,
) -> Result<Bootstrap, Error> {
    struct FileExistsProbe<F>(F);

    impl<F> HostPathProbe for FileExistsProbe<F>
    where
        F: Fn(&Path) -> bool,
    {
        fn file_exists(&self, path: &Path) -> bool {
            (self.0)(path)
        }

        fn metadata(&self, _path: &Path) -> Option<HostPathMetadata> {
            None
        }
    }

    plan_bootstrap_with_probe_inner(
        with_sandbox,
        ClipboardAccess::Disabled,
        host,
        &FileExistsProbe(file_exists),
        false,
    )
}

#[cfg(test)]
pub(crate) fn plan_bootstrap_with_probe(
    with_sandbox: bool,
    clipboard_access: ClipboardAccess,
    host: &Host,
    probe: &impl HostPathProbe,
) -> Result<Bootstrap, Error> {
    plan_bootstrap_with_probe_inner(with_sandbox, clipboard_access, host, probe, false)
}

fn plan_bootstrap_with_probe_inner(
    with_sandbox: bool,
    clipboard_access: ClipboardAccess,
    host: &Host,
    probe: &impl HostPathProbe,
    prepare_host_writable_dirs: bool,
) -> Result<Bootstrap, Error> {
    if !with_sandbox {
        return Ok(Bootstrap::Disabled);
    }

    if host.inside_sandbox {
        return Ok(Bootstrap::AlreadyInside);
    }

    validate_outer_paths(host)?;

    let path = sandbox_path(host);
    let bwrap = find_bwrap_in_path(&path, |candidate| probe.file_exists(candidate))
        .ok_or(Error::MissingBubblewrap)?;
    SandboxPathPlan::new(host)?;
    if prepare_host_writable_dirs {
        ensure_host_managed_provider_directories(host)?;
        ensure_host_state_directory(host)?;
    } else {
        ensure_host_log_directory(host)?;
    }
    // Directory preparation may materialize previously missing components.
    // Re-check the filesystem identity immediately before producing mounts.
    let path_plan = SandboxPathPlan::new(host)?;

    Ok(Bootstrap::Reexec(build_plan(
        host,
        path,
        bwrap,
        clipboard_access,
        probe,
        &path_plan,
    )))
}

#[derive(Debug, Clone)]
struct SandboxPathPlan {
    development_rules: Vec<PathAccessRule>,
    trusted_rules: Vec<PathAccessRule>,
    trusted_product_rules: Vec<PathAccessRule>,
}

impl SandboxPathPlan {
    fn new(host: &Host) -> Result<Self, Error> {
        let product_paths = [
            host.xdg_paths.state_dir().to_path_buf(),
            host.xdg_paths.managed_config_dir(),
        ];
        for product_path in &product_paths {
            validate_product_path_identity(product_path)?;
        }

        validate_trusted_rule_conflicts(&host.trusted_path_rules)?;
        for rule in &host.trusted_path_rules {
            if rule.access() == PathAccess::Deny
                && let Some(product_path) = product_paths
                    .iter()
                    .find(|product_path| paths_overlap(product_path, rule.path()))
            {
                return Err(Error::ProductPathConflictsWithTrustedRule {
                    product_path: product_path.clone(),
                    rule_path: rule.path().to_path_buf(),
                    access: rule.access(),
                });
            }
        }

        let (trusted_rules, trusted_product_rules) = host
            .trusted_path_rules
            .iter()
            .cloned()
            .partition::<Vec<_>, _>(|rule| !path_is_inside_product(rule.path(), &product_paths));

        Ok(Self {
            development_rules: order_path_rules(default_development_path_rules(
                host.xdg_paths.home(),
            )),
            trusted_rules: order_path_rules(trusted_rules),
            trusted_product_rules: order_path_rules(trusted_product_rules),
        })
    }
}

fn validate_product_path_identity(path: &Path) -> Result<(), Error> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => {
                current.pop();
                continue;
            }
            Component::Normal(part) => current.push(part),
        }

        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(Error::ProductPathContainsSymlink {
                    product_path: path.to_path_buf(),
                    symlink_path: current,
                });
            }
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => break,
            Err(source) => {
                return Err(Error::ProductPathMetadata {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn validate_trusted_rule_conflicts(rules: &[PathAccessRule]) -> Result<(), Error> {
    for (index, rule) in rules.iter().enumerate() {
        if let Some(conflicting) = rules[index + 1..]
            .iter()
            .find(|other| other.path() == rule.path() && other.access() != rule.access())
        {
            return Err(Error::ConflictingTrustedPathRules {
                path: rule.path().to_path_buf(),
                first_access: rule.access(),
                second_access: conflicting.access(),
            });
        }
    }
    Ok(())
}

fn order_path_rules(mut rules: Vec<PathAccessRule>) -> Vec<PathAccessRule> {
    rules.sort_by(|left, right| {
        path_depth(left.path())
            .cmp(&path_depth(right.path()))
            .then_with(|| {
                left.path()
                    .to_string_lossy()
                    .cmp(&right.path().to_string_lossy())
            })
    });
    rules.dedup_by(|left, right| left.path() == right.path() && left.access() == right.access());
    rules
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn path_is_inside_product(path: &Path, product_paths: &[PathBuf]) -> bool {
    product_paths
        .iter()
        .any(|product_path| path.starts_with(product_path))
}

fn ensure_host_log_directory(host: &Host) -> Result<(), Error> {
    let Some(log_settings) = host.log_settings.as_ref() else {
        return Ok(());
    };
    let Some(log_dir) = log_settings.path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(log_dir).map_err(|source| Error::LogDirectory {
        path: log_dir.to_path_buf(),
        source,
    })
}

fn ensure_host_state_directory(host: &Host) -> Result<(), Error> {
    fs::create_dir_all(host.xdg_paths.state_dir()).map_err(|source| Error::StateDirectory {
        path: host.xdg_paths.state_dir().to_path_buf(),
        source,
    })?;
    ensure_host_log_directory(host)
}

fn ensure_host_managed_provider_directories(host: &Host) -> Result<(), Error> {
    for path in [
        host.xdg_paths.managed_config_dir(),
        host.xdg_paths.managed_secrets_dir(),
    ] {
        fs::create_dir_all(&path).map_err(|source| Error::ManagedConfigDirectory {
            path: path.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|source| {
                Error::ManagedConfigDirectory {
                    path: path.clone(),
                    source,
                }
            })?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct GraphicalAccessPlan {
    mounts: Vec<GraphicalMount>,
    environment: Vec<(OsString, OsString)>,
}

#[derive(Debug)]
struct GraphicalMount {
    source: PathBuf,
    destination: PathBuf,
}

fn graphical_access_plan(host: &Host, probe: &impl HostPathProbe) -> GraphicalAccessPlan {
    let mut plan = GraphicalAccessPlan::default();

    if let Some(socket) = wayland_socket_path(host, probe) {
        plan.mounts.push(GraphicalMount {
            source: socket,
            destination: PathBuf::from(SANDBOX_WAYLAND_SOCKET),
        });
        plan.environment.extend([
            (os("XDG_RUNTIME_DIR"), os(SANDBOX_WAYLAND_RUNTIME_DIR)),
            (os("WAYLAND_DISPLAY"), os(SANDBOX_WAYLAND_DISPLAY)),
        ]);
    }

    if let Some((display, socket, authority)) = x11_connection(host, probe) {
        plan.mounts.push(GraphicalMount {
            source: socket.clone(),
            destination: socket,
        });
        plan.mounts.push(GraphicalMount {
            source: authority,
            destination: PathBuf::from(SANDBOX_X11_AUTHORITY),
        });
        plan.environment.extend([
            (os("DISPLAY"), OsString::from(display)),
            (os("XAUTHORITY"), os(SANDBOX_X11_AUTHORITY)),
        ]);
    }

    plan
}

fn host_integration_access_plan(host: &Host, probe: &impl HostPathProbe) -> GraphicalAccessPlan {
    let mut plan = GraphicalAccessPlan::default();

    for integration in &host.host_integrations {
        match integration {
            HostIntegration::SshAgent => {
                let Some(socket) = host
                    .host_integration_environment
                    .ssh_agent_socket
                    .as_deref()
                else {
                    continue;
                };
                if !is_clean_absolute_path(socket) || !host_owned_socket(host, probe, socket) {
                    continue;
                }
                plan.mounts.push(GraphicalMount {
                    source: socket.to_path_buf(),
                    destination: socket.to_path_buf(),
                });
                plan.environment
                    .push((os("SSH_AUTH_SOCK"), socket.as_os_str().to_owned()));
            }
            HostIntegration::SessionBus => {
                let Some(address) = host
                    .host_integration_environment
                    .session_bus_address
                    .as_ref()
                else {
                    continue;
                };
                let Some(socket) = session_bus_socket_path(address) else {
                    continue;
                };
                if !is_clean_absolute_path(&socket) || !host_owned_socket(host, probe, &socket) {
                    continue;
                }
                plan.mounts.push(GraphicalMount {
                    source: socket.clone(),
                    destination: socket,
                });
                plan.environment
                    .push((os("DBUS_SESSION_BUS_ADDRESS"), address.clone()));
            }
        }
    }

    plan
}

fn host_owned_socket(host: &Host, probe: &impl HostPathProbe, path: &Path) -> bool {
    probe.metadata(path).is_some_and(|metadata| {
        metadata.kind == HostPathKind::UnixSocket && metadata.owner_uid == host.current_uid
    })
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

fn wayland_socket_path(host: &Host, probe: &impl HostPathProbe) -> Option<PathBuf> {
    let runtime_dir = host.graphical_environment.xdg_runtime_dir.as_deref()?;
    if !is_clean_absolute_path(runtime_dir) {
        return None;
    }
    let display = Path::new(host.graphical_environment.wayland_display.as_deref()?);
    let socket = if display.is_absolute() {
        let relative = display.strip_prefix(runtime_dir).ok()?;
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return None;
        }
        display.to_path_buf()
    } else {
        let mut components = display.components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return None;
        }
        runtime_dir.join(display)
    };
    let metadata = probe.metadata(&socket)?;
    (metadata.kind == HostPathKind::UnixSocket && metadata.owner_uid == host.current_uid)
        .then_some(socket)
}

fn x11_connection(host: &Host, probe: &impl HostPathProbe) -> Option<(String, PathBuf, PathBuf)> {
    let display = parse_local_x11_display(host.graphical_environment.display.as_deref()?)?;
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{}", display.number));
    if probe.metadata(&socket)?.kind != HostPathKind::UnixSocket {
        return None;
    }

    let authority = x11_authority_path(&host.graphical_environment)?;
    let metadata = probe.metadata(&authority)?;
    if metadata.kind != HostPathKind::RegularFile || metadata.owner_uid != host.current_uid {
        return None;
    }

    Some((display.normalized, socket, authority))
}

fn x11_authority_path(environment: &GraphicalEnvironment) -> Option<PathBuf> {
    let path = match environment.xauthority.as_ref() {
        Some(path) => path.clone(),
        None => environment.home.as_ref()?.join(".Xauthority"),
    };
    is_clean_absolute_path(&path).then_some(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalX11Display {
    number: u32,
    normalized: String,
}

fn parse_local_x11_display(value: &OsStr) -> Option<LocalX11Display> {
    let value = value.to_str()?;
    let value = value
        .strip_prefix(':')
        .or_else(|| value.strip_prefix("unix:"))?;
    let (display, screen) = match value.split_once('.') {
        Some((display, screen)) if !screen.contains('.') => (display, Some(screen)),
        Some(_) => return None,
        None => (value, None),
    };
    if display.is_empty()
        || !display.bytes().all(|byte| byte.is_ascii_digit())
        || screen.is_some_and(|screen| {
            screen.is_empty() || !screen.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return None;
    }
    let number = display.parse::<u32>().ok()?;
    let screen = screen.map(str::parse::<u32>).transpose().ok()?;
    let normalized = screen.map_or_else(
        || format!(":{number}"),
        |screen| format!(":{number}.{screen}"),
    );
    Some(LocalX11Display { number, normalized })
}

fn is_clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn validate_outer_paths(host: &Host) -> Result<(), Error> {
    if !is_clean_absolute_path(&host.cwd) {
        return Err(Error::InvalidWorkspacePath(
            "workspace root must be a clean absolute path",
        ));
    }

    let home = host.xdg_paths.home();
    if !is_valid_runtime_home_path(home) {
        return Err(Error::InvalidHomeLayout(
            "HOME must be a clean user path outside /tmp",
        ));
    }
    Ok(())
}

fn build_plan(
    host: &Host,
    path: OsString,
    bwrap: PathBuf,
    clipboard_access: ClipboardAccess,
    probe: &impl HostPathProbe,
    path_plan: &SandboxPathPlan,
) -> Plan {
    let cwd = host.cwd.as_os_str().to_owned();
    let current_exe = host.current_exe.as_os_str().to_owned();
    let home = host.xdg_paths.home().as_os_str().to_owned();
    let config_base = host.xdg_paths.config_base_dir().as_os_str().to_owned();
    let state_base = host.xdg_paths.state_base_dir().as_os_str().to_owned();
    let config_dir = host.xdg_paths.config_dir().to_path_buf();
    let managed_config_dir = host.xdg_paths.managed_config_dir();
    let state_dir = host.xdg_paths.state_dir().to_path_buf();
    let graphical_plan = match clipboard_access {
        ClipboardAccess::Disabled => GraphicalAccessPlan::default(),
        ClipboardAccess::Tui => graphical_access_plan(host, probe),
    };
    let host_integration_plan = host_integration_access_plan(host, probe);

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
        os("--perms"),
        os("01777"),
        os("--tmpfs"),
        os(SANDBOX_TMPDIR),
        os("--tmpfs"),
        os(SANDBOX_HOME_ROOT),
    ];
    if !Path::new(&home).starts_with(Path::new(SANDBOX_HOME_ROOT)) {
        append_mount_parent_args(&mut args, home.as_os_str());
        args.extend([os("--tmpfs"), home.clone()]);
    }
    args.extend([os("--perms"), os("0700"), os("--dir"), home.clone()]);
    append_bind_dir_try_args(&mut args, &config_dir, &config_dir);
    args.extend([
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
    for path in SANDBOX_ETC_READ_ONLY_FILE_PATHS {
        if Path::new(path).exists() {
            append_bind_file_args(&mut args, OsStr::new(path), OsStr::new(path));
        }
    }
    for path in SANDBOX_ETC_READ_ONLY_DIR_PATHS {
        if Path::new(path).exists() {
            append_bind_dir_args(&mut args, OsStr::new(path), OsStr::new(path));
        }
    }
    args.extend([
        os("--bind"),
        cwd.clone(),
        cwd.clone(),
        os("--chdir"),
        cwd.clone(),
    ]);
    if let Some(log_settings) = host.log_settings.as_ref()
        && let Some(host_log_dir) = log_settings.path.parent()
    {
        args.extend([
            os("--bind"),
            host_log_dir.as_os_str().to_owned(),
            host_log_dir.as_os_str().to_owned(),
        ]);
    }
    for rule in &path_plan.development_rules {
        append_path_rule_args(&mut args, rule);
    }
    for rule in &path_plan.trusted_rules {
        append_path_rule_args(&mut args, rule);
    }
    // A trusted read-only parent may be overridden by a narrower product-owned
    // write mount. Trusted read-only children are appended below to retain the
    // user's narrower restriction.
    append_bind_dir_rw_args(&mut args, &state_dir, &state_dir);
    append_bind_dir_rw_args(&mut args, &managed_config_dir, &managed_config_dir);
    for rule in &path_plan.trusted_product_rules {
        append_path_rule_args(&mut args, rule);
    }
    for mount in &graphical_plan.mounts {
        append_bind_file_args(
            &mut args,
            mount.source.as_os_str(),
            mount.destination.as_os_str(),
        );
    }
    for mount in &host_integration_plan.mounts {
        append_bind_file_args(
            &mut args,
            mount.source.as_os_str(),
            mount.destination.as_os_str(),
        );
    }
    args.extend([
        os("--clearenv"),
        os("--setenv"),
        os("PATH"),
        path.clone(),
        os("--setenv"),
        os("HOME"),
        home,
        os("--setenv"),
        os("TMPDIR"),
        os(SANDBOX_TMPDIR),
        os("--setenv"),
        os("XDG_CONFIG_HOME"),
        config_base,
        os("--setenv"),
        os("XDG_STATE_HOME"),
        state_base,
        os("--setenv"),
        os("PWD"),
        cwd,
        os("--setenv"),
        os(MERRY_SANDBOX_ENV),
        os("1"),
        os("--setenv"),
        os(MERRY_SANDBOX_VERSION_ENV),
        os(MERRY_SANDBOX_VERSION),
    ]);
    for (name, value) in graphical_plan.environment {
        args.extend([os("--setenv"), name, value]);
    }
    for (name, value) in host_integration_plan.environment {
        args.extend([os("--setenv"), name, value]);
    }
    for (name, value) in &host.development_environment {
        args.extend([os("--setenv"), name.clone(), value.clone()]);
    }
    if host.openai_debug.as_deref() == Some(OsStr::new("1")) {
        args.extend([os("--setenv"), os(MERRY_OPENAI_DEBUG_ENV), os("1")]);
    }
    args.extend([
        current_exe,
        os(SANDBOX_CHILD_HANDOFF_ARG),
        os(ChildHandoff::CliBwrap.as_cli_value()),
    ]);
    args.extend(args_without_sandbox_bootstrap_flags(&host.args));

    Plan {
        program: bwrap.as_os_str().to_owned(),
        args,
        env: vec![(os("PATH"), path)],
    }
}

pub(crate) fn find_bwrap_in_path(
    path: &OsStr,
    file_exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(BWRAP_PROGRAM))
        .find(|candidate| file_exists(candidate))
}

fn append_bind_file_args(args: &mut Vec<OsString>, source: &OsStr, destination: &OsStr) {
    append_mount_parent_args(args, destination);
    args.extend([os("--ro-bind"), source.to_owned(), destination.to_owned()]);
}

fn append_bind_dir_args(args: &mut Vec<OsString>, source: &OsStr, destination: &OsStr) {
    append_mount_parent_args(args, destination);
    args.extend([os("--ro-bind"), source.to_owned(), destination.to_owned()]);
}

fn append_bind_dir_try_args(args: &mut Vec<OsString>, source: &Path, destination: &Path) {
    append_mount_parent_args(args, destination.as_os_str());
    args.extend([
        os("--ro-bind-try"),
        source.as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ]);
}

fn append_bind_dir_rw_args(args: &mut Vec<OsString>, source: &Path, destination: &Path) {
    append_mount_parent_args(args, destination.as_os_str());
    args.extend([
        os("--bind"),
        source.as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ]);
}

fn append_path_rule_args(args: &mut Vec<OsString>, rule: &PathAccessRule) {
    let path = rule.path().as_os_str();
    match rule.access() {
        PathAccess::ReadOnly => {
            append_mount_parent_args(args, path);
            args.extend([os("--ro-bind-try"), path.to_owned(), path.to_owned()]);
        }
        PathAccess::ReadWrite => {
            append_mount_parent_args(args, path);
            args.extend([os("--bind-try"), path.to_owned(), path.to_owned()]);
        }
        PathAccess::Deny => {
            append_mount_parent_args(args, path);
            args.extend([os("--tmpfs"), path.to_owned()]);
        }
    }
}

fn append_mount_parent_args(args: &mut Vec<OsString>, destination: &OsStr) {
    let Some(destination) = destination.to_str() else {
        return;
    };
    let Some(parent) = Path::new(destination).parent() else {
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

fn sandbox_path(host: &Host) -> OsString {
    host.path
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| os(DEFAULT_SANDBOX_PATH))
}

pub(crate) fn args_without_sandbox_bootstrap_flags(args: &[OsString]) -> Vec<OsString> {
    let mut removed = false;
    let mut sanitized = Vec::with_capacity(args.len());
    let mut index = 0;
    let mut scanning_root_flags = true;

    while index < args.len() {
        let arg = &args[index];

        if scanning_root_flags {
            if !removed && arg == OsStr::new("--with-sandbox") {
                removed = true;
                index += 1;
                continue;
            }

            if arg == OsStr::new(SANDBOX_CHILD_HANDOFF_ARG) {
                index += 1;
                if index < args.len() {
                    index += 1;
                }
                continue;
            }

            if is_child_handoff_assignment(arg) {
                index += 1;
                continue;
            }

            scanning_root_flags = false;
        }

        sanitized.push(arg.clone());
        index += 1;
    }

    sanitized
}

fn is_child_handoff_assignment(arg: &OsStr) -> bool {
    arg.to_str().is_some_and(|value| {
        value
            .strip_prefix(SANDBOX_CHILD_HANDOFF_ARG)
            .is_some_and(|suffix| suffix.starts_with('='))
    })
}

pub(crate) fn os(value: &str) -> OsString {
    OsString::from(value)
}

fn exec(plan: Plan) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        let error = exec_plan(&plan);
        if error.kind() == io::ErrorKind::NotFound {
            Err(Error::MissingBubblewrap)
        } else {
            Err(Error::Exec(error))
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = plan;
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
fn exec_plan(plan: &Plan) -> io::Error {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(&plan.program);
    command.args(&plan.args).env_clear().envs(plan.env.clone());
    command.exec()
}

pub(crate) async fn read_proc_self_mountinfo() -> Option<String> {
    tokio::task::spawn_blocking(|| std::fs::read_to_string("/proc/self/mountinfo"))
        .await
        .ok()?
        .ok()
}

pub(crate) fn runtime_profile_from_evidence(
    home: Option<&OsStr>,
    tmpdir: Option<&OsStr>,
    mountinfo: Option<&str>,
) -> Option<RuntimeProfile> {
    let home = home.map(Path::new)?;
    if !is_valid_runtime_home_path(home) || tmpdir != Some(OsStr::new(SANDBOX_TMPDIR)) {
        return None;
    }
    let home_mount = if home.starts_with(Path::new(SANDBOX_HOME_ROOT)) {
        SANDBOX_HOME_ROOT.to_owned()
    } else {
        home.to_str()?.to_owned()
    };
    let mountinfo = mountinfo?;
    if mountinfo_has_tmpfs_mount(mountinfo, &home_mount)
        && mountinfo_has_tmpfs_mount(mountinfo, SANDBOX_TMPDIR)
    {
        Some(RuntimeProfile::CliBwrap)
    } else {
        None
    }
}

fn is_valid_runtime_home_path(path: &Path) -> bool {
    is_clean_absolute_path(path)
        && path != Path::new("/")
        && path != Path::new(SANDBOX_HOME_ROOT)
        && !path.starts_with(Path::new(SANDBOX_TMPDIR))
}

fn mountinfo_has_tmpfs_mount(mountinfo: &str, mount_point: &str) -> bool {
    mountinfo
        .lines()
        .filter_map(parse_mountinfo_mount)
        .any(|mount| mount.mount_point == mount_point && mount.fs_type == "tmpfs")
}

fn parse_mountinfo_mount(line: &str) -> Option<MountInfoMount<'_>> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let separator_index = fields.iter().position(|field| *field == "-")?;
    if separator_index < 5 || fields.len() <= separator_index + 1 {
        return None;
    }

    Some(MountInfoMount {
        mount_point: fields[4],
        fs_type: fields[separator_index + 1],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountInfoMount<'a> {
    mount_point: &'a str,
    fs_type: &'a str,
}

#[cfg(target_os = "linux")]
fn filesystem_path_metadata(path: &Path) -> Option<HostPathMetadata> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let file_type = metadata.file_type();
    let kind = if file_type.is_socket() {
        HostPathKind::UnixSocket
    } else if file_type.is_file() {
        HostPathKind::RegularFile
    } else {
        HostPathKind::Other
    };
    Some(HostPathMetadata::new(kind, metadata.uid()))
}

#[cfg(not(target_os = "linux"))]
fn filesystem_path_metadata(_path: &Path) -> Option<HostPathMetadata> {
    None
}

#[cfg(target_os = "linux")]
fn current_process_uid() -> Result<u32, Error> {
    fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .map_err(Error::CurrentUser)
}

#[cfg(not(target_os = "linux"))]
fn current_process_uid() -> Result<u32, Error> {
    Ok(0)
}

#[derive(Debug)]
pub(crate) enum Error {
    CurrentDir(io::Error),
    CurrentExe(io::Error),
    CurrentUser(io::Error),
    Config(config::ConfigError),
    LogDirectory {
        path: PathBuf,
        source: io::Error,
    },
    StateDirectory {
        path: PathBuf,
        source: io::Error,
    },
    ManagedConfigDirectory {
        path: PathBuf,
        source: io::Error,
    },
    #[cfg(not(target_os = "linux"))]
    UnsupportedPlatform,
    MissingBubblewrap,
    InvalidWorkspacePath(&'static str),
    InvalidHomeLayout(&'static str),
    ProductPathConflictsWithTrustedRule {
        product_path: PathBuf,
        rule_path: PathBuf,
        access: PathAccess,
    },
    ProductPathContainsSymlink {
        product_path: PathBuf,
        symlink_path: PathBuf,
    },
    ProductPathMetadata {
        path: PathBuf,
        source: io::Error,
    },
    ConflictingTrustedPathRules {
        path: PathBuf,
        first_access: PathAccess,
        second_access: PathAccess,
    },
    Exec(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CurrentDir(error) => write!(
                formatter,
                "failed to read current directory before sandbox bootstrap: {error}"
            ),
            Error::CurrentExe(error) => write!(
                formatter,
                "failed to locate current executable before sandbox bootstrap: {error}"
            ),
            Error::CurrentUser(error) => write!(
                formatter,
                "failed to identify the current user before sandbox bootstrap: {error}"
            ),
            Error::Config(error) => write!(
                formatter,
                "failed to load Merry config before sandbox bootstrap: {error}"
            ),
            Error::LogDirectory { path, source } => write!(
                formatter,
                "failed to create host log directory {} before sandbox bootstrap: {source}",
                path.display()
            ),
            Error::StateDirectory { path, source } => write!(
                formatter,
                "failed to create host state directory {} before sandbox bootstrap: {source}",
                path.display()
            ),
            Error::ManagedConfigDirectory { path, source } => write!(
                formatter,
                "failed to prepare managed provider directory {} before sandbox bootstrap: {source}",
                path.display()
            ),
            #[cfg(not(target_os = "linux"))]
            Error::UnsupportedPlatform => write!(
                formatter,
                "Merry's product sandbox is supported only on Linux with bubblewrap; debug commands can omit --with-sandbox"
            ),
            Error::MissingBubblewrap => write!(
                formatter,
                "bubblewrap executable `bwrap` was not found in PATH; install bubblewrap to use TUI/run, or omit --with-sandbox for debug commands"
            ),
            Error::InvalidWorkspacePath(reason) => {
                write!(formatter, "sandbox workspace path is invalid: {reason}")
            }
            Error::InvalidHomeLayout(reason) => {
                write!(formatter, "sandbox HOME layout is invalid: {reason}")
            }
            Error::ProductPathConflictsWithTrustedRule {
                product_path,
                rule_path,
                access,
            } => write!(
                formatter,
                "sandbox product path {} conflicts with trusted global {} rule {}",
                product_path.display(),
                access.as_str(),
                rule_path.display()
            ),
            Error::ProductPathContainsSymlink {
                product_path,
                symlink_path,
            } => write!(
                formatter,
                "sandbox product path {} contains symlink component {}; refusing writable mount",
                product_path.display(),
                symlink_path.display()
            ),
            Error::ProductPathMetadata { path, source } => write!(
                formatter,
                "failed to inspect sandbox product path component {}: {source}",
                path.display()
            ),
            Error::ConflictingTrustedPathRules {
                path,
                first_access,
                second_access,
            } => write!(
                formatter,
                "trusted sandbox path {} has conflicting {} and {} rules",
                path.display(),
                first_access.as_str(),
                second_access.as_str()
            ),
            Error::Exec(error) => {
                write!(
                    formatter,
                    "failed to execute bubblewrap sandbox bootstrap: {error}"
                )
            }
        }
    }
}
