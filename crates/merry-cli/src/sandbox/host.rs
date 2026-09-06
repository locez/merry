//! Host evidence collection and runtime-profile admission; no process execution.

use crate::{
    config::{EffectiveLogSettings, MerryConfig, XdgPaths},
    provider_config::MERRY_OPENAI_DEBUG_ENV,
    sandbox::{
        ChildHandoff, Error, MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION, SANDBOX_HOME_ROOT,
        SANDBOX_TMPDIR,
        integrations::{GraphicalEnvironment, HostIntegrationEnvironment, is_clean_absolute_path},
        os,
    },
};
use merry_runtime::{AcceptedLocalWorkspaceProcessAdmission, HostIntegration, PathAccessRule};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileTypeExt, MetadataExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostPathKind {
    RegularFile,
    UnixSocket,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostPathMetadata {
    pub(super) kind: HostPathKind,
    pub(super) owner_uid: u32,
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

pub(super) struct FilesystemHostProbe;

impl HostPathProbe for FilesystemHostProbe {
    fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn metadata(&self, path: &Path) -> Option<HostPathMetadata> {
        filesystem_path_metadata(path)
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
    pub(super) fn from_env(args: Vec<OsString>) -> Result<Self, Error> {
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

pub(super) fn is_valid_runtime_home_path(path: &Path) -> bool {
    is_clean_absolute_path(path)
        && path != Path::new("/")
        && path != Path::new(SANDBOX_HOME_ROOT)
        && !path.starts_with(Path::new(SANDBOX_TMPDIR))
}

pub(super) fn mountinfo_has_tmpfs_mount(mountinfo: &str, mount_point: &str) -> bool {
    mountinfo
        .lines()
        .filter_map(parse_mountinfo_mount)
        .any(|mount| mount.mount_point == mount_point && mount.fs_type == "tmpfs")
}

pub(super) fn parse_mountinfo_mount(line: &str) -> Option<MountInfoMount<'_>> {
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
pub(super) struct MountInfoMount<'a> {
    pub(super) mount_point: &'a str,
    pub(super) fs_type: &'a str,
}

#[cfg(target_os = "linux")]
pub(super) fn filesystem_path_metadata(path: &Path) -> Option<HostPathMetadata> {
    let metadata = fs::metadata(path).ok()?;
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
pub(super) fn filesystem_path_metadata(_path: &Path) -> Option<HostPathMetadata> {
    None
}

#[cfg(target_os = "linux")]
pub(super) fn current_process_uid() -> Result<u32, Error> {
    fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .map_err(Error::CurrentUser)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn current_process_uid() -> Result<u32, Error> {
    Ok(0)
}
