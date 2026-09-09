//! Symlink resolution through a planned namespace, independent of admission policy.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

/// Failure to resolve a destination through admitted directory imports.
#[derive(Debug, Error)]
pub enum SandboxPathError {
    /// A filesystem inspection failed at the source corresponding to a destination.
    #[error("failed to inspect sandbox mount destination through {path}: {source}")]
    Io {
        /// Inspected source path, not file contents.
        path: PathBuf,
        /// Original inspection error.
        #[source]
        source: io::Error,
    },
    /// The namespace contains a cycle or more than forty symlink expansions.
    #[error("sandbox mount destination has a cyclic or excessively deep symlink chain: {path}")]
    Loop {
        /// Requested destination.
        path: PathBuf,
    },
}

/// Resolves links using only the source mapping supplied for each path prefix.
/// `None` means the prefix is not imported or must not be inspected. This never
/// admits additional paths, creates mount points, or canonicalizes unmapped host paths.
pub fn resolve_sandbox_path(
    path: &Path,
    source_for: impl Fn(&Path) -> Option<PathBuf>,
) -> Result<PathBuf, SandboxPathError> {
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
            match component {
                Component::CurDir => continue,
                Component::ParentDir => {
                    if prefix != Path::new("/") {
                        prefix.pop();
                    }
                    continue;
                }
                component => prefix.push(component),
            }
            let Some(host_path) = source_for(&prefix) else {
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
                    return Err(SandboxPathError::Io {
                        path: host_path,
                        source,
                    });
                }
            };
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&host_path).map_err(|source| SandboxPathError::Io {
                    path: host_path,
                    source,
                })?;
                let parent = prefix.parent().unwrap_or(Path::new("/"));
                next = Some(parent.join(target).join(components.as_path()));
                break;
            }
        }
        match next {
            Some(next) => destination = next,
            None => return Ok(prefix),
        }
    }
    Err(SandboxPathError::Loop {
        path: path.to_path_buf(),
    })
}
