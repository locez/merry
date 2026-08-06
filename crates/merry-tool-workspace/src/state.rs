use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use crate::config::{WorkspaceToolConfigError, WorkspaceToolLimits, WorkspaceToolsConfig};

const CODING_PROFILE_CAPABILITY_SUMMARY: &str = "\
Workspace coding profile:\n- Workspace file tool paths are relative to configured workspace roots, not host-absolute paths.\n- Process execution runs through Merry runtime policy and the configured sandbox/profile, so filesystem and network access may be intentionally restricted; environment and host IPC access may also be intentionally restricted.\n- For run_process, omit cwd or set cwd=\".\" for the workspace root; do not pass an empty cwd string.\n- The default process profile may block network access and paths outside the configured workspace or trusted path rules.\n- Use run_process directly for read-only checks such as cargo fmt --all --check; do not call request_permissions for an action that already fits the active process profile.\n- A failed process action is the signal to recover. If the failure appears caused by unavailable network, filesystem, or host-integration access (including its required environment), call request_permissions for that exact action and request only the corresponding minimum capability before retrying it.\n- Linux Unix sockets are filesystem paths. If a host resource is not represented by a named integration, request its exact socket/file path through requested.paths; the outer sandbox must already expose that path.\n- request_permissions is not a reusable grant. It must name the exact planned action, request only the minimum needed capability, and the runtime may approve, deny, or fail the request.";

#[derive(Debug)]
pub(crate) struct WorkspaceToolState {
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) allow_hidden: bool,
    pub(crate) limits: WorkspaceToolLimits,
    pub(crate) patch_write_scope: Option<Vec<String>>,
    pub(crate) forbidden_paths: Vec<String>,
}

impl WorkspaceToolState {
    pub(crate) fn new(config: WorkspaceToolsConfig) -> Result<Self, WorkspaceToolConfigError> {
        if config.roots.is_empty() {
            return Err(WorkspaceToolConfigError::NoRoots);
        }

        validate_limits(&config.limits)?;

        let mut roots = Vec::with_capacity(config.roots.len());
        for root in config.roots {
            if !root.exists() {
                return Err(WorkspaceToolConfigError::RootNotFound { root });
            }

            let canonical = fs::canonicalize(&root).map_err(|source| {
                WorkspaceToolConfigError::RootCanonicalize {
                    root: root.clone(),
                    source,
                }
            })?;

            if !canonical.is_dir() {
                return Err(WorkspaceToolConfigError::RootNotDirectory { root });
            }

            roots.push(canonical);
        }

        let patch_write_scope = match config.patch_write_scope {
            Some(paths) => Some(normalize_scope_paths(paths)?),
            None => None,
        };
        let forbidden_paths = normalize_scope_paths(config.forbidden_paths)?;

        Ok(Self {
            roots,
            allow_hidden: config.allow_hidden,
            limits: config.limits,
            patch_write_scope,
            forbidden_paths,
        })
    }

    pub(crate) fn project_capability_summary(&self) -> String {
        match self
            .roots
            .iter()
            .find_map(|root| project_capability_summary_for_root(root))
        {
            Some(project_summary) => {
                format!("{CODING_PROFILE_CAPABILITY_SUMMARY}\n{project_summary}")
            }
            None => CODING_PROFILE_CAPABILITY_SUMMARY.to_owned(),
        }
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
        ("max_write_bytes", limits.max_write_bytes),
        ("max_patch_bytes", limits.max_patch_bytes),
        ("max_list_entries", limits.max_list_entries),
        ("max_search_matches", limits.max_search_matches),
        ("max_search_files", limits.max_search_files),
        ("max_search_entries", limits.max_search_entries),
        ("max_search_bytes", limits.max_search_bytes),
        ("max_search_line_bytes", limits.max_search_line_bytes),
        ("max_search_query_bytes", limits.max_search_query_bytes),
    ] {
        if value == 0 {
            return Err(WorkspaceToolConfigError::InvalidLimit { name });
        }
    }

    Ok(())
}

fn project_capability_summary_for_root(root: &Path) -> Option<String> {
    let mut lines = Vec::new();
    let mut checks = Vec::new();

    if root.join("Cargo.toml").is_file() {
        lines.push(
            "Detected Rust project metadata: Cargo.toml is present at the workspace root."
                .to_owned(),
        );
        checks.push("cargo fmt --all --check");
        checks.push("cargo clippy --all-targets --all-features -- -D warnings");
        checks.push("cargo test --all");
    }

    if root.join("justfile").is_file() || root.join("Justfile").is_file() {
        lines.push(
            "Detected justfile; prefer project-provided just tasks when AGENTS.md or user instructions name them."
                .to_owned(),
        );
    }

    if root.join("package.json").is_file() {
        lines.push(
            "Detected JavaScript/TypeScript project metadata: package.json is present.".to_owned(),
        );
    }

    if root.join("pyproject.toml").is_file() {
        lines.push("Detected Python project metadata: pyproject.toml is present.".to_owned());
    }

    if !checks.is_empty() {
        lines.push(format!(
            "Default Rust verification candidates if not overridden by AGENTS.md or the user: {}.",
            checks.join("; ")
        ));
    }

    if lines.is_empty() {
        return None;
    }

    Some(lines.join("\n"))
}
