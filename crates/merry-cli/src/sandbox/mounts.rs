//! Compile outer-sandbox mounts independently of command-line argument ordering.
//!
//! Origin order preserves admission precedence before mounts are emitted from
//! parent to child. Destination links are interpreted in the planned sandbox,
//! never by blindly canonicalizing a path in the host namespace.

use crate::sandbox::os;
use merry_process::resolve_bwrap_path;
use merry_runtime::{PathAccess, PathAccessRule};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Product writes override broad trusted rules but not narrower product restrictions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum MountOrigin {
    System,
    Workspace,
    Development,
    Trusted,
    Product,
    ProductRestriction,
    Integration,
}

impl MountOrigin {
    fn is_trusted(self) -> bool {
        matches!(self, Self::Trusted | Self::ProductRestriction)
    }
}

#[derive(Debug, Error)]
pub(crate) enum MountPlanError {
    #[error("cannot prepare sandbox mounts: {0}")]
    Shared(#[from] merry_process::SandboxMountError),
    #[cfg(target_os = "linux")]
    #[error("cannot prepare SSH configuration for sandbox: {0}")]
    SshConfig(#[source] merry_runtime::ProcessRunnerError),
}

struct Mount {
    original_source: PathBuf,
    source: PathBuf,
    logical_destination: PathBuf,
    destination: PathBuf,
    access: PathAccess,
    origin: MountOrigin,
    optional: bool,
    directory: bool,
    exists: bool,
}

impl Mount {
    fn new(
        source: &Path,
        destination: &Path,
        access: PathAccess,
        optional: bool,
        origin: MountOrigin,
    ) -> Self {
        let original_source = source.to_path_buf();
        let source = resolve_bwrap_path(source);
        let directory = source.is_dir();
        let exists = source.exists();
        Self {
            original_source,
            source,
            logical_destination: destination.to_path_buf(),
            destination: destination.to_path_buf(),
            access,
            origin,
            optional,
            directory,
            exists,
        }
    }

    fn active(&self) -> bool {
        !self.optional || self.exists
    }

    fn denies(&self, mount: &Self) -> bool {
        self.access == PathAccess::Deny
            && self.origin.is_trusted()
            && [
                &mount.logical_destination,
                &mount.destination,
                &mount.source,
            ]
            .into_iter()
            .any(|path| {
                path.starts_with(&self.logical_destination)
                    || path.starts_with(&self.destination)
                    || path.starts_with(&self.source)
            })
    }
}

#[derive(Default)]
pub(super) struct MountPlan {
    mounts: Vec<Mount>,
}

impl MountPlan {
    pub(super) fn bind(
        &mut self,
        source: &Path,
        destination: &Path,
        access: PathAccess,
        optional: bool,
        origin: MountOrigin,
    ) {
        self.mounts
            .push(Mount::new(source, destination, access, optional, origin));
    }

    pub(super) fn rule(&mut self, rule: &PathAccessRule, origin: MountOrigin) {
        self.bind(
            rule.path(),
            rule.path(),
            rule.access(),
            rule.access() != PathAccess::Deny,
            origin,
        );
    }

    #[cfg(any(test, not(target_os = "linux")))]
    pub(super) fn append_args(mut self, args: &mut Vec<OsString>) -> Result<(), MountPlanError> {
        self.append_resolved_args(args, false).map(|_| ())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn append_args_with_ssh_config(
        mut self,
        args: &mut Vec<OsString>,
    ) -> Result<merry_process::BwrapSshConfigFiles, MountPlanError> {
        self.append_resolved_args(args, true)
    }

    fn append_resolved_args(
        &mut self,
        args: &mut Vec<OsString>,
        prepare_ssh: bool,
    ) -> Result<SshConfigFiles, MountPlanError> {
        self.mounts.sort_by_key(|mount| mount.origin);
        let destinations = namespace_inputs(&self.mounts)?.resolved_destinations()?;
        for (mount, destination) in self.mounts.iter_mut().zip(destinations) {
            mount.destination = destination;
        }
        let access = self
            .mounts
            .iter()
            .enumerate()
            .map(|(index, mount)| self.effective_access(index, mount))
            .collect::<Vec<_>>();
        let mut mounts = std::mem::take(&mut self.mounts)
            .into_iter()
            .zip(access)
            .filter_map(|(mut mount, access)| {
                mount.access = access?;
                Some(mount)
            })
            .collect::<Vec<_>>();
        mounts.sort_by_key(|mount| mount.destination.components().count());
        let namespace = namespace_inputs(&mounts)?;
        let scan_roots = mounts
            .iter()
            .filter(|mount| {
                mount.directory
                    && mount.access != PathAccess::Deny
                    && matches!(
                        mount.origin,
                        MountOrigin::Trusted
                            | MountOrigin::ProductRestriction
                            | MountOrigin::Integration
                    )
            })
            .map(|mount| mount.logical_destination.clone())
            .collect::<Vec<_>>();
        let namespace = namespace.complete(&scan_roots)?;
        for issue in namespace.issues() {
            tracing::debug!(
                ?issue,
                "sandbox symlink dependency retains its restricted or unavailable representation"
            );
        }
        #[cfg(target_os = "linux")]
        let ssh_config = if prepare_ssh {
            merry_process::BwrapSshConfigFiles::prepare(Path::new("/etc/ssh/ssh_config"), |path| {
                namespace.resolve(path).map_err(|error| {
                    merry_runtime::ProcessRunnerError::infrastructure(error.to_string())
                })
            })
            .map_err(MountPlanError::SshConfig)?
        } else {
            merry_process::BwrapSshConfigFiles::default()
        };
        #[cfg(not(target_os = "linux"))]
        let ssh_config = {
            let _ = prepare_ssh;
        };
        #[cfg(target_os = "linux")]
        namespace.append_args(args, |path| ssh_config.replaces_file(path))?;
        #[cfg(not(target_os = "linux"))]
        namespace.append_args(args, |_| false)?;
        Ok(ssh_config)
    }

    fn effective_access(&self, index: usize, mount: &Mount) -> Option<PathAccess> {
        if mount.access == PathAccess::Deny {
            return Some(PathAccess::Deny);
        }
        if self.mounts.iter().any(|other| other.denies(mount)) {
            return None;
        }
        let mut access = mount.access;
        for other in &self.mounts[index + 1..] {
            if !other.active() {
                continue;
            }
            if other.logical_destination == mount.logical_destination
                || other.destination == mount.destination
            {
                return None;
            }
            if mount
                .logical_destination
                .starts_with(&other.logical_destination)
                || mount.destination.starts_with(&other.destination)
            {
                access = other.access;
            }
            if other.origin.is_trusted()
                && other.access == PathAccess::ReadOnly
                && mount.source.starts_with(&other.source)
            {
                access = PathAccess::ReadOnly;
            }
        }
        Some(access)
    }
}

#[cfg(target_os = "linux")]
type SshConfigFiles = merry_process::BwrapSshConfigFiles;
#[cfg(not(target_os = "linux"))]
type SshConfigFiles = ();

fn namespace_inputs(mounts: &[Mount]) -> Result<merry_process::SandboxMountPlan, MountPlanError> {
    let mut inputs = merry_process::SandboxMountPlan::new();
    inputs.opaque(Path::new("/proc"))?;
    inputs.opaque(Path::new("/dev"))?;
    for mount in mounts {
        inputs.bind(
            &mount.original_source,
            &mount.logical_destination,
            mount.access,
            mount.optional,
        )?;
    }
    Ok(inputs)
}

pub(super) fn append_mount_parent_args(args: &mut Vec<OsString>, destination: &Path) {
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
