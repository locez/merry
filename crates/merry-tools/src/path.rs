use std::{
    borrow::Cow,
    fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::errors::{
    DomainError, ERROR_FILE_ALREADY_EXISTS, ERROR_NOT_DIRECTORY, ERROR_PATH_DENIED,
    ERROR_READ_FAILED, ERROR_WRITE_FAILED, PathValidationError,
};

#[derive(Debug)]
pub(crate) struct ResolvedWorkspacePath {
    pub(crate) path: PathBuf,
}

pub(crate) fn resolve_existing_path(
    root: &Path,
    relative: &ValidatedRelativePath,
) -> Result<Option<ResolvedWorkspacePath>, DomainError> {
    let mut current = root.to_path_buf();
    for component in &relative.components {
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

        if metadata.file_type().is_symlink() {
            return Err(DomainError::new(
                ERROR_PATH_DENIED,
                "workspace path uses a symlink",
            ));
        }
    }

    let canonical = fs::canonicalize(&current).map_err(|_| {
        DomainError::new(ERROR_READ_FAILED, "could not canonicalize workspace path")
    })?;
    if !canonical.starts_with(root) {
        return Err(DomainError::new(
            ERROR_PATH_DENIED,
            "workspace path resolves outside a configured root",
        ));
    }

    Ok(Some(ResolvedWorkspacePath { path: current }))
}

#[derive(Debug)]
pub(crate) enum NewWorkspacePath {
    Missing(PathBuf),
    Existing,
    ParentMissing,
}

pub(crate) fn resolve_new_file_path(
    root: &Path,
    relative: &ValidatedRelativePath,
) -> Result<NewWorkspacePath, DomainError> {
    let last_index = relative.components.len().saturating_sub(1);
    let mut current = root.to_path_buf();

    for (index, component) in relative.components.iter().enumerate() {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if index == last_index {
                    let parent = current.parent().ok_or_else(|| {
                        DomainError::new(
                            ERROR_READ_FAILED,
                            "could not inspect workspace file parent",
                        )
                    })?;
                    let canonical_parent = fs::canonicalize(parent).map_err(|_| {
                        DomainError::new(
                            ERROR_READ_FAILED,
                            "could not canonicalize workspace file parent",
                        )
                    })?;
                    if !canonical_parent.starts_with(root) {
                        return Err(DomainError::new(
                            ERROR_PATH_DENIED,
                            "workspace path resolves outside a configured root",
                        ));
                    }
                    return Ok(NewWorkspacePath::Missing(current));
                }
                return Ok(NewWorkspacePath::ParentMissing);
            }
            Err(_) => {
                return Err(DomainError::new(
                    ERROR_READ_FAILED,
                    "could not inspect workspace path",
                ));
            }
        };

        if metadata.file_type().is_symlink() {
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
            return Ok(NewWorkspacePath::Existing);
        }
    }

    Err(DomainError::new(
        ERROR_READ_FAILED,
        "could not resolve workspace file path",
    ))
}

pub(crate) fn open_file_for_read(path: &Path) -> Result<fs::File, DomainError> {
    open_file_for_read_impl(path).map_err(|error| {
        if is_symlink_open_error(&error) {
            DomainError::new(ERROR_PATH_DENIED, "workspace path uses a symlink")
        } else {
            DomainError::new(ERROR_READ_FAILED, "could not open workspace file")
        }
    })
}

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
fn is_symlink_open_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_open_error(_: &io::Error) -> bool {
    false
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedRelativePath {
    pub(crate) components: Vec<String>,
    pub(crate) display: String,
}

/// Validates a workspace tool path and normalizes it to workspace-relative
/// components.
///
/// A caller may name the target either relative to a workspace root or with an
/// absolute path inside one of `roots`. Both address the same file, and a
/// caller that copied an absolute path out of process output or a tool result
/// should not have to translate it first. `.` segments are redundant spelling
/// and are dropped, `..` is resolved against the segments before it, and a path
/// that escapes every root is denied, so accepting another spelling never
/// widens what may be read or written. The resulting components go through the
/// same hidden-path, write-scope, forbidden-path, symlink, and
/// canonical-containment checks as any other path.
///
/// Failures report the workspace-relative form of the argument. A host absolute
/// path is never echoed, because failure text is provider-visible and a root
/// path is host detail the model does not need back.
pub(crate) fn validate_workspace_path_argument<I, P>(
    raw_path: &str,
    allow_hidden: bool,
    roots: I,
) -> Result<ValidatedRelativePath, PathValidationError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    if raw_path.is_empty() {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must not be empty",
            None,
        ));
    }

    if raw_path.chars().any(char::is_control) {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must not contain control characters",
            None,
        ));
    }

    let relative_text = match absolute_path_within_root(raw_path, roots)? {
        Some(relative_text) => Cow::Owned(relative_text),
        None => Cow::Borrowed(raw_path),
    };
    let mut components = Vec::new();
    for component in Path::new(relative_text.as_ref()).components() {
        match component {
            Component::Normal(value) => {
                let Some(value) = value.to_str() else {
                    return Err(PathValidationError::new(
                        ERROR_PATH_DENIED,
                        "workspace path component must be UTF-8",
                        None,
                    ));
                };
                if !allow_hidden && value.starts_with('.') {
                    return Err(PathValidationError::new(
                        ERROR_PATH_DENIED,
                        "workspace hidden paths are not allowed",
                        Some(relative_text.as_ref().to_owned()),
                    ));
                }
                components.push(value.to_owned());
            }
            // A `.` segment is redundant spelling for the same path, so it is
            // dropped. `..` is resolved here, before any filesystem access, and
            // cannot reach above the workspace root because popping an empty
            // component list is denied.
            Component::CurDir => {}
            Component::ParentDir => {
                if components.pop().is_none() {
                    return Err(PathValidationError::new(
                        ERROR_PATH_DENIED,
                        "workspace path escapes the workspace root through '..' components",
                        Some(relative_text.as_ref().to_owned()),
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathValidationError::new(
                    ERROR_PATH_DENIED,
                    "workspace path must be relative to a workspace root",
                    None,
                ));
            }
        }
    }

    if components.is_empty() {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must name a file inside the workspace root",
            Some(relative_text.as_ref().to_owned()),
        ));
    }

    let display = components.join("/");
    Ok(ValidatedRelativePath {
        components,
        display,
    })
}

/// Converts an absolute argument inside a workspace root into a relative path.
///
/// Returns `None` when the argument is already relative, and denies an absolute
/// path that no root contains. Roots are canonical and the comparison is
/// lexical over path components, so a path that only shares a prefix string
/// with a root does not match it.
fn absolute_path_within_root<I, P>(
    raw_path: &str,
    roots: I,
) -> Result<Option<String>, PathValidationError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let requested = Path::new(raw_path);
    if !requested.is_absolute() {
        return Ok(None);
    }

    for root in roots {
        let Ok(stripped) = requested.strip_prefix(root.as_ref()) else {
            continue;
        };
        let Some(stripped) = stripped.to_str() else {
            return Err(PathValidationError::new(
                ERROR_PATH_DENIED,
                "workspace path must be UTF-8",
                None,
            ));
        };
        if stripped.is_empty() {
            return Err(PathValidationError::new(
                ERROR_PATH_DENIED,
                "workspace path must name a file inside the workspace root, not the root itself",
                None,
            ));
        }
        return Ok(Some(stripped.to_owned()));
    }

    Err(PathValidationError::new(
        ERROR_PATH_DENIED,
        "workspace path is an absolute path outside every configured workspace root; use a workspace-relative path or an absolute path inside the workspace",
        None,
    ))
}
