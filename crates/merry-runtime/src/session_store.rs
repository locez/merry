use merry_core::{ArtifactId, SessionId};
use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

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
    #[error(
        "session {session_id} uses legacy compacted history whose covered source transcript was physically deleted and cannot satisfy exact evidence recovery"
    )]
    LegacyCompactedHistoryUnavailable { session_id: SessionId },
    #[error("legacy user message migration collides with existing artifact id {artifact_id}")]
    LegacyUserArtifactCollision { artifact_id: ArtifactId },
    #[error("session document id {actual} does not match requested session {requested}")]
    SessionIdMismatch {
        requested: SessionId,
        actual: SessionId,
    },
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

#[derive(Debug, Clone)]
pub struct FileSessionStore {
    sessions_dir: PathBuf,
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

    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            sessions_dir: root.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    pub(crate) fn session_dir(&self, session_id: &SessionId) -> PathBuf {
        self.sessions_dir.join(session_id.as_str())
    }

    pub(crate) fn state_path(&self, session_id: &SessionId) -> PathBuf {
        self.session_dir(session_id).join("state.json")
    }

    #[cfg(test)]
    pub(crate) fn artifacts_dir(&self, session_id: &SessionId) -> PathBuf {
        self.session_dir(session_id).join("artifacts")
    }

    pub(crate) async fn write_state_bytes(
        &self,
        session_id: &SessionId,
        bytes: &[u8],
    ) -> Result<(), SessionStoreError> {
        let session_dir = self.session_dir(session_id);
        tokio::fs::create_dir_all(&session_dir)
            .await
            .map_err(|source| io_error(session_dir.clone(), source))?;

        let temp_path = session_dir.join(".state.json.tmp");
        let final_path = session_dir.join("state.json");
        write_temp_file(&temp_path, bytes).await?;
        tokio::fs::rename(&temp_path, &final_path)
            .await
            .map_err(|source| io_error(final_path, source))?;
        Ok(())
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

async fn write_temp_file(path: &Path, bytes: &[u8]) -> Result<(), SessionStoreError> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|source| io_error(path.to_path_buf(), source))?;
    file.write_all(bytes)
        .await
        .map_err(|source| io_error(path.to_path_buf(), source))?;
    file.sync_all()
        .await
        .map_err(|source| io_error(path.to_path_buf(), source))?;
    drop(file);
    Ok(())
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
}
