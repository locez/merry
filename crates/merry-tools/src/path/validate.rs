//! Validation and lexical normalization of a tool path argument.
//!
//! Validation never touches the filesystem. It decides what the caller named, in
//! which form to report it, and whether the form is usable at all, so the same
//! argument always normalizes to the same path.

use std::{
    borrow::Cow,
    path::{Component, Path, PathBuf},
};

use crate::errors::{ERROR_PATH_DENIED, PathValidationError};

/// A tool path argument after validation and lexical normalization.
#[derive(Debug, Clone)]
pub(crate) struct ValidatedToolPath {
    /// Absolute directory an absolute argument is anchored at, or `None` when
    /// the argument is relative and a configured root supplies the anchor.
    ///
    /// An absolute path that lies inside the workspace root keeps the relative
    /// form instead, so it is reported the same way as the equivalent relative
    /// path and the same scope rules apply to it.
    anchored: Option<PathBuf>,
    pub(crate) components: Vec<String>,
    /// Normalized form reported in results and diagnostics.
    pub(crate) display: String,
}

impl ValidatedToolPath {
    /// Returns the anchors and components this argument resolves under, in order.
    ///
    /// An absolute argument names exactly one path, so it has one anchor and it
    /// does not matter which roots the caller passes. A relative argument names
    /// one path per root, which is what lets a skill's own relative `SKILL.md`
    /// path resolve below a read-only resource root.
    pub(super) fn anchors<'a>(
        &'a self,
        roots: impl IntoIterator<Item = &'a PathBuf>,
    ) -> Vec<(PathBuf, &'a [String])> {
        match self.anchored.as_ref() {
            Some(anchor) => vec![(anchor.clone(), self.components.as_slice())],
            None => roots
                .into_iter()
                .map(|root| (root.clone(), self.components.as_slice()))
                .collect(),
        }
    }

    /// Returns whether the tools walk this argument themselves below a root.
    ///
    /// A relative argument is walked without following symlinks, so a path
    /// inside the workspace cannot be redirected through a link. An absolute
    /// argument is resolved by the operating system as the caller named it,
    /// which is what lets a sandbox-exposed location such as a platform `/tmp`
    /// link work; the leaf stays protected because every open in this crate
    /// uses `O_NOFOLLOW`.
    #[must_use]
    pub(crate) fn denies_symlinks(&self) -> bool {
        self.anchored.is_none()
    }

    /// Returns the workspace-relative spelling this argument resolved to.
    ///
    /// Child workspace scope patterns and forbidden paths are root-relative, so
    /// only an argument that resolved below the workspace root has a spelling
    /// they can match. An absolute argument outside the root is `None`, which no
    /// relative pattern authorizes.
    #[must_use]
    pub(crate) fn relative_spelling(&self) -> Option<&str> {
        self.anchored.is_none().then_some(self.display.as_str())
    }
}

/// Validates a tool path and normalizes its components.
///
/// Every spelling a caller may reasonably produce is accepted: a path relative
/// to the workspace root, an absolute path inside it, an absolute path outside
/// it, a path that climbs above the root with `..`, and a path whose components
/// start with a dot such as `.github/workflows`. Shape is not policy here. The
/// tools are not the sandbox: the sandbox, accepted process profile, and trusted
/// path rules decide which paths exist and which of them are writable, and
/// re-deciding that inside the tool would only hide their answer.
///
/// An absolute path below the workspace root is rewritten to its relative
/// spelling, because that is the same file, it keeps tool results free of host
/// paths, and it lets root-relative scope rules match. Every other absolute
/// path is kept as the caller wrote it instead of being matched against any
/// root, so naming one file can never resolve to a different same-named file.
///
/// Normalization stays lexical so a result is reproducible without touching the
/// filesystem: `.` is dropped, `..` pops the component before it, and an
/// absolute argument clamps at its filesystem anchor. A relative argument that
/// climbs past its root keeps the leading `..`, which is what makes the escape
/// visible in the reported path instead of being silently rewritten.
///
/// Child workspace scope is unaffected, because that is a deliberate narrowing
/// of one child agent rather than sandbox policy: its patterns are root-relative,
/// so a path outside the workspace root has no scope spelling to authorize it.
pub(crate) fn validate_workspace_path_argument(
    raw_path: &str,
    workspace_root: &Path,
) -> Result<ValidatedToolPath, PathValidationError> {
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
        match strip_workspace_root(requested, workspace_root)? {
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
/// Returns `None` when the argument is outside the root. The root is canonical
/// and the comparison is lexical over path components, so a path that only
/// shares a prefix string with the root does not match it.
fn strip_workspace_root<'a>(
    requested: &'a Path,
    workspace_root: &Path,
) -> Result<Option<&'a str>, PathValidationError> {
    let Ok(stripped) = requested.strip_prefix(workspace_root) else {
        return Ok(None);
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
    Ok(Some(stripped))
}

/// Splits an absolute path into its filesystem anchor and the text below it.
///
/// The anchor is the root or volume prefix, which is what an absolute argument
/// outside the workspace root is relative to. `..` is resolved here: a `..`
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
