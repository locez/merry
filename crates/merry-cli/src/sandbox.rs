use crate::config::{self, EffectiveLogSettings, MerryConfig, XdgPaths};
use crate::provider_config::MERRY_OPENAI_DEBUG_ENV;
use merry_runtime::{PathAccess, PathAccessRule};
use std::{
    env,
    ffi::{OsStr, OsString},
    fmt, fs, io,
    path::{Path, PathBuf},
};

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
pub(crate) const SANDBOX_HOME: &str = "/home/merry";
pub(crate) const SANDBOX_TMPDIR: &str = "/tmp";
// These are sandbox-child paths. Host paths are resolved separately before
// re-exec; inside bwrap, HOME is intentionally set to SANDBOX_HOME.
pub(crate) const SANDBOX_XDG_CONFIG_HOME: &str = "/home/merry/.config";
pub(crate) const SANDBOX_XDG_STATE_HOME: &str = "/home/merry/.local/state";
pub(crate) const SANDBOX_MERRY_CONFIG_DIR: &str = "/home/merry/.config/merry";
pub(crate) const SANDBOX_MERRY_LOG_DIR: &str = "/home/merry/.local/state/merry/logs";

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

pub(crate) fn maybe_reexec(with_sandbox: bool, args: Vec<OsString>) -> Result<(), Error> {
    let host = Host::from_env(args)?;
    match plan_bootstrap(with_sandbox, &host)? {
        Bootstrap::Disabled | Bootstrap::AlreadyInside => Ok(()),
        Bootstrap::Reexec(plan) => exec(plan),
    }
}

pub(crate) fn plan_bootstrap(with_sandbox: bool, host: &Host) -> Result<Bootstrap, Error> {
    plan_bootstrap_with_file_exists(with_sandbox, host, Path::exists)
}

pub(crate) fn plan_bootstrap_with_file_exists(
    with_sandbox: bool,
    host: &Host,
    file_exists: impl Fn(&Path) -> bool,
) -> Result<Bootstrap, Error> {
    if !with_sandbox {
        return Ok(Bootstrap::Disabled);
    }

    if host.inside_sandbox {
        return Ok(Bootstrap::AlreadyInside);
    }

    let path = sandbox_path(host);
    let bwrap = find_bwrap_in_path(&path, file_exists).ok_or(Error::MissingBubblewrap)?;
    ensure_host_log_directory(host)?;

    Ok(Bootstrap::Reexec(build_plan(host, path, bwrap)))
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

fn build_plan(host: &Host, path: OsString, bwrap: PathBuf) -> Plan {
    let cwd = host.cwd.as_os_str().to_owned();
    let current_exe = host.current_exe.as_os_str().to_owned();

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
        os("/home"),
        os("--perms"),
        os("0700"),
        os("--dir"),
        os(SANDBOX_HOME),
        os("--ro-bind-try"),
        host.xdg_paths.config_dir().as_os_str().to_owned(),
        os(SANDBOX_MERRY_CONFIG_DIR),
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
    ];
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
            os(SANDBOX_MERRY_LOG_DIR),
        ]);
    }
    for rule in &host.trusted_path_rules {
        append_path_rule_args(&mut args, rule);
    }
    args.extend([
        os("--clearenv"),
        os("--setenv"),
        os("PATH"),
        path.clone(),
        os("--setenv"),
        os("HOME"),
        os(SANDBOX_HOME),
        os("--setenv"),
        os("TMPDIR"),
        os(SANDBOX_TMPDIR),
        os("--setenv"),
        os("XDG_CONFIG_HOME"),
        os(SANDBOX_XDG_CONFIG_HOME),
        os("--setenv"),
        os("XDG_STATE_HOME"),
        os(SANDBOX_XDG_STATE_HOME),
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
    if home == Some(OsStr::new(SANDBOX_HOME))
        && tmpdir == Some(OsStr::new(SANDBOX_TMPDIR))
        && mountinfo_has_tmpfs_mounts(mountinfo?, ["/home", SANDBOX_TMPDIR])
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

#[derive(Debug)]
pub(crate) enum Error {
    CurrentDir(io::Error),
    CurrentExe(io::Error),
    Config(config::ConfigError),
    LogDirectory {
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
            Error::Config(error) => write!(
                formatter,
                "failed to load Merry config before sandbox bootstrap: {error}"
            ),
            Error::LogDirectory { path, source } => write!(
                formatter,
                "failed to create host log directory {} before sandbox bootstrap: {source}",
                path.display()
            ),
            #[cfg(not(target_os = "linux"))]
            Error::UnsupportedPlatform => write!(
                formatter,
                "merry --with-sandbox is only supported on Linux with bubblewrap (bwrap)"
            ),
            Error::MissingBubblewrap => write!(
                formatter,
                "bubblewrap executable `bwrap` was not found in PATH; install bubblewrap or run without --with-sandbox"
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
