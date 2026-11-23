use std::{
    collections::BTreeSet,
    fs,
    path::{Component, PathBuf},
    sync::Arc,
};

use merry_runtime::DurableMemory;

use crate::{
    config::{WorkspaceToolConfigError, WorkspaceToolLimits, WorkspaceToolsConfig},
    errors::{DomainError, ERROR_PATH_DENIED},
    path::ValidatedToolPath,
};

#[derive(Debug)]
pub(crate) struct WorkspaceToolState {
    pub(crate) root: PathBuf,
    pub(crate) readonly_resource_roots: Vec<PathBuf>,
    pub(crate) limits: WorkspaceToolLimits,
    pub(crate) read_scope: Option<Vec<String>>,
    pub(crate) patch_write_scope: Option<Vec<String>>,
    pub(crate) read_forbidden_paths: Vec<String>,
    pub(crate) forbidden_paths: Vec<String>,
    pub(crate) denied_absolute_paths: Vec<PathBuf>,
    pub(crate) durable_memory: Option<Arc<DurableMemory>>,
}

impl WorkspaceToolState {
    pub(crate) fn new(config: WorkspaceToolsConfig) -> Result<Self, WorkspaceToolConfigError> {
        validate_limits(&config.limits)?;

        let root = canonical_root(config.root)?;
        let mut readonly_resource_roots = Vec::with_capacity(config.readonly_resource_roots.len());
        for resource_root in config.readonly_resource_roots {
            if let Some(canonical) = canonical_optional_root(resource_root)?
                && canonical != root
            {
                readonly_resource_roots.push(canonical);
            }
        }

        let read_scope = match config.read_scope {
            Some(paths) => Some(normalize_scope_paths(paths)?),
            None => None,
        };
        let patch_write_scope = match config.patch_write_scope {
            Some(paths) => Some(normalize_scope_paths(paths)?),
            None => None,
        };
        let read_forbidden_paths = normalize_scope_paths(config.forbidden_paths)?;
        let mut forbidden_paths = read_forbidden_paths.clone();
        if !forbidden_paths.iter().any(|path| path == ".git") {
            forbidden_paths.push(".git".to_owned());
        }
        let denied_absolute_paths = normalize_denied_absolute_paths(config.denied_absolute_paths);

        Ok(Self {
            root,
            readonly_resource_roots,
            limits: config.limits,
            read_scope,
            patch_write_scope,
            read_forbidden_paths,
            forbidden_paths,
            denied_absolute_paths,
            durable_memory: config.durable_memory,
        })
    }

    /// Returns whether `path` falls inside any host-level denied directory or
    /// file (such as Merry's private state or credential store).
    pub(crate) fn is_denied_absolute_path(&self, path: &std::path::Path) -> bool {
        if self.denied_absolute_paths.is_empty() {
            return false;
        }
        let lexical = normalize_path_lexically(path);
        let resolved = resolve_path_with_existing_ancestor(&lexical);
        self.denied_absolute_paths
            .iter()
            .any(|denied| lexical.starts_with(denied) || resolved.starts_with(denied))
    }

    /// Returns the workspace root followed by every read-only resource root.
    ///
    /// A relative argument is looked up in this order, so the workspace stays
    /// the primary namespace and a skill's own relative `SKILL.md` path still
    /// resolves. An absolute argument names one path and ignores this order.
    pub(crate) fn read_roots(&self) -> impl Iterator<Item = &PathBuf> {
        std::iter::once(&self.root).chain(self.readonly_resource_roots.iter())
    }

    /// Validates that a resolved path is permitted by read security boundaries.
    pub(crate) fn validate_read_boundary(
        &self,
        path: &ValidatedToolPath,
        resolved: &std::path::Path,
    ) -> Result<(), DomainError> {
        if self.is_denied_absolute_path(resolved) {
            return Err(DomainError::new(
                ERROR_PATH_DENIED,
                "path is denied by Merry security policy",
            ));
        }
        if self
            .readonly_resource_roots
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            return Ok(());
        }
        if let Some(memory) = &self.durable_memory
            && memory.governs_path(resolved)
        {
            return Ok(());
        }

        let scope_path = path.relative_spelling();
        if let Some(scope_path) = scope_path
            && matches_any_scope_path(scope_path, &self.read_forbidden_paths)
        {
            return Err(DomainError::new(
                ERROR_PATH_DENIED,
                "workspace read path is forbidden by the child workspace scope",
            ));
        }

        let Some(read_scope) = &self.read_scope else {
            return Ok(());
        };
        match scope_path {
            Some(scope_path) if matches_any_scope_path(scope_path, read_scope) => Ok(()),
            _ => {
                let message = if read_scope.is_empty() {
                    "workspace read path is outside the configured read scope; this read scope allows no workspace files"
                } else {
                    "workspace read path is outside the configured read scope"
                };
                Err(DomainError::new(ERROR_PATH_DENIED, message))
            }
        }
    }
}

/// Canonicalizes a required workspace root.
fn canonical_root(root: PathBuf) -> Result<PathBuf, WorkspaceToolConfigError> {
    if !root.exists() {
        return Err(WorkspaceToolConfigError::RootNotFound { root });
    }

    let canonical =
        fs::canonicalize(&root).map_err(|source| WorkspaceToolConfigError::RootCanonicalize {
            root: root.clone(),
            source,
        })?;

    if !canonical.is_dir() {
        return Err(WorkspaceToolConfigError::RootNotDirectory { root });
    }

    Ok(canonical)
}

/// Canonicalizes an optional read-only resource root.
///
/// Resource roots are optional configuration, so a missing or non-directory
/// entry is skipped instead of failing the whole tool set. A root that exists
/// but cannot be canonicalized is still an error.
fn canonical_optional_root(root: PathBuf) -> Result<Option<PathBuf>, WorkspaceToolConfigError> {
    match canonical_root(root) {
        Ok(canonical) => Ok(Some(canonical)),
        Err(WorkspaceToolConfigError::RootNotFound { .. })
        | Err(WorkspaceToolConfigError::RootNotDirectory { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn normalize_scope_paths(paths: Vec<PathBuf>) -> Result<Vec<String>, WorkspaceToolConfigError> {
    let paths = paths
        .into_iter()
        .map(normalize_scope_path)
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(paths.into_iter().collect())
}

fn normalize_scope_path(path: PathBuf) -> Result<String, WorkspaceToolConfigError> {
    let Some(path_text) = path.to_str() else {
        return Err(WorkspaceToolConfigError::InvalidScopePath { path });
    };

    if path_text == "." {
        return Ok(String::new());
    }

    if path_text.is_empty()
        || path.is_absolute()
        || path_text.contains('\\')
        || path_text.chars().any(char::is_control)
        || path_text
            .split('/')
            .any(|segment| matches!(segment, "" | "." | ".."))
    {
        return Err(WorkspaceToolConfigError::InvalidScopePath { path });
    }

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let Some(value) = value.to_str() else {
                    return Err(WorkspaceToolConfigError::InvalidScopePath { path });
                };
                components.push(value.to_owned());
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(WorkspaceToolConfigError::InvalidScopePath { path });
            }
        }
    }

    if components.is_empty() {
        return Err(WorkspaceToolConfigError::InvalidScopePath { path });
    }

    Ok(components.join("/"))
}

pub(crate) fn matches_any_scope_path(relative: &str, scope_paths: &[String]) -> bool {
    scope_paths
        .iter()
        .any(|scope_path| matches_scope_path(relative, scope_path))
}

fn matches_scope_path(relative: &str, scope_path: &str) -> bool {
    scope_path.is_empty()
        || relative == scope_path
        || relative
            .strip_prefix(scope_path)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_limits(limits: &WorkspaceToolLimits) -> Result<(), WorkspaceToolConfigError> {
    for (name, value) in [
        ("max_read_bytes", limits.max_read_bytes),
        ("max_read_lines", limits.max_read_lines),
        ("max_write_bytes", limits.max_write_bytes),
        ("max_patch_bytes", limits.max_patch_bytes),
    ] {
        if value == 0 {
            return Err(WorkspaceToolConfigError::InvalidLimit { name });
        }
    }

    Ok(())
}

fn normalize_denied_absolute_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut normalized = BTreeSet::new();
    for path in paths {
        if !path.is_absolute() {
            continue;
        }
        let lexical = normalize_path_lexically(&path);
        let resolved = resolve_path_with_existing_ancestor(&lexical);
        normalized.insert(lexical);
        normalized.insert(resolved);
    }
    normalized.into_iter().collect()
}

fn resolve_path_with_existing_ancestor(path: &std::path::Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    let mut suffix = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        if let Some(name) = current.file_name() {
            suffix.push(name.to_os_string());
        }
        if let Ok(canonical_parent) = fs::canonicalize(parent) {
            let mut resolved = canonical_parent;
            for part in suffix.into_iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        current = parent;
    }
    path.to_path_buf()
}

fn normalize_path_lexically(path: &std::path::Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let is_absolute = path.is_absolute();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !is_absolute {
                    normalized.push("..");
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}
