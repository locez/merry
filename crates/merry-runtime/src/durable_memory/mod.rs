//! Durable cross-session memory.
//!
//! Notes live under `$XDG_DATA_HOME/merry/memory`. The agent reads and writes
//! them with the ordinary file tools. After a completed user turn, the host
//! extracts observations and may consolidate them with one-shot dream.
//! Coordination state is a JSON file beside the notes. There is no vector index.

pub(crate) mod capture;
pub(crate) mod condense;
pub(crate) mod dream;
mod manifest;
mod paths;
mod policy;
pub(crate) mod state;

#[cfg(test)]
mod tests;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use paths::{MANIFEST_FILE, ensure_scope};
use policy::{MemoryPathKind, OBSERVATION_MAX_BYTES, TOPIC_MAX_BYTES};
use thiserror::Error;
use tokio::sync::Mutex;

pub use paths::MemoryLayout;

const LEASE_SECONDS: u64 = 10 * 60;
const GLOBAL_HEADING: &str = "Global memory index";
const WORKSPACE_HEADING: &str = "Workspace memory index";

/// Why a durable-memory operation failed.
#[derive(Debug, Error)]
pub enum DurableMemoryError {
    /// A required path was relative.
    #[error("{role} must be an absolute path: {path}")]
    RelativePath {
        /// What the path was supposed to name.
        role: &'static str,
        /// Rejected path.
        path: PathBuf,
    },
    /// `HOME` was missing or relative, so the XDG data home cannot be resolved.
    #[error("HOME is missing or is not an absolute path")]
    HomeMissing,
    /// Filesystem operation failed.
    #[error("failed to {operation} at {path}: {source}")]
    Io {
        /// What was being attempted.
        operation: &'static str,
        /// Path involved in the failure.
        path: PathBuf,
        /// Source IO error.
        #[source]
        source: std::io::Error,
    },
    /// The workspace root exists and is not a directory.
    #[error("workspace root is not a directory: {path}")]
    WorkspaceNotDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// The coordination file could not be decoded.
    #[error("memory state at {path} is corrupt: {message}")]
    CorruptState {
        /// State file path.
        path: PathBuf,
        /// Decoder message.
        message: String,
    },
    /// A memory write violated the filesystem policy.
    #[error("{message}")]
    Denied {
        /// Actionable reason.
        message: String,
    },
}

/// Kind of `apply_patch` change aimed at a memory file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryWriteKind {
    /// Create a file that does not exist yet.
    Create,
    /// Replace an existing topic.
    Update,
    /// Remove an existing topic.
    Delete,
}

/// Open memory store for one workspace, shared by parent and child runtimes.
#[derive(Debug)]
pub struct DurableMemory {
    layout: MemoryLayout,
    maintenance: Mutex<()>,
}

impl DurableMemory {
    /// Creates both scopes and writes their initial indexes.
    pub fn open(layout: MemoryLayout) -> Result<Arc<Self>, DurableMemoryError> {
        ensure_scope(layout.global_dir())?;
        ensure_scope(layout.workspace_dir())?;
        let memory = Arc::new(Self {
            layout,
            maintenance: Mutex::new(()),
        });
        memory.refresh_scope(memory.layout.global_dir(), GLOBAL_HEADING)?;
        memory.refresh_scope(memory.layout.workspace_dir(), WORKSPACE_HEADING)?;
        Ok(memory)
    }

    /// Opens the store at the XDG data-home location for `workspace_root`.
    pub fn open_from_env(workspace_root: &Path) -> Result<Arc<Self>, DurableMemoryError> {
        Self::open(MemoryLayout::from_env(workspace_root)?)
    }

    /// Returns the resolved global and workspace directories.
    #[must_use]
    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
    }

    /// Stable instructions for the provider prefix.
    ///
    /// The injected index is separate and belongs in the dynamic segment.
    #[must_use]
    pub fn instructions(&self) -> String {
        let global = self.layout.global_dir().display();
        let workspace = self.layout.workspace_dir().display();
        format!(
            r#"Memory is a user-controlled filesystem knowledge base of what earlier sessions learned. The memory index in `merry_memory_context` lists every topic and any pending observations, so never read or edit MEMORY.md. Before starting work in an area, read the topic files whose titles cover it, and open the paths their `## Files` sections name before listing or searching the tree. Skip memory only for requests with no plausible overlap with past work. The user's instructions in this conversation override memory. A note marked as a past agent decision is a record, not a rule, so verify it against the current tree. When the request conflicts with the situation a note describes, follow the request.

Global memory, shared across workspaces:
- `{global}/topics/` holds maintained Markdown notes
- `{global}/observations/_inbox/` holds new Markdown observations
- `{global}/MEMORY.md` is the generated topic index and is already injected

Workspace memory, specific to this workspace:
- `{workspace}/topics/` holds maintained Markdown notes
- `{workspace}/observations/_inbox/` holds new Markdown observations
- `{workspace}/MEMORY.md` is the generated topic index and is already injected

`topics/` holds durable preferences, conventions, architecture, decisions, recurring workflows, and other facts worth reusing. `observations/_inbox/` holds new observations that may later be consolidated into topics. Paths in the index are relative to the scope root named in its header.

Use `read_text` to read memory files and `apply_patch` to create or edit them. Search with `run_process` when a memory file points at a path you still need to find. Existing files must be read successfully before editing. Writes are allowed only to `.md` files directly under `topics/` or `observations/_inbox/`. Generated indexes, archives, state files, and other internals are protected.

Remember information when the user explicitly asks, or when it is stable, specific, useful across sessions, and not already available from the repository or its documentation. Do not store secrets, credentials, transient task state, speculative conclusions, or facts that are likely to become stale. Prefer one focused topic file over duplicating the same fact in several places.

Treat memory as historical context, not current truth. Verify paths, commands, repository state, external facts, and other changeable claims with live tools before relying on them. When live tools and memory disagree, the live tools win."#
        )
    }

    /// Renders the global and workspace indexes for one session.
    ///
    /// Callers freeze the returned string for the life of the runtime so later
    /// topic writes do not rewrite the provider prefix.
    pub fn render_context(&self) -> Result<String, DurableMemoryError> {
        let global = self.manifest_text(self.layout.global_dir(), GLOBAL_HEADING)?;
        let workspace = self.manifest_text(self.layout.workspace_dir(), WORKSPACE_HEADING)?;
        let global_pending = pending_observations(self.layout.global_dir())?;
        let workspace_pending = pending_observations(self.layout.workspace_dir())?;
        Ok(manifest::render_injected_context(
            self.layout.global_dir(),
            &global,
            &global_pending,
            self.layout.workspace_dir(),
            &workspace,
            &workspace_pending,
        ))
    }

    /// Records a successful read of a topic and snapshots its full bytes.
    ///
    /// Observation files, the generated index, and paths outside memory are
    /// ignored. A snapshot failure is returned to the caller, which should
    /// keep the file read itself successful.
    pub fn note_read(&self, path: &Path) -> Result<(), DurableMemoryError> {
        let Some(classified) = policy::classify(&self.layout, path) else {
            return Ok(());
        };
        let MemoryPathKind::Topic { relative } = classified.kind else {
            return Ok(());
        };
        let bytes = fs::read(&classified.absolute).map_err(|source| DurableMemoryError::Io {
            operation: "read memory topic for snapshot",
            path: classified.absolute.clone(),
            source,
        })?;
        let hash = paths::content_hash(&bytes);
        let key = policy::snapshot_key(&classified.absolute);
        state::update(&classified.scope_dir, move |state| {
            state.snapshots.insert(key, hash);
            *state.topic_reads.entry(relative).or_insert(0) += 1;
            Ok(())
        })
    }

    /// Allows a patch when `path` is outside memory or the memory write is valid.
    ///
    /// Topic updates and deletes require a snapshot from [`Self::note_read`]
    /// that still matches the current file. The snapshot is not refreshed by
    /// the write, so the next edit must read the file again.
    pub fn authorize_write(
        &self,
        kind: MemoryWriteKind,
        path: &Path,
        byte_len: usize,
    ) -> Result<(), DurableMemoryError> {
        let Some(classified) = policy::classify(&self.layout, path) else {
            return Ok(());
        };
        match classified.kind {
            MemoryPathKind::Protected { reason } => Err(policy::deny(reason)),
            MemoryPathKind::Observation { .. } => match kind {
                MemoryWriteKind::Create if byte_len <= OBSERVATION_MAX_BYTES => Ok(()),
                MemoryWriteKind::Create => {
                    Err(policy::deny("memory observations are limited to 16 KiB"))
                }
                MemoryWriteKind::Update | MemoryWriteKind::Delete => Err(policy::deny(
                    "observations are immutable; add a new observation or update a topic",
                )),
            },
            MemoryPathKind::Topic { .. } => match kind {
                MemoryWriteKind::Create if byte_len <= TOPIC_MAX_BYTES => Ok(()),
                MemoryWriteKind::Create => {
                    Err(policy::deny("memory topics are limited to 256 KiB"))
                }
                MemoryWriteKind::Update if byte_len > TOPIC_MAX_BYTES => {
                    Err(policy::deny("memory topics are limited to 256 KiB"))
                }
                MemoryWriteKind::Update | MemoryWriteKind::Delete => {
                    self.require_snapshot(&classified)
                }
            },
        }
    }

    /// Rewrites indexes for scopes touched by a successful memory write.
    pub fn refresh_after_writes(&self, paths: &[PathBuf]) -> Result<(), DurableMemoryError> {
        let mut refreshed = Vec::new();
        for path in paths {
            let Some(classified) = policy::classify(&self.layout, path) else {
                continue;
            };
            if !matches!(
                classified.kind,
                MemoryPathKind::Topic { .. } | MemoryPathKind::Observation { .. }
            ) {
                continue;
            }
            if refreshed.iter().any(|dir| dir == &classified.scope_dir) {
                continue;
            }
            let heading = self.heading_for(&classified.scope_dir);
            self.refresh_scope(&classified.scope_dir, heading)?;
            refreshed.push(classified.scope_dir);
        }
        Ok(())
    }

    pub(crate) fn maintenance(&self) -> &Mutex<()> {
        &self.maintenance
    }

    pub(crate) fn captured_turns(&self, session_id: &str) -> Result<u32, DurableMemoryError> {
        let session_id = session_id.to_owned();
        state::update(self.layout.workspace_dir(), move |state| {
            Ok(state.session_turns.get(&session_id).copied().unwrap_or(0))
        })
    }

    pub(crate) fn set_captured_turns(
        &self,
        session_id: &str,
        turns: u32,
    ) -> Result<(), DurableMemoryError> {
        let session_id = session_id.to_owned();
        state::update(self.layout.workspace_dir(), move |state| {
            let entry = state.session_turns.entry(session_id).or_insert(0);
            if *entry < turns {
                *entry = turns;
            }
            Ok(())
        })
    }

    pub(crate) fn try_acquire_lease(&self, owner: &str) -> Result<bool, DurableMemoryError> {
        let owner = owner.to_owned();
        let now = state::unix_now();
        state::update(self.layout.workspace_dir(), move |state| {
            if let Some(lease) = &state.dream_lease
                && lease.expires_at > now
                && lease.owner != owner
            {
                return Ok(false);
            }
            state.dream_lease = Some(state::DreamLease {
                owner,
                expires_at: now.saturating_add(LEASE_SECONDS),
            });
            Ok(true)
        })
    }

    pub(crate) fn release_lease(&self, owner: &str) -> Result<(), DurableMemoryError> {
        let owner = owner.to_owned();
        state::update(self.layout.workspace_dir(), move |state| {
            if state
                .dream_lease
                .as_ref()
                .is_some_and(|lease| lease.owner == owner)
            {
                state.dream_lease = None;
            }
            Ok(())
        })
    }

    pub(crate) fn scan_inbox(&self) -> Result<dream::InboxScan, DurableMemoryError> {
        dream::scan_inbox(self.layout.workspace_dir())
    }

    pub(crate) fn dream_input(
        &self,
        claimed: &[dream::ClaimedNote],
    ) -> Result<String, DurableMemoryError> {
        dream::render_dream_input(self.layout.workspace_dir(), claimed)
    }

    pub(crate) fn apply_dream(
        &self,
        claimed: &[dream::ClaimedNote],
        response: &str,
    ) -> Result<usize, DurableMemoryError> {
        let count = dream::apply_plan(self.layout.workspace_dir(), claimed, response)?;
        self.refresh_scope(self.layout.workspace_dir(), WORKSPACE_HEADING)?;
        Ok(count)
    }

    pub(crate) fn observation_exists(
        &self,
        session_id: &str,
        from_turn: u32,
        through_turn: u32,
    ) -> bool {
        let name = capture::observation_file_name(session_id, from_turn, through_turn, 1);
        self.layout
            .workspace_dir()
            .join(paths::INBOX_DIR)
            .join(name)
            .is_file()
    }

    pub(crate) fn commit_capture(
        &self,
        write: &capture::CaptureWrite<'_>,
    ) -> Result<(), DurableMemoryError> {
        if write.observations.is_empty()
            || capture::first_observation_exists(self.layout.workspace_dir(), write)
        {
            return Ok(());
        }
        capture::write_observations(self.layout.workspace_dir(), write)?;
        self.refresh_scope(self.layout.workspace_dir(), WORKSPACE_HEADING)?;
        Ok(())
    }

    fn require_snapshot(
        &self,
        classified: &policy::ClassifiedPath,
    ) -> Result<(), DurableMemoryError> {
        let bytes = fs::read(&classified.absolute).map_err(|source| DurableMemoryError::Io {
            operation: "read memory file for snapshot check",
            path: classified.absolute.clone(),
            source,
        })?;
        let hash = paths::content_hash(&bytes);
        let key = policy::snapshot_key(&classified.absolute);
        let saved = state::update(&classified.scope_dir, |state| Ok(state.snapshots.clone()))?;
        match saved.get(&key) {
            Some(existing) if existing == &hash => Ok(()),
            Some(_) => Err(policy::deny(
                "memory file changed since it was read; read_text it again before editing",
            )),
            None => Err(policy::deny("read_text the memory file before editing it")),
        }
    }

    fn refresh_scope(&self, scope_dir: &Path, heading: &str) -> Result<String, DurableMemoryError> {
        let reads = state::update(scope_dir, |state| Ok(state.topic_reads.clone()))?;
        manifest::write_scope_manifest(scope_dir, heading, &reads)
    }

    fn manifest_text(&self, scope_dir: &Path, heading: &str) -> Result<String, DurableMemoryError> {
        let path = scope_dir.join(MANIFEST_FILE);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.refresh_scope(scope_dir, heading)
            }
            Err(source) => Err(DurableMemoryError::Io {
                operation: "read memory index",
                path,
                source,
            }),
        }
    }

    fn heading_for(&self, scope_dir: &Path) -> &'static str {
        if scope_dir == self.layout.global_dir() {
            GLOBAL_HEADING
        } else {
            WORKSPACE_HEADING
        }
    }
}

/// Lists inbox notes that are not topics yet.
///
/// An empty inbox returns an empty string so the injected index stays
/// identical to the on-disk topic index. At most 32 titles are listed.
fn pending_observations(scope_dir: &Path) -> Result<String, DurableMemoryError> {
    let scan = dream::scan_inbox(scope_dir)?;
    if scan.eligible.is_empty() && scan.oversized == 0 {
        return Ok(String::new());
    }
    let mut section = String::from(
        "\n## Pending observations\n\nThese inbox notes are not topics yet. Read them before treating this scope as empty.\n",
    );
    const LISTED: usize = 32;
    for note in scan.eligible.iter().take(LISTED) {
        let title = manifest::observation_title(&note.absolute_path);
        section.push_str(&format!("\n- **{title}** (`{}`)\n", note.relative_path));
    }
    let omitted = scan.eligible.len().saturating_sub(LISTED) + scan.oversized;
    if omitted > 0 {
        section.push_str(&format!("\n{omitted} more observations are not listed.\n"));
    }
    Ok(section)
}
