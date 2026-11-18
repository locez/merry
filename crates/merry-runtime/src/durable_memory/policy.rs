//! Path policy for durable memory writes and read accounting.
//!
//! A path outside both scope directories is left to the caller. A path inside
//! a scope is either a topic, an immutable observation, or protected storage.

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
    let scope = fs::canonicalize(scope_dir).ok()?;
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
    if let Ok(canonical) = fs::canonicalize(path)
        && canonical.starts_with(scope)
    {
        return Some(canonical);
    }
    let parent = path.parent()?;
    let parent = fs::canonicalize(parent).ok()?;
    if parent.starts_with(scope) {
        let name = path.file_name()?;
        return Some(parent.join(name));
    }
    None
}

fn lexical_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

fn path_has_symlink(path: &Path) -> bool {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component);
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
    name.ends_with(".md") && !name.starts_with('.') && !name.contains('/') && name != ".md"
}

pub(crate) fn snapshot_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn deny(message: impl Into<String>) -> DurableMemoryError {
    DurableMemoryError::Denied {
        message: message.into(),
    }
}
