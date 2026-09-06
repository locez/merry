//! Resolve links introduced by planned directory imports, not by unmapped host paths.

use super::{Mount, MountPlanError};
use merry_runtime::PathAccess;
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Project directory aliases before resolving descendants through their imported trees.
pub(super) fn resolve_all(mounts: &mut [Mount]) -> Result<(), MountPlanError> {
    let mut remaining = mounts.len();
    loop {
        let destinations = mounts
            .iter()
            .enumerate()
            .map(|(index, mount)| resolve(&mount.logical_destination, index, mounts))
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

fn resolve(path: &Path, current_mount: usize, mounts: &[Mount]) -> Result<PathBuf, MountPlanError> {
    let mut destination = path.to_path_buf();
    let mut visited = BTreeSet::new();
    for _ in 0..=40 {
        if !visited.insert(destination.clone()) {
            break;
        }
        let mut prefix = PathBuf::new();
        let mut components = destination.components();
        let mut next = None;
        while let Some(component) = components.next() {
            prefix.push(component);
            let Some(host_path) = imported_path(&prefix, current_mount, mounts) else {
                continue;
            };
            let metadata = match fs::symlink_metadata(&host_path) {
                Ok(metadata) => metadata,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(source) => {
                    return Err(MountPlanError::DestinationIo {
                        path: host_path,
                        source,
                    });
                }
            };
            if metadata.file_type().is_symlink() {
                let target =
                    fs::read_link(&host_path).map_err(|source| MountPlanError::DestinationIo {
                        path: host_path,
                        source,
                    })?;
                let parent = prefix.parent().unwrap_or(Path::new("/"));
                next = Some(normalize(&parent.join(target).join(components.as_path())));
                break;
            }
        }
        match next {
            Some(next) => destination = next,
            None => return Ok(destination),
        }
    }
    Err(MountPlanError::DestinationLoop {
        path: path.to_path_buf(),
    })
}

fn imported_path(path: &Path, current_mount: usize, mounts: &[Mount]) -> Option<PathBuf> {
    let (_, mount) = mounts
        .iter()
        .enumerate()
        .filter(|(index, mount)| {
            *index != current_mount
                && mount.logical_destination != mounts[current_mount].logical_destination
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

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized != Path::new("/") {
                    normalized.pop();
                }
            }
            component => normalized.push(component),
        }
    }
    normalized
}
