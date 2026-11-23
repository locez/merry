use merry_core::SessionId;
use std::{
    env,
    ffi::OsStr,
    fs::{File, OpenOptions as StdOpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::Mutex};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const JOURNAL_FILE_NAME: &str = "journal.jsonl";

const LEGACY_STATE_FILE_NAME: &str = "state.json";

const SESSION_LOCK_FILE_NAME: &str = ".session.lock";

const PLANS_DIR_NAME: &str = "plans";

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
use tokio::sync::Notify;

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("could not resolve XDG state home: neither XDG_STATE_HOME nor HOME is set")]
    StateHomeUnavailable,
    #[error("session store path {path} is invalid: {reason}")]
    InvalidPath { path: PathBuf, reason: &'static str },
    #[error("session store IO error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("session state JSON error: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },
    #[error("session document format version {actual} is not supported")]
    UnsupportedFormatVersion { actual: u32 },
    #[error("session document id {actual} does not match requested session {requested}")]
    SessionIdMismatch {
        requested: SessionId,
        actual: SessionId,
    },
    #[error("session {session_id} is already reserved by another run")]
    SessionAlreadyReserved { session_id: SessionId },
    #[error(
        "session {session_id} has {pending_count} pending tool calls and cannot be saved at an incomplete tool boundary"
    )]
    UnsafePendingToolCalls {
        session_id: SessionId,
        pending_count: usize,
    },
    #[error("session document is invalid: {reason}")]
    InvalidDocument { reason: &'static str },
}

/// File-backed session storage. Durable session facts append directly to `journal.jsonl`.
///
/// Store clones share an in-process append lock so runtime savepoints,
/// compaction, and plan updates cannot interleave bytes or write duplicate
/// headers. Callers that create or resume a live run should also hold a
/// [`SessionReservation`] for cross-process exclusion.
#[derive(Debug, Clone)]
pub struct FileSessionStore {
    sessions_dir: PathBuf,
    write_lock: Arc<Mutex<()>>,
    #[cfg(test)]
    stage_pause: Option<SessionStoreStagePause>,
    #[cfg(test)]
    commit_pause: Option<SessionStoreCommitPause>,
    #[cfg(test)]
    fail_commit: bool,
    #[cfg(test)]
    fail_directory_sync: bool,
}

/// Exclusive ownership of one session's on-disk state for the lifetime of a
/// run. The operating-system file lock is released when this value is dropped,
/// including when the owning process is terminated.
#[derive(Debug)]
pub struct SessionReservation {
    _lock_file: File,
}

/// Prepared in-memory journal append awaiting final transactional commit.
///
/// No temporary file is written for append-only commits: discarding simply
/// drops the prepared bytes, while committing appends directly to
/// `journal.jsonl` under the store's write lock and fsyncs once.
#[derive(Debug)]
pub(crate) struct StagedSessionBundle {
    final_path: PathBuf,
    header_bytes: Vec<u8>,
    commit_bytes: Vec<u8>,
    write_lock: Arc<Mutex<()>>,
    search: Option<search::SessionSearchUpdate>,
    rotation: Option<crate::session::PreparedJournalRotation>,
    #[cfg(test)]
    commit_pause: Option<SessionStoreCommitPause>,
    #[cfg(test)]
    fail_commit: bool,
    #[cfg(test)]
    fail_directory_sync: bool,
}

#[derive(Debug)]
#[must_use = "an appended session commit must be checked for directory durability"]
pub(crate) enum StagedSessionCommit {
    Durable,
    AppendedButNotSynced(SessionStoreError),
}

impl StagedSessionCommit {
    pub(crate) fn require_durable(self) -> Result<(), SessionStoreError> {
        match self {
            Self::Durable => Ok(()),
            Self::AppendedButNotSynced(error) => Err(error),
        }
    }
}

impl StagedSessionBundle {
    pub(crate) async fn commit(self) -> Result<StagedSessionCommit, SessionStoreError> {
        #[cfg(test)]
        if self.fail_commit {
            return Err(io_error(
                self.final_path,
                std::io::Error::other("injected session store commit failure"),
            ));
        }

        let created = {
            let _guard = self.write_lock.lock().await;
            if let Some(rotation) = self.rotation {
                crate::session::commit_journal_rotation(
                    &self.final_path,
                    &self.header_bytes,
                    &self.commit_bytes,
                    rotation,
                )
                .await?;
                true
            } else {
                append_journal_bytes(&self.final_path, &self.header_bytes, &self.commit_bytes)
                    .await?
            }
        };

        #[cfg(test)]
        if let Some(pause) = &self.commit_pause {
            pause.pause_once().await;
        }

        if let Some(search) = self.search {
            search::upsert_user_text(search).await;
        }

        #[cfg(test)]
        if self.fail_directory_sync {
            return Ok(StagedSessionCommit::AppendedButNotSynced(io_error(
                self.final_path
                    .parent()
                    .unwrap_or(self.final_path.as_path())
                    .to_path_buf(),
                std::io::Error::other("injected session directory sync failure"),
            )));
        }

        if created {
            match sync_parent_directory(&self.final_path).await {
                Ok(()) => Ok(StagedSessionCommit::Durable),
                Err(error) => Ok(StagedSessionCommit::AppendedButNotSynced(error)),
            }
        } else {
            Ok(StagedSessionCommit::Durable)
        }
    }

    pub(crate) async fn discard(self) -> Result<(), SessionStoreError> {
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct SessionStoreStagePause {
    inner: Arc<SessionStoreStagePauseInner>,
}

#[cfg(test)]
#[derive(Debug)]
struct SessionStoreStagePauseInner {
    claimed: AtomicBool,
    staged: Notify,
    resume: Notify,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct SessionStoreCommitPause {
    inner: Arc<SessionStoreCommitPauseInner>,
}

#[cfg(test)]
#[derive(Debug)]
struct SessionStoreCommitPauseInner {
    claimed: AtomicBool,
    committed: Notify,
    resume: Notify,
}

#[cfg(test)]
impl SessionStoreStagePause {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(SessionStoreStagePauseInner {
                claimed: AtomicBool::new(false),
                staged: Notify::new(),
                resume: Notify::new(),
            }),
        }
    }

    pub(crate) async fn wait_until_staged(&self) {
        self.inner.staged.notified().await;
    }

    pub(crate) fn resume(&self) {
        self.inner.resume.notify_one();
    }

    async fn pause_once(&self) {
        if self.inner.claimed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.inner.staged.notify_one();
        self.inner.resume.notified().await;
    }
}

#[cfg(test)]
impl SessionStoreCommitPause {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(SessionStoreCommitPauseInner {
                claimed: AtomicBool::new(false),
                committed: Notify::new(),
                resume: Notify::new(),
            }),
        }
    }

    pub(crate) async fn wait_until_committed(&self) {
        self.inner.committed.notified().await;
    }

    pub(crate) fn resume(&self) {
        self.inner.resume.notify_one();
    }

    async fn pause_once(&self) {
        if self.inner.claimed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.inner.committed.notify_one();
        self.inner.resume.notified().await;
    }
}

impl FileSessionStore {
    pub fn default_sessions_dir() -> Result<PathBuf, SessionStoreError> {
        let xdg_state_home = env::var_os("XDG_STATE_HOME");
        if let Some(root) = non_empty_os_str(xdg_state_home.as_deref()) {
            return Ok(PathBuf::from(root).join("merry").join("sessions"));
        }
        let local_appdata = env::var_os("LOCALAPPDATA").map(PathBuf::from);
        if cfg!(windows)
            && let Some(local) = local_appdata
                .as_deref()
                .filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
        {
            return Ok(local.join("merry").join("sessions"));
        }
        let Some(home) = merry_core::host_paths::user_home_dir() else {
            return Err(SessionStoreError::StateHomeUnavailable);
        };
        let state_base =
            merry_core::host_paths::platform_state_base(&home, None, local_appdata.as_deref());
        Ok(state_base.join("merry").join("sessions"))
    }

    pub fn default_store() -> Result<Self, SessionStoreError> {
        Ok(Self::new(Self::default_sessions_dir()?))
    }

    /// Creates a store rooted at a directory.
    ///
    /// Each session is stored below `<root>/<session_id>/journal.jsonl`.
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            sessions_dir: root.as_ref().to_path_buf(),
            write_lock: Arc::new(Mutex::new(())),
            #[cfg(test)]
            stage_pause: None,
            #[cfg(test)]
            commit_pause: None,
            #[cfg(test)]
            fail_commit: false,
            #[cfg(test)]
            fail_directory_sync: false,
        }
    }

    #[must_use]
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Returns a child-scoped session store rooted at `<sessions_dir>/<parent_id>/subagents`.
    ///
    /// Storing child journals under a nested `subagents/` directory keeps them
    /// durably paired with the parent session without polluting top-level
    /// session discovery listings.
    #[must_use]
    pub fn subagents_store(&self, parent_session_id: &SessionId) -> Self {
        Self::new(self.session_dir(parent_session_id).join("subagents"))
    }

    /// Reserves exclusive write ownership of a session id until the returned
    /// guard is dropped.
    ///
    /// The lock is acquired before callers inspect or write `journal.jsonl`, so
    /// independent runs cannot both pass a check-then-save collision window.
    /// The lock file is intentionally retained as an empty per-session inode;
    /// it is not session state and is ignored by session discovery.
    pub async fn reserve_session(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionReservation, SessionStoreError> {
        let session_dir = self.session_dir(session_id);
        tokio::fs::create_dir_all(&session_dir)
            .await
            .map_err(|source| io_error(session_dir.clone(), source))?;
        let lock_path = session_dir.join(SESSION_LOCK_FILE_NAME);
        let session_id = session_id.clone();
        let lock_file = tokio::task::spawn_blocking({
            let lock_path = lock_path.clone();
            move || {
                let mut options = StdOpenOptions::new();
                options.read(true).write(true).create(true);
                #[cfg(unix)]
                options.mode(0o600);
                let file = options
                    .open(&lock_path)
                    .map_err(|source| io_error(lock_path.clone(), source))?;
                #[cfg(unix)]
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|source| io_error(lock_path.clone(), source))?;
                match file.try_lock() {
                    Ok(()) => Ok(file),
                    Err(std::fs::TryLockError::WouldBlock) => {
                        Err(SessionStoreError::SessionAlreadyReserved {
                            session_id: session_id.clone(),
                        })
                    }
                    Err(std::fs::TryLockError::Error(source)) => Err(io_error(lock_path, source)),
                }
            }
        })
        .await
        .map_err(|source| {
            io_error(
                lock_path,
                std::io::Error::other(format!("session reservation task failed: {source}")),
            )
        })??;
        Ok(SessionReservation {
            _lock_file: lock_file,
        })
    }

    /// Reports whether this store already holds committed state for a session id.
    ///
    /// Saving appends `journal.jsonl`, so a new run that reuses an id would
    /// mix its transcript into an existing session. Callers that start a
    /// session check this first and refuse the id. A legacy `state.json` still
    /// occupies the id; those sessions are not resumed.
    pub async fn contains_session(
        &self,
        session_id: &SessionId,
    ) -> Result<bool, SessionStoreError> {
        for name in [JOURNAL_FILE_NAME, LEGACY_STATE_FILE_NAME] {
            let path = self.session_dir(session_id).join(name);
            if tokio::fs::try_exists(&path)
                .await
                .map_err(|source| io_error(path.clone(), source))?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn session_dir(&self, session_id: &SessionId) -> PathBuf {
        self.sessions_dir.join(session_id.as_str())
    }

    pub(crate) fn journal_path(&self, session_id: &SessionId) -> PathBuf {
        self.session_dir(session_id).join(JOURNAL_FILE_NAME)
    }

    #[cfg(test)]
    pub(crate) async fn overwrite_journal_bytes(
        &self,
        session_id: &SessionId,
        bytes: &[u8],
    ) -> Result<(), SessionStoreError> {
        let session_dir = self.session_dir(session_id);
        tokio::fs::create_dir_all(&session_dir)
            .await
            .map_err(|source| io_error(session_dir.clone(), source))?;
        let path = self.journal_path(session_id);
        let _guard = self.write_lock.lock().await;
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|source| io_error(path, source))
    }

    #[cfg(test)]
    pub(crate) async fn write_state_bytes(
        &self,
        session_id: &SessionId,
        bytes: &[u8],
    ) -> Result<(), SessionStoreError> {
        self.overwrite_journal_bytes(session_id, bytes).await
    }

    pub(crate) async fn stage_journal_append(
        &self,
        session_id: &SessionId,
        header_bytes: Vec<u8>,
        commit_bytes: Vec<u8>,
        images: &[crate::session::JournalImageFile],
        search_user_text: Option<String>,
        rotation: Option<crate::session::PreparedJournalRotation>,
    ) -> Result<StagedSessionBundle, SessionStoreError> {
        let session_dir = self.session_dir(session_id);
        tokio::fs::create_dir_all(&session_dir)
            .await
            .map_err(|source| io_error(session_dir.clone(), source))?;
        self.write_journal_images(session_id, images).await?;
        if let Some(rot) = rotation.as_ref() {
            self.write_journal_images(session_id, &rot.archive_images)
                .await?;
        }

        #[cfg(test)]
        if let Some(pause) = &self.stage_pause {
            pause.pause_once().await;
        }

        Ok(StagedSessionBundle {
            final_path: session_dir.join(JOURNAL_FILE_NAME),
            header_bytes,
            commit_bytes,
            write_lock: Arc::clone(&self.write_lock),
            search: search_user_text.map(|user_text| search::SessionSearchUpdate {
                sessions_dir: self.sessions_dir.clone(),
                session_id: session_id.clone(),
                user_text,
            }),
            rotation,
            #[cfg(test)]
            commit_pause: self.commit_pause.clone(),
            #[cfg(test)]
            fail_commit: self.fail_commit,
            #[cfg(test)]
            fail_directory_sync: self.fail_directory_sync,
        })
    }

    async fn write_journal_images(
        &self,
        session_id: &SessionId,
        images: &[crate::session::JournalImageFile],
    ) -> Result<(), SessionStoreError> {
        if images.is_empty() {
            return Ok(());
        }
        let directory = self.session_dir(session_id).join("images");
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|source| io_error(directory.clone(), source))?;
        for image in images {
            if !crate::session::is_image_id(&image.id) {
                return Err(SessionStoreError::InvalidDocument {
                    reason: "session journal image id is invalid",
                });
            }
            let path = directory.join(format!("{}.png", image.id));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = match options.open(&path).await {
                Ok(file) => file,
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(io_error(path, source)),
            };
            file.write_all(&image.bytes)
                .await
                .map_err(|source| io_error(path.clone(), source))?;
            file.sync_all()
                .await
                .map_err(|source| io_error(path, source))?;
        }
        Ok(())
    }

    pub(crate) async fn read_journal_bytes(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<u8>, SessionStoreError> {
        let path = self.journal_path(session_id);
        tokio::fs::read(&path)
            .await
            .map_err(|source| io_error(path, source))
    }

    /// Truncates an incomplete trailing line after a crash so the next append
    /// starts cleanly on a new line instead of gluing onto torn bytes.
    pub(crate) async fn truncate_journal_tail(
        &self,
        session_id: &SessionId,
        valid_len: u64,
    ) -> Result<(), SessionStoreError> {
        let path = self.journal_path(session_id);
        let _guard = self.write_lock.lock().await;
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .map_err(|source| io_error(path.clone(), source))?;
        let current_len = file
            .metadata()
            .await
            .map_err(|source| io_error(path.clone(), source))?
            .len();
        if current_len > valid_len {
            file.set_len(valid_len)
                .await
                .map_err(|source| io_error(path.clone(), source))?;
            file.sync_all()
                .await
                .map_err(|source| io_error(path, source))?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn read_state_bytes(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<u8>, SessionStoreError> {
        self.read_journal_bytes(session_id).await
    }

    /// Writes one named Markdown plan file under `<session_dir>/plans/<file_name>`.
    pub(crate) async fn write_plan_file(
        &self,
        session_id: &SessionId,
        file_name: &str,
        content: &str,
    ) -> Result<PathBuf, SessionStoreError> {
        if crate::plan::validate_plan_file_name(file_name).is_err() {
            return Err(SessionStoreError::InvalidDocument {
                reason: "plan file name is invalid",
            });
        }
        let directory = self.session_dir(session_id).join(PLANS_DIR_NAME);
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|source| io_error(directory.clone(), source))?;
        let path = directory.join(file_name);
        let tmp_path = directory.join(format!(".{file_name}.tmp"));
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&tmp_path)
            .await
            .map_err(|source| io_error(tmp_path.clone(), source))?;
        file.write_all(content.as_bytes())
            .await
            .map_err(|source| io_error(tmp_path.clone(), source))?;
        file.sync_all()
            .await
            .map_err(|source| io_error(tmp_path.clone(), source))?;
        drop(file);
        tokio::fs::rename(&tmp_path, &path)
            .await
            .map_err(|source| io_error(path.clone(), source))?;
        Ok(path)
    }

    /// Reads one named Markdown plan file from `<session_dir>/plans/<file_name>`.
    pub(crate) async fn read_plan_file(
        &self,
        session_id: &SessionId,
        file_name: &str,
    ) -> Result<Option<String>, SessionStoreError> {
        if crate::plan::validate_plan_file_name(file_name).is_err() {
            return Ok(None);
        }
        let path = self
            .session_dir(session_id)
            .join(PLANS_DIR_NAME)
            .join(file_name);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(Some(content)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io_error(path, source)),
        }
    }

    pub(crate) async fn read_legacy_bytes(
        &self,
        session_id: &SessionId,
        file_name: &str,
    ) -> Result<Option<Vec<u8>>, SessionStoreError> {
        let path = self.session_dir(session_id).join(file_name);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io_error(path, source)),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_stage_pause_for_tests(mut self, pause: SessionStoreStagePause) -> Self {
        self.stage_pause = Some(pause);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_commit_pause_for_tests(mut self, pause: SessionStoreCommitPause) -> Self {
        self.commit_pause = Some(pause);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_commit_failure_for_tests(mut self) -> Self {
        self.fail_commit = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_directory_sync_failure_for_tests(mut self) -> Self {
        self.fail_directory_sync = true;
        self
    }
}

#[cfg(test)]
pub(crate) fn sessions_dir_from_env(
    xdg_state_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, SessionStoreError> {
    if let Some(root) = non_empty_os_str(xdg_state_home) {
        return Ok(PathBuf::from(root).join("merry").join("sessions"));
    }

    if let Some(root) = non_empty_os_str(home) {
        return Ok(PathBuf::from(root).join(".local/state/merry/sessions"));
    }

    Err(SessionStoreError::StateHomeUnavailable)
}

fn non_empty_os_str(value: Option<&OsStr>) -> Option<&OsStr> {
    value.filter(|value| !value.is_empty())
}

/// Appends one commit directly to `journal.jsonl`, prepending the header line
/// atomically when the file is empty. Returns `true` when the journal started
/// empty so the caller can sync the parent directory once.
async fn append_journal_bytes(
    final_path: &Path,
    header_bytes: &[u8],
    commit_bytes: &[u8],
) -> Result<bool, SessionStoreError> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(final_path)
        .await
        .map_err(|source| io_error(final_path.to_path_buf(), source))?;
    let empty = file
        .metadata()
        .await
        .map_err(|source| io_error(final_path.to_path_buf(), source))?
        .len()
        == 0;
    if empty && !header_bytes.is_empty() {
        file.write_all(header_bytes)
            .await
            .map_err(|source| io_error(final_path.to_path_buf(), source))?;
    }
    if !commit_bytes.is_empty() {
        file.write_all(commit_bytes)
            .await
            .map_err(|source| io_error(final_path.to_path_buf(), source))?;
    }
    file.sync_all()
        .await
        .map_err(|source| io_error(final_path.to_path_buf(), source))?;
    Ok(empty)
}

#[cfg(unix)]
async fn sync_parent_directory(path: &Path) -> Result<(), SessionStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| SessionStoreError::InvalidPath {
            path: path.to_path_buf(),
            reason: "session state path has no parent directory",
        })?;
    let directory = tokio::fs::File::open(parent)
        .await
        .map_err(|source| io_error(parent.to_path_buf(), source))?;
    directory
        .sync_all()
        .await
        .map_err(|source| io_error(parent.to_path_buf(), source))
}

#[cfg(not(unix))]
async fn sync_parent_directory(_path: &Path) -> Result<(), SessionStoreError> {
    Ok(())
}

fn io_error(path: PathBuf, source: std::io::Error) -> SessionStoreError {
    SessionStoreError::Io { path, source }
}

mod search;

#[cfg(test)]
mod tests;
