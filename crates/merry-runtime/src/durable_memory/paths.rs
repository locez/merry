//! XDG data-home layout for durable memory.
//!
//! Notes live under `$XDG_DATA_HOME/merry/memory` (default
//! `$HOME/.local/share/merry/memory`). Coordination state sits beside the
//! notes so one scope can be committed together. There is no vector index.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::DurableMemoryError;

pub(crate) const GLOBAL_DIR: &str = "global";
pub(crate) const WORKSPACES_DIR: &str = "workspaces";
pub(crate) const TOPICS_DIR: &str = "topics";
pub(crate) const INBOX_DIR: &str = "observations/_inbox";
pub(crate) const ARCHIVE_DIR: &str = "archive";
pub(crate) const MANIFEST_FILE: &str = "MEMORY.md";
pub(crate) const STATE_FILE: &str = "memory_state.json";

/// Roots for the global scope and one workspace scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLayout {
    root: PathBuf,
    global_dir: PathBuf,
    workspace_dir: PathBuf,
    workspace_id: String,
}

impl MemoryLayout {
    /// Builds a layout under an explicit memory root.
    ///
    /// `memory_root` is the directory that contains `global/` and
    /// `workspaces/`. `workspace_root` is the project directory whose canonical
    /// path selects the workspace scope.
    pub fn at(memory_root: &Path, workspace_root: &Path) -> Result<Self, DurableMemoryError> {
        if !memory_root.is_absolute() {
            return Err(DurableMemoryError::RelativePath {
                role: "memory root",
                path: memory_root.to_path_buf(),
            });
        }
        let workspace_root = canonical_directory(workspace_root)?;
        let workspace_id = workspace_id(&workspace_root);
        let root = memory_root.to_path_buf();
        Ok(Self {
            global_dir: root.join(GLOBAL_DIR),
            workspace_dir: root.join(WORKSPACES_DIR).join(&workspace_id),
            workspace_id,
            root,
        })
    }

    /// Resolves the layout from `HOME` and `XDG_DATA_HOME`.
    ///
    /// An absolute `XDG_DATA_HOME` wins. Otherwise the root is
    /// `$HOME/.local/share/merry/memory`.
    pub fn from_env(workspace_root: &Path) -> Result<Self, DurableMemoryError> {
        Self::at(&Self::memory_root_from_env()?, workspace_root)
    }

    /// Directory that contains `global/` and `workspaces/`.
    ///
    /// An absolute `XDG_DATA_HOME` wins. Otherwise the root is
    /// `$HOME/.local/share/merry/memory`. `HOME` must still be absolute.
    pub fn memory_root_from_env() -> Result<PathBuf, DurableMemoryError> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(DurableMemoryError::HomeMissing)?;
        let data_home = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"));
        Ok(data_home.join("merry/memory"))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn global_dir(&self) -> &Path {
        &self.global_dir
    }

    #[must_use]
    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}

pub(crate) fn ensure_scope(scope_dir: &Path) -> Result<(), DurableMemoryError> {
    for relative in [TOPICS_DIR, INBOX_DIR, ARCHIVE_DIR] {
        let directory = scope_dir.join(relative);
        fs::create_dir_all(&directory).map_err(|source| DurableMemoryError::Io {
            operation: "create memory directory",
            path: directory,
            source,
        })?;
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, DurableMemoryError> {
    if !path.is_absolute() {
        return Err(DurableMemoryError::RelativePath {
            role: "workspace root",
            path: path.to_path_buf(),
        });
    }
    let canonical = fs::canonicalize(path).map_err(|source| DurableMemoryError::Io {
        operation: "canonicalize workspace root",
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(DurableMemoryError::WorkspaceNotDirectory { path: canonical });
    }
    Ok(canonical)
}

fn workspace_id(canonical_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_root.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    hex_encode(&digest[..8])
}

pub(crate) fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}
