use crate::{
    config,
    sandbox::{
        command::{build_plan, exec, sandbox_path},
        host::FilesystemHostProbe,
        paths::{
            SandboxPathPlan, ensure_host_log_directory, ensure_host_managed_provider_directories,
            ensure_host_state_directory, validate_outer_paths,
        },
    },
};
pub(crate) use command::find_bwrap_in_path;
pub(crate) use host::{
    Host, HostPathProbe, local_workspace_process_admission, read_proc_self_mountinfo,
    runtime_profile_from_evidence,
};
use merry_runtime::PathAccess;
pub(crate) use paths::default_inner_development_path_rules;
use std::{
    env,
    ffi::OsString,
    fmt, io,
    path::{Path, PathBuf},
};

#[cfg(test)]
use host::HostPathMetadata;

#[cfg(test)]
pub(crate) use host::RuntimeProfile;

mod command;

mod host;

mod integrations;

mod paths;

mod mounts;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
    pub(crate) env: Vec<(OsString, OsString)>,
    #[cfg(target_os = "linux")]
    pub(crate) ssh_config: merry_process::BwrapSshConfigFiles,
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
    let mut host = Host::from_env(args)?;
    if with_sandbox
        && !host.inside_sandbox
        && host
            .host_integrations
            .contains(&merry_runtime::HostIntegration::GpgAgent)
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(Error::AgentDiscoveryRuntime)?;
        host.host_integration_environment.gpg_agent_sockets = runtime
            .block_on(merry_process::GpgAgentSockets::discover(
                host.xdg_paths.home(),
                &[],
            ))
            .map_err(Error::AgentDiscovery)?;
        if let Some(sockets) = &host.host_integration_environment.gpg_agent_sockets {
            sockets
                .validate_public_key_access()
                .map_err(Error::AgentDiscovery)?;
        } else {
            return Err(Error::AgentDiscovery(
                merry_runtime::ProcessRunnerError::infrastructure(
                    "gpg_agent requires gpgconf in PATH for socket discovery",
                ),
            ));
        }
    }
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
    )?))
}

pub(crate) fn os(value: &str) -> OsString {
    OsString::from(value)
}

#[derive(Debug)]
pub(crate) enum Error {
    CurrentDir(io::Error),
    CurrentExe(io::Error),
    CurrentUser(io::Error),
    Config(config::ConfigError),
    AgentDiscovery(merry_runtime::ProcessRunnerError),
    AgentDiscoveryRuntime(io::Error),
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
    ConflictingTrustedPathRules {
        path: PathBuf,
        first_access: PathAccess,
        second_access: PathAccess,
    },
    MountPlan(mounts::MountPlanError),
    Exec(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::AgentDiscovery(error) => {
                write!(formatter, "GPG agent discovery failed: {error}")
            }
            Error::AgentDiscoveryRuntime(error) => {
                write!(formatter, "could not initialize GPG discovery: {error}")
            }
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
            Error::MountPlan(error) => error.fmt(formatter),
            Error::Exec(error) => {
                write!(
                    formatter,
                    "failed to execute bubblewrap sandbox bootstrap: {error}"
                )
            }
        }
    }
}
