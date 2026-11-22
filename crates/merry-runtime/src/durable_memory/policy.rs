//! Path policy for durable memory writes and read accounting.
//!
//! A path outside both scope directories is left to the caller. A path inside
//! a scope is either a topic, an immutable observation, or protected storage.

use merry_core::host_paths::{canonicalize_clean, simplify_verbatim_path};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use super::{
    DurableMemoryError, MemoryLayout,
    paths::{ARCHIVE_DIR, INBOX_DIR, MANIFEST_FILE, STATE_FILE, TOPICS_DIR},
};

pub(crate) const TOPIC_MAX_BYTES: usize = 256 * 1024;
pub(crate) const OBSERVATION_MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClassifiedPath {
    pub(crate) scope_dir: PathBuf,
    pub(crate) absolute: PathBuf,
    pub(crate) kind: MemoryPathKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryPathKind {
    Topic { relative: String },
    Observation { relative: String },
    Protected { reason: &'static str },
}

pub(crate) fn classify(layout: &MemoryLayout, path: &Path) -> Option<ClassifiedPath> {
    let scopes = [
        (layout.global_dir(), "global"),
        (layout.workspace_dir(), "workspace"),
    ];
    for (scope_dir, _label) in scopes {
        if let Some(classified) = classify_scope(scope_dir, path) {
            return Some(classified);
        }
    }
    None
}

fn classify_scope(scope_dir: &Path, path: &Path) -> Option<ClassifiedPath> {
    let scope = canonicalize_clean(scope_dir).ok()?;
    let candidate = path_inside_scope(&scope, path)?;
    let relative = candidate.strip_prefix(&scope).ok()?;
    let parts = relative
        .components()
        .map(|component| match component {
            Component::Normal(part) => part.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let symlink = path_has_symlink(path) || path_has_symlink(&candidate);
    let kind = if symlink {
        MemoryPathKind::Protected {
            reason: "memory path uses a symlink",
        }
    } else {
        classify_parts(&parts)
    };
    Some(ClassifiedPath {
        scope_dir: scope_dir.to_path_buf(),
        absolute: candidate,
        kind,
    })
}

fn path_inside_scope(scope: &Path, path: &Path) -> Option<PathBuf> {
    let lexical = lexical_absolute(path)?;
    if lexical.starts_with(scope) {
        return Some(lexical);
    }
    if let Ok(canonical) = canonicalize_clean(path)
        && canonical.starts_with(scope)
    {
        return Some(canonical);
    }
    let mut ancestor = lexical.as_path();
    let mut missing = Vec::new();
    while let Some(name) = ancestor.file_name() {
        missing.push(name.to_os_string());
        ancestor = ancestor.parent()?;
        if let Ok(canonical_ancestor) = canonicalize_clean(ancestor) {
            let mut resolved = canonical_ancestor;
            for part in missing.into_iter().rev() {
                resolved.push(part);
            }
            return resolved.starts_with(scope).then_some(resolved);
        }
    }
    None
}

fn lexical_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let path = simplify_verbatim_path(path);
    let mut anchor = PathBuf::new();
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => anchor.push(prefix.as_os_str()),
            Component::RootDir => anchor.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::Normal(part) => parts.push(part.to_os_string()),
        }
    }
    for part in parts {
        anchor.push(part);
    }
    Some(anchor)
}

fn path_has_symlink(path: &Path) -> bool {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component);
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        if fs::symlink_metadata(&cursor)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

fn classify_parts(parts: &[String]) -> MemoryPathKind {
    match parts {
        [file] if file == MANIFEST_FILE => MemoryPathKind::Protected {
            reason: "MEMORY.md is generated",
        },
        [file] if file == STATE_FILE => MemoryPathKind::Protected {
            reason: "memory state is internal",
        },
        [directory, ..] if directory == ARCHIVE_DIR => MemoryPathKind::Protected {
            reason: "archive is internal",
        },
        [directory, file] if directory == TOPICS_DIR && is_markdown_name(file) => {
            MemoryPathKind::Topic {
                relative: format!("{TOPICS_DIR}/{file}"),
            }
        }
        [observations, inbox, file]
            if format!("{observations}/{inbox}") == INBOX_DIR && is_markdown_name(file) =>
        {
            MemoryPathKind::Observation {
                relative: format!("{INBOX_DIR}/{file}"),
            }
        }
        _ => MemoryPathKind::Protected {
            reason: "only markdown files directly under topics/ or observations/_inbox/ can be written",
        },
    }
}

fn is_markdown_name(name: &str) -> bool {
    name.ends_with(".md") && !name.starts_with('.') && !name.contains(['/', '\\']) && name != ".md"
}

pub(crate) fn snapshot_key(path: &Path) -> String {
    canonicalize_clean(path)
        .unwrap_or_else(|_| simplify_verbatim_path(path))
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn deny(message: impl Into<String>) -> DurableMemoryError {
    DurableMemoryError::Denied {
        message: message.into(),
    }
}
