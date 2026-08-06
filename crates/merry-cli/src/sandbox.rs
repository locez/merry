use crate::config::{self, EffectiveLogSettings, MerryConfig, XdgPaths};
use crate::provider_config::MERRY_OPENAI_DEBUG_ENV;
use merry_runtime::{PathAccess, PathAccessRule};
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
pub(crate) const SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1: &str = "cli-bwrap-v1";
pub(crate) const SANDBOX_HOME_ROOT: &str = "/home";
pub(crate) const SANDBOX_TMPDIR: &str = "/tmp";
pub(crate) const SANDBOX_WAYLAND_RUNTIME_DIR: &str = "/run/merry-wayland";
pub(crate) const SANDBOX_WAYLAND_DISPLAY: &str = "wayland-0";
pub(crate) const SANDBOX_WAYLAND_SOCKET: &str = "/run/merry-wayland/wayland-0";
pub(crate) const SANDBOX_X11_AUTHORITY: &str = "/run/merry-x11/Xauthority";

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
    #[value(name = "cli-bwrap-v1")]
    CliBwrapV1,
}

impl ChildHandoff {
    fn as_cli_value(self) -> &'static str {
        match self {
            Self::CliBwrapV1 => SANDBOX_CHILD_HANDOFF_CLI_BWRAP_V1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeProfile {
    CliBwrapV1,
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
        Bootstrap::Reexec(plan) => {
            ensure_host_managed_provider_directories(&host)?;
            ensure_host_state_directory(&host)?;
            exec(plan)
        }
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
    plan_bootstrap_with_probe(with_sandbox, clipboard_access, host, &FilesystemHostProbe)
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

    plan_bootstrap_with_probe(
        with_sandbox,
        ClipboardAccess::Disabled,
        host,
        &FileExistsProbe(file_exists),
    )
}

pub(crate) fn plan_bootstrap_with_probe(
    with_sandbox: bool,
    clipboard_access: ClipboardAccess,
    host: &Host,
    probe: &impl HostPathProbe,
) -> Result<Bootstrap, Error> {
    if !with_sandbox {
        return Ok(Bootstrap::Disabled);
    }

    if host.inside_sandbox {
        return Ok(Bootstrap::AlreadyInside);
    }

    let path = sandbox_path(host);
    let bwrap = find_bwrap_in_path(&path, |candidate| probe.file_exists(candidate))
        .ok_or(Error::MissingBubblewrap)?;
    ensure_host_log_directory(host)?;

    Ok(Bootstrap::Reexec(build_plan(
        host,
        path,
        bwrap,
        clipboard_access,
        probe,
    )))
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

fn build_plan(
    host: &Host,
    path: OsString,
    bwrap: PathBuf,
    clipboard_access: ClipboardAccess,
    probe: &impl HostPathProbe,
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
        os("--perms"),
        os("0700"),
    ];
    if !Path::new(&home).starts_with(Path::new(SANDBOX_HOME_ROOT)) {
        append_mount_parent_args(&mut args, home.as_os_str());
    }
    args.extend([os("--dir"), home.clone()]);
    append_bind_dir_try_args(&mut args, &config_dir, &config_dir);
    append_bind_dir_rw_args(&mut args, &managed_config_dir, &managed_config_dir);
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
    append_bind_dir_rw_args(&mut args, &state_dir, &state_dir);
    if let Some(log_settings) = host.log_settings.as_ref()
        && let Some(host_log_dir) = log_settings.path.parent()
    {
        args.extend([
            os("--bind"),
            host_log_dir.as_os_str().to_owned(),
            host_log_dir.as_os_str().to_owned(),
        ]);
    }
    for rule in &host.trusted_path_rules {
        append_path_rule_args(&mut args, rule);
    }
    for mount in &graphical_plan.mounts {
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
    if host.openai_debug.as_deref() == Some(OsStr::new("1")) {
        args.extend([os("--setenv"), os(MERRY_OPENAI_DEBUG_ENV), os("1")]);
    }
    args.extend([
        current_exe,
        os(SANDBOX_CHILD_HANDOFF_ARG),
        os(ChildHandoff::CliBwrapV1.as_cli_value()),
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
    if home.is_some_and(|path| is_clean_absolute_path(Path::new(path)))
        && tmpdir == Some(OsStr::new(SANDBOX_TMPDIR))
        && mountinfo_has_tmpfs_mounts(mountinfo?, [SANDBOX_HOME_ROOT, SANDBOX_TMPDIR])
    {
        Some(RuntimeProfile::CliBwrapV1)
    } else {
        None
    }
}

fn mountinfo_has_tmpfs_mounts(mountinfo: &str, mount_points: [&str; 2]) -> bool {
    mount_points
        .into_iter()
        .all(|mount_point| mountinfo_has_tmpfs_mount(mountinfo, mount_point))
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
            Error::Exec(error) => {
                write!(
                    formatter,
                    "failed to execute bubblewrap sandbox bootstrap: {error}"
                )
            }
        }
    }
}
