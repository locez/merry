//! Validated host display and clipboard endpoints for the sandbox boundary.

use crate::sandbox::{
    SANDBOX_WAYLAND_DISPLAY, SANDBOX_WAYLAND_RUNTIME_DIR, SANDBOX_WAYLAND_SOCKET,
    SANDBOX_X11_AUTHORITY,
    host::{Host, HostPathKind, HostPathProbe},
    os,
};
use merry_runtime::HostIntegration;
use std::{
    collections::BTreeSet,
    env,
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GraphicalEnvironment {
    pub(super) xdg_runtime_dir: Option<PathBuf>,
    pub(super) wayland_display: Option<OsString>,
    pub(super) display: Option<OsString>,
    pub(super) xauthority: Option<PathBuf>,
    pub(super) home: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HostIntegrationEnvironment {
    pub(super) ssh_agent_socket: Option<PathBuf>,
    pub(super) session_bus_address: Option<OsString>,
    pub(super) gpg_agent_sockets: Option<merry_process::GpgAgentSockets>,
}

impl HostIntegrationEnvironment {
    pub(super) fn from_env() -> Self {
        Self {
            ssh_agent_socket: env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            session_bus_address: env::var_os("DBUS_SESSION_BUS_ADDRESS"),
            gpg_agent_sockets: None,
        }
    }
}

impl GraphicalEnvironment {
    pub(super) fn from_env() -> Self {
        Self {
            xdg_runtime_dir: env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            wayland_display: env::var_os("WAYLAND_DISPLAY"),
            display: env::var_os("DISPLAY"),
            xauthority: env::var_os("XAUTHORITY").map(PathBuf::from),
            home: env::var_os("HOME").map(PathBuf::from),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct GraphicalAccessPlan {
    pub(super) mounts: Vec<GraphicalMount>,
    pub(super) environment: Vec<(OsString, OsString)>,
    pub(super) private_directories: BTreeSet<PathBuf>,
}

#[derive(Debug)]
pub(super) struct GraphicalMount {
    pub(super) source: PathBuf,
    pub(super) destination: PathBuf,
}

pub(super) fn graphical_access_plan(
    host: &Host,
    probe: &impl HostPathProbe,
) -> GraphicalAccessPlan {
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

pub(super) fn host_integration_access_plan(
    host: &Host,
    probe: &impl HostPathProbe,
) -> GraphicalAccessPlan {
    let mut plan = GraphicalAccessPlan::default();

    for integration in &host.host_integrations {
        match integration {
            HostIntegration::SshAgent => {
                for (path, kind) in [
                    ("/etc/passwd", HostPathKind::RegularFile),
                    ("/etc/group", HostPathKind::RegularFile),
                    ("/etc/ssh/ssh_config", HostPathKind::RegularFile),
                    ("/etc/ssh/ssh_config.d", HostPathKind::Directory),
                ] {
                    let path = Path::new(path);
                    if probe
                        .metadata(path)
                        .is_some_and(|metadata| metadata.kind() == kind)
                    {
                        plan.mounts.push(GraphicalMount {
                            source: path.to_path_buf(),
                            destination: path.to_path_buf(),
                        });
                    }
                }
                for path in merry_process::ssh_known_hosts(host.xdg_paths.home()) {
                    if probe
                        .metadata(&path)
                        .is_some_and(|metadata| metadata.kind() == HostPathKind::RegularFile)
                    {
                        plan.mounts.push(GraphicalMount {
                            source: path.clone(),
                            destination: path,
                        });
                    }
                }
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
            HostIntegration::GpgAgent => {
                let Some(sockets) = &host.host_integration_environment.gpg_agent_sockets else {
                    continue;
                };
                plan.private_directories
                    .insert(sockets.home().to_path_buf());
                plan.environment
                    .push((os("GNUPGHOME"), sockets.home().as_os_str().to_owned()));
                for path in sockets.public_keyrings() {
                    if probe
                        .metadata(&path)
                        .is_some_and(|metadata| metadata.kind() == HostPathKind::RegularFile)
                    {
                        plan.mounts.push(GraphicalMount {
                            source: path.clone(),
                            destination: path,
                        });
                    }
                }
                if !host_owned_socket(host, probe, sockets.agent()) {
                    continue;
                }
                for directory in sockets.agent().ancestors().skip(1) {
                    if directory != Path::new("/")
                        && probe.metadata(directory).is_some_and(|metadata| {
                            metadata.kind() == HostPathKind::Directory
                                && metadata.owner_uid() == host.current_uid
                                && metadata.mode() & 0o777 == 0o700
                        })
                    {
                        plan.private_directories.insert(directory.to_path_buf());
                    }
                }
                plan.mounts.push(GraphicalMount {
                    source: sockets.agent().to_path_buf(),
                    destination: sockets.agent().to_path_buf(),
                });
            }
        }
    }

    plan
}

pub(super) fn host_owned_socket(host: &Host, probe: &impl HostPathProbe, path: &Path) -> bool {
    probe
        .metadata(path)
        .is_some_and(|metadata| metadata.is_owned_socket(host.current_uid))
}

pub(super) fn session_bus_socket_path(address: &OsStr) -> Option<PathBuf> {
    let address = address.to_str()?;
    address.split(';').find_map(|candidate| {
        let options = candidate.strip_prefix("unix:")?;
        options.split(',').find_map(|option| {
            let (name, value) = option.split_once('=')?;
            (name == "path").then(|| PathBuf::from(value))
        })
    })
}

pub(super) fn wayland_socket_path(host: &Host, probe: &impl HostPathProbe) -> Option<PathBuf> {
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
    metadata.is_owned_socket(host.current_uid).then_some(socket)
}

pub(super) fn x11_connection(
    host: &Host,
    probe: &impl HostPathProbe,
) -> Option<(String, PathBuf, PathBuf)> {
    let display = parse_local_x11_display(host.graphical_environment.display.as_deref()?)?;
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{}", display.number));
    if probe.metadata(&socket)?.kind() != HostPathKind::UnixSocket {
        return None;
    }

    let authority = x11_authority_path(&host.graphical_environment)?;
    let metadata = probe.metadata(&authority)?;
    if metadata.kind() != HostPathKind::RegularFile || metadata.owner_uid() != host.current_uid {
        return None;
    }

    Some((display.normalized, socket, authority))
}

pub(super) fn x11_authority_path(environment: &GraphicalEnvironment) -> Option<PathBuf> {
    let path = match environment.xauthority.as_ref() {
        Some(path) => path.clone(),
        None => environment.home.as_ref()?.join(".Xauthority"),
    };
    is_clean_absolute_path(&path).then_some(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalX11Display {
    pub(super) number: u32,
    pub(super) normalized: String,
}

pub(super) fn parse_local_x11_display(value: &OsStr) -> Option<LocalX11Display> {
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

pub(super) fn is_clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}
