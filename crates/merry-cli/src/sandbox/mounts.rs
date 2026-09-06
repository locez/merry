//! Compile outer-sandbox mounts independently of command-line argument ordering.
//!
//! Origin order preserves admission precedence before mounts are emitted from
//! parent to child. Destination links are interpreted in the planned sandbox,
//! never by blindly canonicalizing a path in the host namespace.

mod destination;

use crate::sandbox::os;
use merry_process::resolve_bwrap_path;
use merry_runtime::{PathAccess, PathAccessRule};
use std::{
    ffi::OsString,
    io,
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
    #[error("failed to inspect sandbox mount destination through {path}: {source}")]
    DestinationIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sandbox mount destination has a cyclic or excessively deep symlink chain: {path}")]
    DestinationLoop { path: PathBuf },
}

struct Mount {
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
        let source = resolve_bwrap_path(source);
        let directory = source.is_dir();
        let exists = source.exists();
        Self {
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

    pub(super) fn append_args(mut self, args: &mut Vec<OsString>) -> Result<(), MountPlanError> {
        self.mounts.sort_by_key(|mount| mount.origin);
        destination::resolve_all(&mut self.mounts)?;
        let access = self
            .mounts
            .iter()
            .enumerate()
            .map(|(index, mount)| self.effective_access(index, mount))
            .collect::<Vec<_>>();
        let mut mounts = self
            .mounts
            .into_iter()
            .zip(access)
            .filter_map(|(mut mount, access)| {
                mount.access = access?;
                Some(mount)
            })
            .collect::<Vec<_>>();
        mounts.sort_by_key(|mount| mount.destination.components().count());
        for mount in mounts {
            append_mount_parent_args(args, &mount.destination);
            let flag = match (mount.access, mount.optional) {
                (PathAccess::Deny, _) => {
                    args.extend([os("--tmpfs"), mount.destination.into_os_string()]);
                    continue;
                }
                (PathAccess::ReadOnly, false) => "--ro-bind",
                (PathAccess::ReadOnly, true) => "--ro-bind-try",
                (PathAccess::ReadWrite, false) => "--bind",
                (PathAccess::ReadWrite, true) => "--bind-try",
            };
            args.extend([
                os(flag),
                mount.source.into_os_string(),
                mount.destination.into_os_string(),
            ]);
        }
        Ok(())
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
