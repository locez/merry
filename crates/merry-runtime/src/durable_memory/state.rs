//! Per-scope coordination record.
//!
//! The file stores read counts, read snapshots, per-session capture cursors,
//! and the dream lease. Updates hold an exclusive lock on the state file and
//! rewrite it in place. One-shot dream does not need a lexical search index.

use std::{
    collections::BTreeMap,
    fs,
    io::{Seek, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::{DurableMemoryError, paths::STATE_FILE};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ScopeState {
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) topic_reads: BTreeMap<String, u64>,
    #[serde(default)]
    pub(crate) snapshots: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) session_turns: BTreeMap<String, u32>,
    #[serde(default)]
    pub(crate) dream_lease: Option<DreamLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DreamLease {
    pub(crate) owner: String,
    pub(crate) expires_at: u64,
}

impl Default for ScopeState {
    fn default() -> Self {
        Self {
            version: 1,
            topic_reads: BTreeMap::new(),
            snapshots: BTreeMap::new(),
            session_turns: BTreeMap::new(),
            dream_lease: None,
        }
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn state_path(scope_dir: &Path) -> PathBuf {
    scope_dir.join(STATE_FILE)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), DurableMemoryError> {
    let mut temporary_name = path.as_os_str().to_owned();
    temporary_name.push(".tmp");
    let temporary = PathBuf::from(temporary_name);
    {
        let mut file = fs::File::create(&temporary).map_err(|source| DurableMemoryError::Io {
            operation: "create memory temporary file",
            path: temporary.clone(),
            source,
        })?;
        file.write_all(bytes)
            .map_err(|source| DurableMemoryError::Io {
                operation: "write memory temporary file",
                path: temporary.clone(),
                source,
            })?;
        file.sync_all().map_err(|source| DurableMemoryError::Io {
            operation: "sync memory temporary file",
            path: temporary.clone(),
            source,
        })?;
    }
    fs::rename(&temporary, path).map_err(|source| {
        let _ = fs::remove_file(&temporary);
        DurableMemoryError::Io {
            operation: "publish memory file",
            path: path.to_path_buf(),
            source,
        }
    })
}

/// Applies a read-modify-write to one scope under an exclusive file lock.
///
/// The lock is the state file itself. It is held only for this update, so a
/// model call must not run inside `update`.
pub(crate) fn update<T>(
    scope_dir: &Path,
    mutate: impl FnOnce(&mut ScopeState) -> Result<T, DurableMemoryError>,
) -> Result<T, DurableMemoryError> {
    let path = state_path(scope_dir);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| DurableMemoryError::Io {
            operation: "open memory state",
            path: path.clone(),
            source,
        })?;
    file.lock().map_err(|source| DurableMemoryError::Io {
        operation: "lock memory state",
        path: path.clone(),
        source,
    })?;
    let mut locked = StateFileLock { file, path };
    let mut state = read_state(&mut locked.file, &locked.path)?;
    let value = mutate(&mut state)?;
    write_state(&mut locked.file, &locked.path, &state)?;
    Ok(value)
}

struct StateFileLock {
    file: fs::File,
    path: PathBuf,
}

impl Drop for StateFileLock {
    fn drop(&mut self) {
        if let Err(source) = self.file.unlock() {
            tracing::warn!(
                error = %source,
                path = %self.path.display(),
                "failed to unlock memory state"
            );
        }
    }
}

fn read_state(file: &mut fs::File, path: &Path) -> Result<ScopeState, DurableMemoryError> {
    use std::io::Read;
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|source| DurableMemoryError::Io {
            operation: "rewind memory state",
            path: path.to_path_buf(),
            source,
        })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| DurableMemoryError::Io {
            operation: "read memory state",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(ScopeState::default());
    }
    serde_json::from_slice(&bytes).map_err(|error| DurableMemoryError::CorruptState {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn write_state(
    file: &mut fs::File,
    path: &Path,
    state: &ScopeState,
) -> Result<(), DurableMemoryError> {
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|error| DurableMemoryError::CorruptState {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|source| DurableMemoryError::Io {
            operation: "rewind memory state",
            path: path.to_path_buf(),
            source,
        })?;
    file.set_len(0).map_err(|source| DurableMemoryError::Io {
        operation: "truncate memory state",
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(&bytes)
        .map_err(|source| DurableMemoryError::Io {
            operation: "write memory state",
            path: path.to_path_buf(),
            source,
        })?;
    file.sync_all().map_err(|source| DurableMemoryError::Io {
        operation: "sync memory state",
        path: path.to_path_buf(),
        source,
    })
}
