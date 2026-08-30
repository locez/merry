use merry_core::SessionId;
use std::{
    env,
    ffi::OsStr,
    fmt,
    fs::{File, OpenOptions as StdOpenOptions},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use tokio::fs::OpenOptions;

/// Identifies which persisted Plan snapshot failed document validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanPersistenceLocation {
    Active,
    Terminal { index: usize },
    OverlayActive,
    OverlayTerminal { index: usize },
}

impl fmt::Display for PlanPersistenceLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => formatter.write_str("active plan"),
            Self::Terminal { index } => write!(formatter, "terminal plan {index}"),
            Self::OverlayActive => formatter.write_str("plan overlay active plan"),
            Self::OverlayTerminal { index } => {
                write!(formatter, "plan overlay terminal plan {index}")
            }
        }
    }
}

const TEMP_FILE_CREATE_ATTEMPTS: u32 = 1_024;
const TEMP_FILE_PREFIX: &str = ".state.json.tmp-";
const PLAN_OVERLAY_TEMP_FILE_PREFIX: &str = ".plan-state.json.tmp-";
const PLAN_OVERLAY_FILE_NAME: &str = "plan-state.json";
const SESSION_LOCK_FILE_NAME: &str = ".session.lock";

#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
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
    #[error("persisted plan at {location} is invalid: {source}")]
    InvalidPlan {
        location: PlanPersistenceLocation,
        #[source]
        source: crate::PlanError,
    },
}

pub(crate) fn invalid_plan(
    location: PlanPersistenceLocation,
    source: crate::PlanError,
) -> SessionStoreError {
    SessionStoreError::InvalidPlan { location, source }
}

pub(crate) fn validate_plan_snapshot(
    snapshot: &merry_core::PlanSnapshot,
    location: PlanPersistenceLocation,
) -> Result<(), SessionStoreError> {
    crate::plan::validate_snapshot_limits(snapshot).map_err(|source| invalid_plan(location, source))
}

/// File-backed session state storage with atomic replacement per write.
///
/// Store clones are safe to use through one runtime's serialized session
/// lifecycle. Callers that create or resume a live run should hold a
/// [`SessionReservation`] for the session id while it is active.
#[derive(Debug, Clone)]
pub struct FileSessionStore {
    sessions_dir: PathBuf,
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

#[derive(Debug)]
pub(crate) struct StagedSessionBundle {
    temp_path: PathBuf,
    final_path: PathBuf,
    #[cfg(test)]
    commit_pause: Option<SessionStoreCommitPause>,
    #[cfg(test)]
    fail_commit: bool,
    #[cfg(test)]
    fail_directory_sync: bool,
}

#[derive(Debug)]
#[must_use = "a renamed session bundle must be checked for directory durability"]
pub(crate) enum StagedSessionCommit {
    Durable,
    RenamedButNotSynced(SessionStoreError),
}

impl StagedSessionCommit {
    pub(crate) fn require_durable(self) -> Result<(), SessionStoreError> {
        match self {
            Self::Durable => Ok(()),
            Self::RenamedButNotSynced(error) => Err(error),
        }
    }
}

impl StagedSessionBundle {
    pub(crate) async fn commit(self) -> Result<StagedSessionCommit, SessionStoreError> {
        #[cfg(test)]
        if self.fail_commit {
            let error = io_error(
                self.final_path.clone(),
                std::io::Error::other("injected session store commit failure"),
            );
            let _ = tokio::fs::remove_file(&self.temp_path).await;
            return Err(error);
        }

        if let Err(source) = tokio::fs::rename(&self.temp_path, &self.final_path).await {
            let error = io_error(self.final_path.clone(), source);
            let _ = tokio::fs::remove_file(&self.temp_path).await;
            return Err(error);
        }

        #[cfg(test)]
        if let Some(pause) = &self.commit_pause {
            pause.pause_once().await;
        }

        #[cfg(test)]
        if self.fail_directory_sync {
            return Ok(StagedSessionCommit::RenamedButNotSynced(io_error(
                self.final_path
                    .parent()
                    .unwrap_or(self.final_path.as_path())
                    .to_path_buf(),
                std::io::Error::other("injected session directory sync failure"),
            )));
        }

        match sync_parent_directory(&self.final_path).await {
            Ok(()) => Ok(StagedSessionCommit::Durable),
            Err(error) => Ok(StagedSessionCommit::RenamedButNotSynced(error)),
        }
    }

    pub(crate) async fn discard(self) -> Result<(), SessionStoreError> {
        match tokio::fs::remove_file(&self.temp_path).await {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(io_error(self.temp_path, source)),
        }
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
        sessions_dir_from_env(
            env::var_os("XDG_STATE_HOME").as_deref(),
            env::var_os("HOME").as_deref(),
        )
    }

    pub fn default_store() -> Result<Self, SessionStoreError> {
        Ok(Self::new(Self::default_sessions_dir()?))
    }

    /// Creates a store rooted at a directory.
    ///
    /// Each session is stored below `<root>/<session_id>/state.json`.
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            sessions_dir: root.as_ref().to_path_buf(),
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

    /// Reserves exclusive write ownership of a session id until the returned
    /// guard is dropped.
    ///
    /// The lock is acquired before callers inspect or write `state.json`, so
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
    /// Saving is an atomic replace of `state.json`, so a new run that reuses an
    /// id would silently overwrite that session's transcript, ledger,
    /// artifacts, and checkpoints. Callers that start a session check this
    /// first and refuse the id instead of destroying resumable state.
    pub async fn contains_session(
        &self,
        session_id: &SessionId,
    ) -> Result<bool, SessionStoreError> {
        let path = self.state_path(session_id);
        tokio::fs::try_exists(&path)
            .await
            .map_err(|source| io_error(path, source))
    }

    pub(crate) fn session_dir(&self, session_id: &SessionId) -> PathBuf {
        self.sessions_dir.join(session_id.as_str())
    }

    pub(crate) fn state_path(&self, session_id: &SessionId) -> PathBuf {
        self.session_dir(session_id).join("state.json")
    }

    pub(crate) fn plan_overlay_path(&self, session_id: &SessionId) -> PathBuf {
        self.session_dir(session_id).join(PLAN_OVERLAY_FILE_NAME)
    }

    #[cfg(test)]
    pub(crate) fn artifacts_dir(&self, session_id: &SessionId) -> PathBuf {
        self.session_dir(session_id).join("artifacts")
    }

    #[cfg(test)]
    pub(crate) async fn write_state_bytes(
        &self,
        session_id: &SessionId,
        bytes: &[u8],
    ) -> Result<(), SessionStoreError> {
        self.stage_state_bytes(session_id, bytes)
            .await?
            .commit()
            .await?
            .require_durable()
    }

    pub(crate) async fn stage_state_bytes(
        &self,
        session_id: &SessionId,
        bytes: &[u8],
    ) -> Result<StagedSessionBundle, SessionStoreError> {
        self.stage_named_bytes(session_id, bytes, "state.json", TEMP_FILE_PREFIX)
            .await
    }

    pub(crate) async fn stage_plan_overlay_bytes(
        &self,
        session_id: &SessionId,
        bytes: &[u8],
    ) -> Result<StagedSessionBundle, SessionStoreError> {
        self.stage_named_bytes(
            session_id,
            bytes,
            PLAN_OVERLAY_FILE_NAME,
            PLAN_OVERLAY_TEMP_FILE_PREFIX,
        )
        .await
    }

    async fn stage_named_bytes(
        &self,
        session_id: &SessionId,
        bytes: &[u8],
        file_name: &str,
        temp_file_prefix: &str,
    ) -> Result<StagedSessionBundle, SessionStoreError> {
        let session_dir = self.session_dir(session_id);
        tokio::fs::create_dir_all(&session_dir)
            .await
            .map_err(|source| io_error(session_dir.clone(), source))?;

        let temp_path = write_temp_file(&session_dir, bytes, temp_file_prefix).await?;
        let final_path = session_dir.join(file_name);

        #[cfg(test)]
        if let Some(pause) = &self.stage_pause {
            pause.pause_once().await;
        }

        Ok(StagedSessionBundle {
            temp_path,
            final_path,
            #[cfg(test)]
            commit_pause: self.commit_pause.clone(),
            #[cfg(test)]
            fail_commit: self.fail_commit,
            #[cfg(test)]
            fail_directory_sync: self.fail_directory_sync,
        })
    }

    pub(crate) async fn read_state_bytes(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<u8>, SessionStoreError> {
        let path = self.state_path(session_id);
        tokio::fs::read(&path)
            .await
            .map_err(|source| io_error(path, source))
    }

    pub(crate) async fn read_plan_overlay_bytes(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<u8>>, SessionStoreError> {
        let path = self.plan_overlay_path(session_id);
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

    #[cfg(test)]
    pub(crate) fn staged_state_paths_for_tests(&self, session_id: &SessionId) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.session_dir(session_id)) else {
            return Vec::new();
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(TEMP_FILE_PREFIX)
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }
}

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

async fn write_temp_file(
    session_dir: &Path,
    bytes: &[u8],
    temp_file_prefix: &str,
) -> Result<PathBuf, SessionStoreError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..TEMP_FILE_CREATE_ATTEMPTS {
        let path = session_dir.join(format!(
            "{temp_file_prefix}{}-{nonce}-{attempt}",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error(path, source)),
        };
        let write_result = async {
            file.write_all(bytes).await?;
            file.sync_all().await
        }
        .await;
        drop(file);
        if let Err(source) = write_result {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(io_error(path, source));
        }
        return Ok(path);
    }

    let path = session_dir.join(format!("{TEMP_FILE_PREFIX}{}-{nonce}", std::process::id()));
    Err(io_error(
        path,
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique session state temp file",
        ),
    ))
}

fn io_error(path: PathBuf, source: std::io::Error) -> SessionStoreError {
    SessionStoreError::Io { path, source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn session_store_default_sessions_dir_uses_xdg_state_home_when_present() {
        let root = sessions_dir_from_env(
            Some(OsStr::new("/tmp/merry-state")),
            Some(OsStr::new("/home/test")),
        )
        .expect("xdg state root resolves");

        assert_eq!(
            root,
            PathBuf::from("/tmp/merry-state")
                .join("merry")
                .join("sessions")
        );
    }

    #[test]
    fn session_store_default_sessions_dir_falls_back_to_local_state_home() {
        let root = sessions_dir_from_env(None, Some(OsStr::new("/home/test")))
            .expect("home fallback resolves");

        assert_eq!(
            root,
            PathBuf::from("/home/test").join(".local/state/merry/sessions")
        );
    }

    #[tokio::test]
    async fn session_store_writes_state_json_with_atomic_replace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-store-test").expect("valid session id");

        store
            .write_state_bytes(&session_id, br#"{"format_version":1}"#)
            .await
            .expect("state write succeeds");
        store
            .write_state_bytes(&session_id, br#"{"format_version":1,"second":true}"#)
            .await
            .expect("state rewrite succeeds");

        let bytes = store
            .read_state_bytes(&session_id)
            .await
            .expect("state reads");
        assert_eq!(bytes, br#"{"format_version":1,"second":true}"#);
        assert!(store.state_path(&session_id).ends_with("state.json"));
    }

    #[tokio::test]
    async fn staged_state_bytes_do_not_replace_committed_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-store-stage").expect("valid session id");
        let old_bytes = br#"{"format_version":1,"value":"old"}"#;
        let new_bytes = br#"{"format_version":1,"value":"new"}"#;
        store
            .write_state_bytes(&session_id, old_bytes)
            .await
            .expect("initial state write succeeds");

        let staged = store
            .stage_state_bytes(&session_id, new_bytes)
            .await
            .expect("state staging succeeds");

        assert_eq!(
            store
                .read_state_bytes(&session_id)
                .await
                .expect("committed state reads"),
            old_bytes
        );
        assert!(staged.temp_path.exists());
        staged.discard().await.expect("staged state discards");
    }

    #[tokio::test]
    async fn discarding_staged_state_keeps_committed_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-store-discard").expect("valid session id");
        let old_bytes = br#"{"format_version":1,"value":"old"}"#;
        store
            .write_state_bytes(&session_id, old_bytes)
            .await
            .expect("initial state write succeeds");
        let staged = store
            .stage_state_bytes(&session_id, br#"{"format_version":1,"value":"discarded"}"#)
            .await
            .expect("state staging succeeds");
        let temp_path = staged.temp_path.clone();

        staged.discard().await.expect("staged state discards");

        assert_eq!(
            store
                .read_state_bytes(&session_id)
                .await
                .expect("committed state reads"),
            old_bytes
        );
        assert!(!temp_path.exists());
    }

    #[tokio::test]
    async fn committing_staged_state_replaces_committed_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-store-commit").expect("valid session id");
        let new_bytes = br#"{"format_version":1,"value":"committed"}"#;
        store
            .write_state_bytes(&session_id, br#"{"format_version":1,"value":"old"}"#)
            .await
            .expect("initial state write succeeds");
        let staged = store
            .stage_state_bytes(&session_id, new_bytes)
            .await
            .expect("state staging succeeds");
        let temp_path = staged.temp_path.clone();

        staged
            .commit()
            .await
            .expect("staged state renames")
            .require_durable()
            .expect("committed state is durable");

        assert_eq!(
            store
                .read_state_bytes(&session_id)
                .await
                .expect("committed state reads"),
            new_bytes
        );
        assert!(!temp_path.exists());
    }

    #[tokio::test]
    async fn independent_stages_for_one_session_do_not_share_temp_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store_a = FileSessionStore::new(temp.path());
        let store_b = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-store-independent-stages").expect("valid id");
        let bytes_a = br#"{"format_version":1,"writer":"a"}"#;
        let bytes_b = br#"{"format_version":1,"writer":"b"}"#;

        let staged_a = store_a
            .stage_state_bytes(&session_id, bytes_a)
            .await
            .expect("first state stages");
        let staged_b = store_b
            .stage_state_bytes(&session_id, bytes_b)
            .await
            .expect("second state stages");

        assert_ne!(staged_a.temp_path, staged_b.temp_path);
        staged_a
            .commit()
            .await
            .expect("first state renames")
            .require_durable()
            .expect("first state is durable");
        assert_eq!(
            store_a
                .read_state_bytes(&session_id)
                .await
                .expect("first committed state reads"),
            bytes_a
        );
        staged_b
            .commit()
            .await
            .expect("second state renames")
            .require_durable()
            .expect("second state is durable");
        assert_eq!(
            store_b
                .read_state_bytes(&session_id)
                .await
                .expect("second committed state reads"),
            bytes_b
        );
    }

    #[tokio::test]
    async fn injected_commit_failure_keeps_committed_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-store-commit-failure").expect("valid session id");
        let old_bytes = br#"{"format_version":1,"value":"old"}"#;
        store
            .write_state_bytes(&session_id, old_bytes)
            .await
            .expect("initial state write succeeds");
        let failing_store = store.clone().with_commit_failure_for_tests();
        let staged = failing_store
            .stage_state_bytes(&session_id, br#"{"format_version":1,"value":"new"}"#)
            .await
            .expect("state staging succeeds");

        staged
            .commit()
            .await
            .expect_err("injected commit failure must fail before rename");

        assert_eq!(
            store
                .read_state_bytes(&session_id)
                .await
                .expect("committed state reads"),
            old_bytes
        );
        assert!(store.staged_state_paths_for_tests(&session_id).is_empty());
    }

    #[tokio::test]
    async fn stage_pause_blocks_after_sync_and_only_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pause = SessionStoreStagePause::new();
        let store = FileSessionStore::new(temp.path()).with_stage_pause_for_tests(pause.clone());
        let session_id = SessionId::new("session-store-pause").expect("valid session id");
        let staged_store = store.clone();
        let staged_session_id = session_id.clone();
        let staging = tokio::spawn(async move {
            staged_store
                .stage_state_bytes(
                    &staged_session_id,
                    br#"{"format_version":1,"value":"first"}"#,
                )
                .await
        });

        pause.wait_until_staged().await;
        assert!(
            store.staged_state_paths_for_tests(&session_id).len() == 1,
            "pause must become observable after the temporary file is synced"
        );
        assert!(!store.state_path(&session_id).exists());
        pause.resume();
        staging
            .await
            .expect("staging task joins")
            .expect("first state staging succeeds")
            .discard()
            .await
            .expect("first staged state discards");

        store
            .stage_state_bytes(&session_id, br#"{"format_version":1,"value":"second"}"#)
            .await
            .expect("pause is consumed after the first stage")
            .discard()
            .await
            .expect("second staged state discards");
    }

    #[tokio::test]
    async fn contains_session_reports_only_committed_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-store-collision").expect("valid session id");

        assert!(
            !store
                .contains_session(&session_id)
                .await
                .expect("an empty store should be readable"),
            "an unused session id has no saved state"
        );

        let staged = store
            .stage_state_bytes(&session_id, br#"{"format_version":1,"value":"staged"}"#)
            .await
            .expect("state should stage");
        assert!(
            !store
                .contains_session(&session_id)
                .await
                .expect("a staged store should be readable"),
            "staged state is not yet a session another run could destroy"
        );

        staged
            .commit()
            .await
            .expect("staged state should commit")
            .require_durable()
            .expect("commit should be durable");
        assert!(
            store
                .contains_session(&session_id)
                .await
                .expect("a committed store should be readable"),
            "committed state must be reported so a new run cannot replace it"
        );
    }

    #[tokio::test]
    async fn session_reservation_is_exclusive_and_releases_on_drop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-reservation").expect("valid session id");
        let first = store
            .reserve_session(&session_id)
            .await
            .expect("first reservation succeeds");
        assert!(matches!(
            store.reserve_session(&session_id).await,
            Err(SessionStoreError::SessionAlreadyReserved { .. })
        ));
        drop(first);
        store
            .reserve_session(&session_id)
            .await
            .expect("dropping the reservation releases it");
    }

    #[tokio::test]
    async fn concurrent_session_reservations_have_one_winner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("session-reservation-race").expect("valid session id");
        let (left, right) = tokio::join!(
            store.reserve_session(&session_id),
            store.reserve_session(&session_id)
        );

        assert_ne!(left.is_ok(), right.is_ok());
        assert!(matches!(
            left.as_ref().err().or_else(|| right.as_ref().err()),
            Some(SessionStoreError::SessionAlreadyReserved { .. })
        ));
    }
}
