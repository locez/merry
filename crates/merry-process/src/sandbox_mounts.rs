//! Admitted mount inputs and immutable prepared sandbox plans.

mod bwrap;
mod inputs;
mod links;
mod namespace;
mod protections;

use crate::SandboxPathError;
use inputs::MountInput;
use merry_runtime::PathAccess;
use namespace::Namespace;
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

const MAX_MOUNTS: usize = 8192;
const MAX_LINKS: usize = 8192;

/// A permitted host source and its resolved location in the planned sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPathSource {
    source: PathBuf,
    destination: PathBuf,
}

impl SandboxPathSource {
    /// Creates an explicit source/destination mapping without inspecting either path.
    #[must_use]
    pub fn new(source: PathBuf, destination: PathBuf) -> Self {
        Self {
            source,
            destination,
        }
    }

    /// Returns the host path from which permitted bytes may be read.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Returns the resolved sandbox path at which a replacement must be mounted.
    #[must_use]
    pub fn destination(&self) -> &Path {
        &self.destination
    }
}

/// A preserved link whose dependencies cannot be completed from admitted imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxLinkIssue {
    /// No import authorizes the referenced host target; no new access was granted.
    UnexposedTarget { link: PathBuf, target: PathBuf },
    /// A dangling link or inaccessible directory retains its original behavior.
    Unavailable { path: PathBuf },
    /// A link cycle was preserved rather than recursively expanded.
    Cycle { path: PathBuf },
}

/// A failure to construct a bounded, unambiguous mount plan.
#[derive(Debug, thiserror::Error)]
pub enum SandboxMountError {
    /// Mount coordinates must be absolute namespace paths.
    #[error("sandbox mount path must be absolute: {path}")]
    RelativePath { path: PathBuf },
    /// A required metadata or link inspection failed.
    #[error("cannot inspect sandbox mount path {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// An explicitly requested destination cannot be resolved safely.
    #[error(transparent)]
    Path(#[from] SandboxPathError),
    /// Creating an alias would overwrite an existing namespace entry.
    #[error("sandbox link conflicts with an existing import at {path}")]
    Conflict { path: PathBuf },
    /// A source alias changed after its target was planned.
    #[error("sandbox source symlink changed during preparation: {path}")]
    ChangedLink { path: PathBuf },
    /// Planning stops rather than silently claiming an incomplete dependency scan.
    #[error("sandbox symlink dependency scan exceeds its {kind} limit at {path}")]
    Limit { kind: &'static str, path: PathBuf },
    /// A protection mount could not be represented without changing host files.
    #[error("cannot prepare sandbox protection: {0}")]
    Protection(#[source] merry_runtime::ProcessRunnerError),
}

/// Admitted mount inputs, before dependency completion and namespace finalization.
///
/// Callers own permission precedence. Completing this builder consumes its mutable
/// inputs and returns a plan that cannot register new mounts or bypass preparation.
#[derive(Debug, Default, Clone)]
pub struct SandboxMountPlan {
    imports: Vec<MountInput>,
    opaque: Vec<PathBuf>,
}

impl SandboxMountPlan {
    /// Creates empty inputs with no implicit host-root access.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an admitted import or deny; only source metadata is inspected.
    /// Relative coordinates and excessive mount counts fail without modifying inputs.
    pub fn bind(
        &mut self,
        source: &Path,
        destination: &Path,
        access: PathAccess,
        optional: bool,
    ) -> Result<(), SandboxMountError> {
        if self.imports.len() >= MAX_MOUNTS {
            return Err(SandboxMountError::Limit {
                kind: "mount count",
                path: destination.to_path_buf(),
            });
        }
        self.imports
            .push(MountInput::new(source, destination, access, optional)?);
        Ok(())
    }

    /// Registers an absolute runtime filesystem whose children must not resolve on the host.
    pub fn opaque(&mut self, destination: &Path) -> Result<(), SandboxMountError> {
        absolute(destination)?;
        self.opaque.push(destination.to_path_buf());
        Ok(())
    }

    /// Resolves destinations in registration order before caller-specific precedence.
    /// This uses the prepared namespace resolver without scanning dependencies or
    /// authorizing imports. Invalid or cyclic destinations return a planning error.
    pub fn resolved_destinations(&self) -> Result<Vec<PathBuf>, SandboxMountError> {
        let mut namespace = Namespace::new(&self.imports, self.opaque.clone());
        namespace.normalize_destinations()?;
        Ok(namespace
            .bindings()
            .iter()
            .map(|binding| binding.destination.clone())
            .collect())
    }

    /// Preserves source links and inspects only each requested directory's direct children.
    /// Only newly imported directory targets add scan roots. Access demands merge
    /// within the original grants without rescanning directories or widening denies.
    /// Unavailable dependencies retain their runtime behavior and produce diagnostics;
    /// invalid explicit mounts, ambiguous aliases, and exceeded bounds fail preparation.
    pub fn complete(
        self,
        scan_roots: &[PathBuf],
    ) -> Result<PreparedSandboxMountPlan, SandboxMountError> {
        let mut namespace = Namespace::new(&self.imports, self.opaque);
        let issues = links::complete(&mut namespace, &self.imports, scan_roots)?;
        namespace.normalize_destinations()?;
        protections::apply(&mut namespace)?;
        namespace.normalize_destinations()?;
        Ok(PreparedSandboxMountPlan { namespace, issues })
    }
}

/// A finalized namespace for read-only queries and bubblewrap argument generation.
///
/// New inputs require a new builder; a prepared plan cannot be mutated into an
/// unvalidated state. Recreated source aliases are revalidated before argument emission.
///
/// ```compile_fail
/// use merry_process::SandboxMountPlan;
/// use merry_runtime::PathAccess;
/// use std::path::Path;
/// let mut prepared = SandboxMountPlan::new().complete(&[]).unwrap();
/// prepared.bind(Path::new("/usr"), Path::new("/usr"), PathAccess::ReadOnly, false);
/// ```
#[derive(Debug, Clone)]
pub struct PreparedSandboxMountPlan {
    namespace: Namespace,
    issues: Vec<SandboxLinkIssue>,
}

impl PreparedSandboxMountPlan {
    /// Returns dependency diagnostics without reading file contents.
    #[must_use]
    pub fn issues(&self) -> &[SandboxLinkIssue] {
        &self.issues
    }

    /// Resolves an absolute sandbox path through admitted imports, never the host root.
    pub fn destination(&self, path: &Path) -> Result<PathBuf, SandboxMountError> {
        self.namespace.destination(path)
    }

    /// Returns the admitted host source and resolved target, or no mapping for
    /// absent, denied, or runtime-provided paths. Inspection failures remain errors.
    pub fn resolve(&self, path: &Path) -> Result<Option<SandboxPathSource>, SandboxMountError> {
        self.namespace.resolve(path)
    }

    pub(crate) fn resolve_checked(
        &self,
        path: &Path,
        visible: impl Fn(&Path) -> bool,
    ) -> Result<Option<SandboxPathSource>, SandboxMountError> {
        self.namespace.resolve_checked(path, visible)
    }

    /// Emits parent-first mounts and preserved aliases without modifying the host.
    /// Snapshot destinations suppress file binds, not deliberate replacements.
    /// Changed source links and failed protection preparation return errors.
    pub fn append_args(
        &self,
        args: &mut Vec<OsString>,
        replaces_file: impl Fn(&Path) -> bool,
    ) -> Result<(), SandboxMountError> {
        bwrap::append_args(&self.namespace, args, replaces_file)
    }
}

fn absolute(path: &Path) -> Result<(), SandboxMountError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(SandboxMountError::RelativePath {
            path: path.to_path_buf(),
        })
    }
}

fn inspect(path: &Path) -> Result<Option<fs::Metadata>, SandboxMountError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::NotADirectory
                    | io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(None)
        }
        Err(source) => Err(SandboxMountError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn with_suffix(mut path: PathBuf, suffix: &Path) -> PathBuf {
    if !suffix.as_os_str().is_empty() {
        path.push(suffix);
    }
    path
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
