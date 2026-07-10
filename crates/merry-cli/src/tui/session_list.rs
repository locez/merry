use merry_core::SessionId;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

const META_FILE: &str = "meta.json";

#[derive(Debug, Error)]
pub(crate) enum TuiSessionListError {
    #[error("session metadata IO error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("session metadata JSON error at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiSessionMetadata {
    pub(crate) session_id: SessionId,
    pub(crate) workspace_root: PathBuf,
    pub(crate) created_at_unix_ms: u128,
    pub(crate) last_active_at_unix_ms: u128,
    pub(crate) title: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<String>,
}

impl TuiSessionMetadata {
    pub(crate) fn new(session_id: SessionId, workspace_root: PathBuf, now_unix_ms: u128) -> Self {
        Self {
            session_id,
            workspace_root,
            created_at_unix_ms: now_unix_ms,
            last_active_at_unix_ms: now_unix_ms,
            title: None,
            model: None,
            reasoning_effort: None,
        }
    }

    pub(crate) fn mark_active(&mut self, now_unix_ms: u128) {
        self.last_active_at_unix_ms = now_unix_ms;
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TuiSessionStore {
    sessions_dir: PathBuf,
}

impl TuiSessionStore {
    pub(crate) fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    pub(crate) fn session_state_store(&self) -> merry_runtime::FileSessionStore {
        merry_runtime::FileSessionStore::new(&self.sessions_dir)
    }

    pub(crate) fn metadata_path(&self, session_id: &SessionId) -> PathBuf {
        self.sessions_dir.join(session_id.as_str()).join(META_FILE)
    }

    pub(crate) fn write_metadata(
        &self,
        metadata: &TuiSessionMetadata,
    ) -> Result<(), TuiSessionListError> {
        let path = self.metadata_path(&metadata.session_id);
        let dir = path
            .parent()
            .expect("metadata path should always have a parent")
            .to_path_buf();
        fs::create_dir_all(&dir).map_err(|source| io_error(dir, source))?;
        let bytes =
            serde_json::to_vec_pretty(metadata).map_err(|source| json_error(&path, source))?;
        fs::write(&path, bytes).map_err(|source| io_error(path, source))
    }

    pub(crate) fn sessions_for_workspace(
        &self,
        workspace_root: &Path,
    ) -> Result<Vec<TuiSessionMetadata>, TuiSessionListError> {
        let mut sessions = Vec::new();
        let entries = match fs::read_dir(&self.sessions_dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(sessions),
            Err(source) => return Err(io_error(self.sessions_dir.clone(), source)),
        };

        for entry in entries {
            let entry = entry.map_err(|source| io_error(self.sessions_dir.clone(), source))?;
            if !entry.path().join("state.json").is_file() {
                continue;
            }
            let meta_path = entry.path().join(META_FILE);
            let metadata = match fs::read(&meta_path) {
                Ok(bytes) => serde_json::from_slice::<TuiSessionMetadata>(&bytes)
                    .map_err(|source| json_error(&meta_path, source))?,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(io_error(meta_path, source)),
            };
            if metadata.workspace_root == workspace_root {
                sessions.push(metadata);
            }
        }

        sessions.sort_by(|left, right| {
            right
                .last_active_at_unix_ms
                .cmp(&left.last_active_at_unix_ms)
                .then_with(|| right.created_at_unix_ms.cmp(&left.created_at_unix_ms))
                .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
        });
        Ok(sessions)
    }
}

pub(crate) fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn io_error(path: PathBuf, source: io::Error) -> TuiSessionListError {
    TuiSessionListError::Io { path, source }
}

fn json_error(path: &Path, source: serde_json::Error) -> TuiSessionListError {
    TuiSessionListError::Json {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_id(value: &str) -> SessionId {
        SessionId::new(value).expect("valid session id")
    }

    fn write_state_placeholder(store: &TuiSessionStore, session_id: &SessionId) {
        let path = store
            .session_state_store()
            .sessions_dir()
            .join(session_id.as_str())
            .join("state.json");
        fs::create_dir_all(path.parent().expect("state parent")).expect("state parent exists");
        fs::write(path, "{}").expect("state placeholder writes");
    }

    #[test]
    fn session_store_lists_current_workspace_sessions_by_last_active_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = TuiSessionStore::new(temp.path().to_path_buf());
        let workspace = PathBuf::from("/repo");
        let other_workspace = PathBuf::from("/other");
        let old_session = session_id("old-session");
        write_state_placeholder(&store, &old_session);
        store
            .write_metadata(&TuiSessionMetadata::new(old_session, workspace.clone(), 10))
            .expect("old metadata writes");
        let other_session = session_id("other-workspace");
        write_state_placeholder(&store, &other_session);
        store
            .write_metadata(&TuiSessionMetadata::new(other_session, other_workspace, 50))
            .expect("other metadata writes");
        let recent_session = session_id("recent-session");
        write_state_placeholder(&store, &recent_session);
        let mut recent = TuiSessionMetadata::new(recent_session, workspace.clone(), 20);
        recent.mark_active(80);
        store
            .write_metadata(&recent)
            .expect("recent metadata writes");

        let sessions = store
            .sessions_for_workspace(&workspace)
            .expect("sessions list");

        assert_eq!(
            sessions
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["recent-session", "old-session"]
        );
    }

    #[test]
    fn session_store_skips_metadata_without_state_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = TuiSessionStore::new(temp.path().to_path_buf());
        let workspace = PathBuf::from("/repo");
        store
            .write_metadata(&TuiSessionMetadata::new(
                session_id("orphan-session"),
                workspace.clone(),
                10,
            ))
            .expect("orphan metadata writes");

        let sessions = store
            .sessions_for_workspace(&workspace)
            .expect("sessions list");

        assert!(sessions.is_empty());
    }
}
