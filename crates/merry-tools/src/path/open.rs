//! Resolution and opening of a validated tool path.
//!
//! Everything here touches the filesystem. A relative argument is walked below
//! each candidate root in order and a symlink component is refused, while an
//! absolute argument is used exactly as the caller named it. On Unix every open
//! also passes `O_NOFOLLOW`, so a symlink swapped into the leaf between
//! resolution and open cannot redirect the operation.

use std::{fs, io, path::Path, path::PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::errors::{
    DomainError, ERROR_FILE_ALREADY_EXISTS, ERROR_NOT_DIRECTORY, ERROR_PATH_DENIED,
    ERROR_READ_FAILED, ERROR_WRITE_FAILED,
};

use super::validate::ValidatedToolPath;

/// Resolves a validated tool path to the file it names, when that file exists.
///
/// `roots` are the anchors a relative argument may resolve under, in order. An
/// absolute argument names exactly one path, so it is resolved as the caller
/// wrote it and can never land below a different, same-named anchor.
///
/// Every component below an anchor is walked with `symlink_metadata`, so a
/// path inside the workspace cannot be silently redirected through a link. An
/// absolute path is resolved by the operating system, and the leaf stays
/// protected because every open in this crate uses `O_NOFOLLOW`.
pub(crate) fn resolve_existing_path<'a>(
    path: &'a ValidatedToolPath,
    roots: impl IntoIterator<Item = &'a PathBuf>,
) -> Result<Option<PathBuf>, DomainError> {
    for (anchor, components) in path.anchors(roots) {
        if let Some(resolved) = walk_existing_path(&anchor, components, path.denies_symlinks())? {
            return Ok(Some(resolved));
        }
    }

    Ok(None)
}

/// Walks `components` below `anchor` and returns the path when it exists.
fn walk_existing_path(
    anchor: &Path,
    components: &[String],
    deny_symlinks: bool,
) -> Result<Option<PathBuf>, DomainError> {
    let mut current = anchor.to_path_buf();
    for component in components {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(DomainError::new(
                    ERROR_READ_FAILED,
                    "could not inspect workspace path",
                ));
            }
        };

        if deny_symlinks && metadata.file_type().is_symlink() {
            return Err(DomainError::new(
                ERROR_PATH_DENIED,
                "workspace path uses a symlink",
            ));
        }
    }

    Ok(Some(current))
}

#[derive(Debug)]
pub(crate) enum NewWorkspacePath {
    /// The target path is free; parent directories may still need creating.
    Missing(PathBuf),
    /// The target path already exists, so creation must not proceed.
    Existing,
}

/// Resolves the file a validated tool path would create.
///
/// `roots` are the anchors a relative argument may resolve under, in order. An
/// absolute argument names exactly one path. A path that exists already reports
/// [`NewWorkspacePath::Existing`], and a path whose parent directories are
/// still missing reports [`NewWorkspacePath::Missing`] because the caller
/// creates those parents when it writes.
pub(crate) fn resolve_new_file_path<'a>(
    path: &'a ValidatedToolPath,
    roots: impl IntoIterator<Item = &'a PathBuf>,
) -> Result<NewWorkspacePath, DomainError> {
    for (anchor, components) in path.anchors(roots) {
        if let Some(outcome) = walk_new_path(&anchor, components, path.denies_symlinks())? {
            return Ok(outcome);
        }
    }

    Err(DomainError::new(
        ERROR_READ_FAILED,
        "could not resolve workspace file path",
    ))
}

/// Walks `components` below `anchor` to find whether a new file can be created.
fn walk_new_path(
    anchor: &Path,
    components: &[String],
    deny_symlinks: bool,
) -> Result<Option<NewWorkspacePath>, DomainError> {
    let last_index = components.len().saturating_sub(1);
    // A missing component does not change the target: the file to create is
    // still the last component, and the caller creates the missing parent
    // directories when it writes.
    let target = components
        .iter()
        .fold(anchor.to_path_buf(), |mut path, component| {
            path.push(component);
            path
        });
    let mut current = anchor.to_path_buf();

    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Some(NewWorkspacePath::Missing(target)));
            }
            Err(_) => {
                return Err(DomainError::new(
                    ERROR_READ_FAILED,
                    "could not inspect workspace path",
                ));
            }
        };

        if deny_symlinks && metadata.file_type().is_symlink() {
            return Err(DomainError::new(
                ERROR_PATH_DENIED,
                "workspace path uses a symlink",
            ));
        }
        if index < last_index && !metadata.is_dir() {
            return Err(DomainError::new(
                ERROR_NOT_DIRECTORY,
                "workspace path parent is not a directory",
            ));
        }
        if index == last_index {
            return Ok(Some(NewWorkspacePath::Existing));
        }
    }

    Ok(None)
}

/// Opens an existing file for reading.
pub(crate) fn open_file_for_read(path: &Path) -> Result<fs::File, DomainError> {
    open_file_for_read_impl(path).map_err(|error| {
        if is_symlink_open_error(&error) {
            DomainError::new(ERROR_PATH_DENIED, "workspace path uses a symlink")
        } else {
            DomainError::new(ERROR_READ_FAILED, "could not open workspace file")
        }
    })
}

/// Opens an existing file for an in-place patch.
pub(crate) fn open_file_for_patch(path: &Path) -> Result<fs::File, DomainError> {
    open_file_for_patch_impl(path).map_err(|error| {
        if is_symlink_open_error(&error) {
            DomainError::new(ERROR_PATH_DENIED, "workspace path uses a symlink")
        } else {
            DomainError::new(
                ERROR_WRITE_FAILED,
                "could not open workspace file for patching",
            )
        }
    })
}

/// Creates a patch target, creating its missing parent directories first.
pub(crate) fn open_file_for_patch_create_new(path: &Path) -> Result<fs::File, DomainError> {
    create_patch_parent_directories(path)?;
    open_file_for_patch_create_new_impl(path).map_err(|error| {
        if is_symlink_open_error(&error) {
            DomainError::new(ERROR_PATH_DENIED, "workspace path uses a symlink")
        } else if error.kind() == io::ErrorKind::AlreadyExists {
            DomainError::new(ERROR_FILE_ALREADY_EXISTS, "workspace file already exists")
        } else {
            DomainError::new(ERROR_WRITE_FAILED, "could not create workspace file")
        }
    })
}

/// Creates the parent directories of a patch target, one component at a time.
///
/// Each component is created explicitly and inspected before the next one, so a
/// symlink cannot be used to make `create_new` write outside the intended
/// directory tree.
fn create_patch_parent_directories(path: &Path) -> Result<(), DomainError> {
    let parent = path.parent().ok_or_else(|| {
        DomainError::new(
            ERROR_WRITE_FAILED,
            "could not determine workspace file parent directory",
        )
    })?;
    let mut current = PathBuf::new();

    for component in parent.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => validate_patch_parent_metadata(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let metadata = fs::symlink_metadata(&current).map_err(|_| {
                            DomainError::new(
                                ERROR_WRITE_FAILED,
                                "could not inspect workspace file parent directory",
                            )
                        })?;
                        validate_patch_parent_metadata(&metadata)?;
                    }
                    Err(_) => {
                        return Err(DomainError::new(
                            ERROR_WRITE_FAILED,
                            "could not create workspace file parent directories",
                        ));
                    }
                }
            }
            Err(_) => {
                return Err(DomainError::new(
                    ERROR_WRITE_FAILED,
                    "could not inspect workspace file parent directory",
                ));
            }
        }
    }

    Ok(())
}

fn validate_patch_parent_metadata(metadata: &fs::Metadata) -> Result<(), DomainError> {
    if metadata.file_type().is_symlink() {
        return Err(DomainError::new(
            ERROR_PATH_DENIED,
            "workspace path uses a symlink",
        ));
    }
    if !metadata.is_dir() {
        return Err(DomainError::new(
            ERROR_NOT_DIRECTORY,
            "workspace file parent is not a directory",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_file_for_read_impl(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options.open(path)
}

#[cfg(not(unix))]
fn open_file_for_read_impl(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(unix)]
fn open_file_for_patch_impl(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(false)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}

#[cfg(not(unix))]
fn open_file_for_patch_impl(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .truncate(false)
        .open(path)
}

#[cfg(unix)]
fn open_file_for_patch_create_new_impl(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}

#[cfg(not(unix))]
fn open_file_for_patch_create_new_impl(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn is_symlink_open_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_open_error(_: &io::Error) -> bool {
    false
}
