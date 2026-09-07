//! Resolve links introduced by planned directory imports, not by unmapped host paths.

use super::{Mount, MountPlanError};
use merry_runtime::PathAccess;
use std::path::{Path, PathBuf};

/// Project directory aliases before resolving descendants through their imported trees.
pub(super) fn resolve_all(mounts: &mut [Mount]) -> Result<(), MountPlanError> {
    let mut remaining = mounts.len();
    loop {
        let destinations = mounts
            .iter()
            .enumerate()
            .map(|(index, mount)| resolve(&mount.logical_destination, Some(index), mounts))
            .collect::<Result<Vec<_>, _>>()?;
        let Some((unresolved, _)) = mounts
            .iter()
            .zip(&destinations)
            .find(|(mount, destination)| mount.destination != **destination)
        else {
            return Ok(());
        };
        if remaining == 0 {
            return Err(MountPlanError::DestinationLoop {
                path: unresolved.logical_destination.clone(),
            });
        }
        for (mount, destination) in mounts.iter_mut().zip(destinations) {
            mount.destination = destination;
        }
        remaining -= 1;
    }
}

pub(super) fn resolve(
    path: &Path,
    current_mount: Option<usize>,
    mounts: &[Mount],
) -> Result<PathBuf, MountPlanError> {
    merry_process::resolve_sandbox_path(path, |prefix| imported_path(prefix, current_mount, mounts))
        .map_err(|error| match error {
            merry_process::SandboxPathError::Io { path, source } => {
                MountPlanError::DestinationIo { path, source }
            }
            merry_process::SandboxPathError::Loop { path } => {
                MountPlanError::DestinationLoop { path }
            }
        })
}

fn imported_path(path: &Path, current_mount: Option<usize>, mounts: &[Mount]) -> Option<PathBuf> {
    let (_, mount) = mounts
        .iter()
        .enumerate()
        .filter(|(index, mount)| {
            Some(*index) != current_mount
                && current_mount.is_none_or(|current| {
                    mount.logical_destination != mounts[current].logical_destination
                })
                && mount.active()
                && path != mount.destination
                && path.starts_with(&mount.destination)
        })
        .max_by_key(|(index, mount)| (mount.destination.components().count(), *index))?;
    if mount.access == PathAccess::Deny || !mount.directory {
        return None;
    }
    let relative = path.strip_prefix(&mount.destination).ok()?;
    Some(mount.source.join(relative))
}
