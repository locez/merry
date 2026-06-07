use std::{io, path::PathBuf};

use thiserror::Error;

/// Limits applied by workspace tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceToolLimits {
    /// Maximum bytes read from one file by `workspace_read_file` and `workspace_search_text`.
    pub max_read_bytes: usize,
    /// Maximum bytes written to one file by `workspace_patch`.
    pub max_write_bytes: usize,
    /// Maximum bytes accepted in one `workspace_patch` patch payload.
    pub max_patch_bytes: usize,
    /// Maximum entries returned by `workspace_list_dir`.
    pub max_list_entries: usize,
    /// Maximum matches returned by `workspace_search_text`.
    pub max_search_matches: usize,
    /// Maximum regular files inspected by one `workspace_search_text` call.
    pub max_search_files: usize,
    /// Maximum directory entries inspected by one `workspace_search_text` call.
    pub max_search_entries: usize,
    /// Maximum total bytes scanned by one `workspace_search_text` call.
    pub max_search_bytes: usize,
    /// Maximum bytes returned for one matched line by `workspace_search_text`.
    pub max_search_line_bytes: usize,
    /// Maximum bytes accepted in a `workspace_search_text` query.
    pub max_search_query_bytes: usize,
}

impl Default for WorkspaceToolLimits {
    fn default() -> Self {
        Self {
            max_read_bytes: 1024 * 1024,
            max_write_bytes: 1024 * 1024,
            max_patch_bytes: 128 * 1024,
            max_list_entries: 512,
            max_search_matches: 100,
            max_search_files: 1_000,
            max_search_entries: 10_000,
            max_search_bytes: 8 * 1024 * 1024,
            max_search_line_bytes: 8 * 1024,
            max_search_query_bytes: 1024,
        }
    }
}

/// Configuration for workspace tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceToolsConfig {
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) allow_hidden: bool,
    pub(crate) limits: WorkspaceToolLimits,
    pub(crate) patch_write_scope: Option<Vec<PathBuf>>,
    pub(crate) forbidden_paths: Vec<PathBuf>,
}

impl WorkspaceToolsConfig {
    /// Creates a config with explicit workspace roots.
    #[must_use]
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            allow_hidden: false,
            limits: WorkspaceToolLimits::default(),
            patch_write_scope: None,
            forbidden_paths: Vec::new(),
        }
    }

    /// Returns the configured, pre-canonical roots.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Returns whether hidden path components are allowed.
    #[must_use]
    pub fn allow_hidden(&self) -> bool {
        self.allow_hidden
    }

    /// Sets whether hidden path components are allowed.
    #[must_use]
    pub fn with_allow_hidden(mut self, allow_hidden: bool) -> Self {
        self.allow_hidden = allow_hidden;
        self
    }

    /// Returns the configured tool limits.
    #[must_use]
    pub fn limits(&self) -> &WorkspaceToolLimits {
        &self.limits
    }

    /// Returns the optional workspace-relative write scope for `workspace_patch`.
    #[must_use]
    pub fn patch_write_scope(&self) -> Option<&[PathBuf]> {
        self.patch_write_scope.as_deref()
    }

    /// Returns workspace-relative paths forbidden to `workspace_patch`.
    #[must_use]
    pub fn forbidden_paths(&self) -> &[PathBuf] {
        &self.forbidden_paths
    }

    /// Sets workspace tool limits.
    #[must_use]
    pub fn with_limits(mut self, limits: WorkspaceToolLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets the optional workspace-relative write scope for `workspace_patch`.
    ///
    /// `None` preserves existing unrestricted patch behavior under configured
    /// roots. `Some([])` makes the patch tool read-only by denying all writes.
    #[must_use]
    pub fn with_patch_write_scope(mut self, paths: Option<Vec<PathBuf>>) -> Self {
        self.patch_write_scope = paths;
        self
    }

    /// Sets workspace-relative paths forbidden to `workspace_patch`.
    #[must_use]
    pub fn with_forbidden_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.forbidden_paths = paths;
        self
    }
}

/// Errors raised while validating workspace tool configuration.
#[derive(Debug, Error)]
pub enum WorkspaceToolConfigError {
    /// At least one root must be configured explicitly.
    #[error("at least one workspace root must be configured")]
    NoRoots,
    /// A configured root does not exist.
    #[error("workspace root does not exist: {root}")]
    RootNotFound {
        /// Configured root path.
        root: PathBuf,
    },
    /// A configured root could not be canonicalized.
    #[error("could not canonicalize workspace root {root}: {source}")]
    RootCanonicalize {
        /// Configured root path.
        root: PathBuf,
        /// Source IO error.
        #[source]
        source: io::Error,
    },
    /// A configured root is not a directory.
    #[error("workspace root is not a directory: {root}")]
    RootNotDirectory {
        /// Configured root path.
        root: PathBuf,
    },
    /// A numeric limit is invalid.
    #[error("workspace tool limit {name} must be greater than zero")]
    InvalidLimit {
        /// Limit name.
        name: &'static str,
    },
    /// A workspace tool scope path was not relative and normalized.
    #[error("workspace tool scope path must be relative and normalized: {path}")]
    InvalidScopePath {
        /// Rejected scope path.
        path: PathBuf,
    },
}
