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

/// Resolves a validated tool path to the file it names, when that file exists.
///
/// A target inside a configured root is walked without following symlinks, so a
/// workspace path cannot be silently redirected through a link to somewhere
/// else. A target outside every root is the sandbox's business: its components
/// are resolved by the operating system, and the leaf stays protected because
/// every open in this crate uses `O_NOFOLLOW`.
pub(crate) fn resolve_existing_path(
    root: &Path,
    path: &ValidatedToolPath,
) -> Result<Option<ResolvedWorkspacePath>, DomainError> {
    let (anchor, components) = path.anchor(root);
    let deny_symlinks = path.resolves_below_root(root);
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
    path: &ValidatedToolPath,
) -> Result<NewWorkspacePath, DomainError> {
    let (anchor, components) = path.anchor(root);
    let deny_symlinks = path.resolves_below_root(root);
    let last_index = components.len().saturating_sub(1);
    let mut current = anchor.to_path_buf();

    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if index == last_index {
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
pub(crate) struct ValidatedToolPath {
    /// Absolute directory a non-rooted argument is anchored at, or `None` when
    /// the argument is relative and each configured root supplies the anchor.
    anchored: Option<PathBuf>,
    pub(crate) components: Vec<String>,
    /// Normalized form reported in results and diagnostics.
    pub(crate) display: String,
}

impl ValidatedToolPath {
    /// Returns the anchor directory and the components below it for `root`.
    ///
    /// A relative argument is anchored at `root`, so a caller that tries every
    /// configured root keeps first-match-wins behavior. An absolute argument
    /// ignores `root`, which makes every iteration resolve the same path.
    #[must_use]
    pub(crate) fn anchor<'a>(&'a self, root: &'a Path) -> (&'a Path, &'a [String]) {
        match self.anchored.as_deref() {
            Some(anchor) => (anchor, &self.components),
            None => (root, &self.components),
        }
    }

    /// Returns the concrete path this argument names when anchored at `root`.
    #[must_use]
    pub(crate) fn resolved(&self, root: &Path) -> PathBuf {
        let (anchor, _) = self.anchor(root);
        self.joined_under(anchor)
    }

    /// Returns the absolute path this argument names, when it is absolute.
    #[must_use]
    pub(crate) fn absolute_path(&self) -> Option<PathBuf> {
        self.anchored
            .as_ref()
            .map(|anchor| self.joined_under(anchor))
    }

    /// Returns whether this argument resolves inside the configured `root`.
    ///
    /// A rooted path is the tools' own resolution responsibility, so the symlink
    /// rule applies to it. A path outside every root is resolved for the caller
    /// as the sandbox allows, including through a symlinked component such as a
    /// `/tmp` link that a platform layout introduces.
    #[must_use]
    pub(crate) fn resolves_below_root(&self, root: &Path) -> bool {
        self.absolute_path().is_none_or(|absolute| {
            absolute
                .strip_prefix(root)
                .is_ok_and(|rest| !rest.as_os_str().is_empty())
        })
    }

    /// Joins the components below `anchor` into one path.
    fn joined_under(&self, anchor: &Path) -> PathBuf {
        self.components
            .iter()
            .fold(anchor.to_path_buf(), |mut path, component| {
                path.push(component);
                path
            })
    }
}

/// Validates a tool path and normalizes its components.
///
/// Every spelling a caller may reasonably produce is accepted: a path relative
/// to a workspace root, an absolute path inside one, an absolute path outside
/// every root, a path that climbs above its root with `..`, and a path whose
/// components start with a dot such as `.github/workflows`. Shape is not policy
/// here. The tools are not the sandbox: the sandbox, accepted process profile,
/// and trusted path rules decide which paths exist and which of them are
/// writable, and re-deciding that inside the tool would only hide their answer.
///
/// Normalization stays lexical so a result is reproducible without touching the
/// filesystem: `.` is dropped, `..` pops the component before it, and an
/// absolute argument clamps at its filesystem anchor. A relative argument that
/// climbs past its root keeps the leading `..`, which is what makes the escape
/// visible in the reported path instead of being silently rewritten.
///
/// Child workspace scope is unaffected, because that is a deliberate narrowing
/// of one child agent rather than sandbox policy: its patterns are root-relative,
/// so a target outside every root has no scope spelling to authorize it.
pub(crate) fn validate_workspace_path_argument<I, P>(
    raw_path: &str,
    roots: I,
) -> Result<ValidatedToolPath, PathValidationError>
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

    let requested = Path::new(raw_path);
    let (anchored, relative_text, clamp) = if requested.is_absolute() {
        match relative_to_configured_root(requested, roots)? {
            Some(stripped) => (None, Cow::Borrowed(stripped), false),
            None => {
                let (anchor, remainder) = split_filesystem_root(requested)?;
                (Some(anchor), Cow::Owned(remainder), true)
            }
        }
    } else {
        (None, Cow::Borrowed(raw_path), false)
    };
    // Failure text reports the argument in the same form the success path
    // reports: workspace-relative for a rooted argument, and the normalized
    // absolute path for an absolute argument, which the caller already named.
    let reported = |text: &str| -> String {
        match anchored.as_ref() {
            Some(anchor) => anchor
                .join(text)
                .to_str()
                .map_or_else(|| text.to_owned(), str::to_owned),
            None => text.to_owned(),
        }
    };
    let mut components: Vec<String> = Vec::new();
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
                components.push(value.to_owned());
            }
            // A `.` segment is redundant spelling for the same path, so it is
            // dropped. `..` is resolved here, before any filesystem access. An
            // absolute argument clamps at its anchor, so `/..` names `/`. A
            // relative argument keeps a `..` that has nothing left to pop, so
            // the reported path still shows that it left the root.
            Component::CurDir => {}
            Component::ParentDir => match components.last() {
                Some(last) if last == ".." => components.push("..".to_owned()),
                Some(_) => {
                    components.pop();
                }
                // `clamp` marks an absolute argument anchored at the filesystem
                // root, where `..` above the anchor still names the anchor.
                None if clamp => {}
                None => components.push("..".to_owned()),
            },
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathValidationError::new(
                    ERROR_PATH_DENIED,
                    "workspace path must be relative to a workspace root or absolute",
                    None,
                ));
            }
        }
    }

    // The reported form keeps the anchor of an absolute argument, so a caller
    // that named `/a/b` sees `/a/b` again instead of a bare `a/b`.
    let display = if components.is_empty() && anchored.is_none() {
        ".".to_owned()
    } else {
        reported(&components.join("/"))
    };
    Ok(ValidatedToolPath {
        anchored,
        components,
        display,
    })
}

/// Converts an absolute argument inside a workspace root into a relative path.
///
/// Returns `None` when no root contains the argument. Roots are canonical and
/// the comparison is lexical over path components, so a path that only shares a
/// prefix string with a root does not match it.
fn relative_to_configured_root<I, P>(
    requested: &Path,
    roots: I,
) -> Result<Option<&str>, PathValidationError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
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
        return Ok(Some(stripped));
    }

    Ok(None)
}

/// Splits an absolute path into its filesystem anchor and the text below it.
///
/// The anchor is the root or volume prefix, which is what an absolute argument
/// outside every configured root is relative to. `..` is resolved here: a `..`
/// at the anchor names the anchor itself, so popping an empty remainder keeps
/// the anchor instead of failing.
fn split_filesystem_root(requested: &Path) -> Result<(PathBuf, String), PathValidationError> {
    let mut anchor = PathBuf::new();
    let mut remainder = Vec::new();

    for component in requested.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => anchor.push(component.as_os_str()),
            Component::Normal(value) => {
                let Some(value) = value.to_str() else {
                    return Err(PathValidationError::new(
                        ERROR_PATH_DENIED,
                        "workspace path component must be UTF-8",
                        None,
                    ));
                };
                remainder.push(value.to_owned());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                remainder.pop();
            }
        }
    }

    if remainder.is_empty() {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must name a file, not a filesystem root",
            None,
        ));
    }

    Ok((anchor, remainder.join("/")))
}
