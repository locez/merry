use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::errors::{
    DomainError, ERROR_FILE_ALREADY_EXISTS, ERROR_NOT_DIRECTORY, ERROR_PATH_DENIED,
    ERROR_READ_FAILED, ERROR_WRITE_FAILED, PathValidationError,
};

pub(crate) fn join_display_path(prefix: &str, name: &str) -> String {
    if prefix == "." {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

pub(crate) fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

pub(crate) fn truncate_utf8_line(line: &str, max_bytes: usize) -> (String, bool) {
    if line.len() <= max_bytes {
        return (line.to_owned(), false);
    }

    let end = (0..=max_bytes)
        .rev()
        .find(|index| line.is_char_boundary(*index))
        .expect("zero is always a UTF-8 character boundary");
    (line[..end].to_owned(), true)
}

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

#[cfg(unix)]
fn open_file_for_read_impl(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
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

pub(crate) fn validate_relative_path(
    raw_path: &str,
    allow_hidden: bool,
) -> Result<ValidatedRelativePath, PathValidationError> {
    validate_relative_path_impl(raw_path, allow_hidden, false)
}

pub(crate) fn validate_relative_path_or_root(
    raw_path: &str,
    allow_hidden: bool,
) -> Result<ValidatedRelativePath, PathValidationError> {
    validate_relative_path_impl(raw_path, allow_hidden, true)
}

fn validate_relative_path_impl(
    raw_path: &str,
    allow_hidden: bool,
    allow_root: bool,
) -> Result<ValidatedRelativePath, PathValidationError> {
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

    if allow_root && raw_path == "." {
        return Ok(ValidatedRelativePath {
            components: Vec::new(),
            display: ".".to_owned(),
        });
    }

    let path = Path::new(raw_path);
    if path.is_absolute() {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must be relative",
            None,
        ));
    }

    if has_forbidden_raw_dot_segment(raw_path) {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must not contain '.' or '..' components",
            Some(raw_path.to_owned()),
        ));
    }

    let mut components = Vec::new();
    for component in path.components() {
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
                        Some(raw_path.to_owned()),
                    ));
                }
                components.push(value.to_owned());
            }
            Component::CurDir => {
                return Err(PathValidationError::new(
                    ERROR_PATH_DENIED,
                    "workspace path must not contain '.' components",
                    Some(raw_path.to_owned()),
                ));
            }
            Component::ParentDir => {
                return Err(PathValidationError::new(
                    ERROR_PATH_DENIED,
                    "workspace path must not contain '..' components",
                    Some(raw_path.to_owned()),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathValidationError::new(
                    ERROR_PATH_DENIED,
                    "workspace path must be relative",
                    None,
                ));
            }
        }
    }

    if components.is_empty() {
        let message = if allow_root {
            "workspace path must be exact '.' or name a relative path"
        } else {
            "workspace path must name a file"
        };
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            message,
            Some(raw_path.to_owned()),
        ));
    }

    let display = components.join("/");
    Ok(ValidatedRelativePath {
        components,
        display,
    })
}

fn has_forbidden_raw_dot_segment(raw_path: &str) -> bool {
    raw_path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
}
