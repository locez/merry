//! Rebuildable full-text index beside session directories.
//!
//! The index is not resume state. A corrupt or outdated database is deleted
//! and rebuilt from `journal.jsonl` plus optional `meta.json` labels. Transient
//! lock contention (`SQLITE_BUSY` / `SQLITE_LOCKED`) never deletes the database.

use super::{FileSessionStore, JOURNAL_FILE_NAME};
use merry_core::SessionId;
use rusqlite::{Connection, ErrorCode, params};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

const INDEX_FILE_NAME: &str = "session_search.sqlite";
const SCHEMA_VERSION: i64 = 3;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const SCHEMA: &str = "
CREATE TABLE session_search (
    session_id TEXT PRIMARY KEY,
    topic TEXT NOT NULL DEFAULT '',
    user_text TEXT NOT NULL DEFAULT ''
);
CREATE VIRTUAL TABLE session_fts USING fts5(
    topic,
    user_text,
    content='session_search',
    content_rowid='rowid'
);
CREATE TRIGGER session_search_ai AFTER INSERT ON session_search BEGIN
    INSERT INTO session_fts(rowid, topic, user_text)
    VALUES (new.rowid, new.topic, new.user_text);
END;
CREATE TRIGGER session_search_ad AFTER DELETE ON session_search BEGIN
    INSERT INTO session_fts(session_fts, rowid, topic, user_text)
    VALUES ('delete', old.rowid, old.topic, old.user_text);
END;
CREATE TRIGGER session_search_au AFTER UPDATE ON session_search BEGIN
    INSERT INTO session_fts(session_fts, rowid, topic, user_text)
    VALUES ('delete', old.rowid, old.topic, old.user_text);
    INSERT INTO session_fts(rowid, topic, user_text)
    VALUES (new.rowid, new.topic, new.user_text);
END;
";

#[derive(Debug, Error)]
enum SearchIndexError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("blocking task join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl SearchIndexError {
    fn is_corruption(&self) -> bool {
        matches!(
            self,
            Self::Sqlite(rusqlite::Error::SqliteFailure(err, _))
                if matches!(err.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
        )
    }
}

#[derive(Debug, Clone)]
pub(super) struct SessionSearchUpdate {
    pub(super) sessions_dir: PathBuf,
    pub(super) session_id: SessionId,
    pub(super) user_text: String,
}

#[derive(Debug, Deserialize)]
struct MetaLabels {
    #[serde(default)]
    topic: Option<String>,
}

impl FileSessionStore {
    fn search_index_path(&self) -> PathBuf {
        self.sessions_dir.join(INDEX_FILE_NAME)
    }

    /// Returns session ids whose topic or user text matches `query`.
    ///
    /// An empty or unusable query returns no rows. Index failures are logged
    /// and also return no rows; they do not affect resume.
    pub async fn search_session_ids(&self, query: &str) -> Vec<SessionId> {
        let Some(fts_query) = fts_query(query) else {
            return Vec::new();
        };
        let path = self.search_index_path();
        let sessions_dir = self.sessions_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            with_recoverable_index(&path, &sessions_dir, |connection| {
                search_ids(connection, &fts_query)
            })
        })
        .await;
        match flatten_join(result) {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(error = %error, "session search failed");
                Vec::new()
            }
        }
    }

    /// Updates list labels without replacing indexed user text.
    pub async fn update_session_search_labels(&self, session_id: &SessionId, topic: Option<&str>) {
        let path = self.search_index_path();
        let sessions_dir = self.sessions_dir.clone();
        let session_id = session_id.as_str().to_owned();
        let topic = indexable_text(topic.unwrap_or(""));
        let result = tokio::task::spawn_blocking(move || {
            with_recoverable_index(&path, &sessions_dir, |connection| {
                connection.execute(
                    "INSERT INTO session_search (session_id, topic, user_text)
                     VALUES (?1, ?2, '')
                     ON CONFLICT(session_id) DO UPDATE SET
                        topic = excluded.topic",
                    params![session_id, topic],
                )?;
                Ok(())
            })
        })
        .await;
        if let Err(error) = flatten_join(result) {
            tracing::warn!(error = %error, "session search label update failed");
        }
    }
}

pub(super) async fn upsert_user_text(update: SessionSearchUpdate) {
    let path = update.sessions_dir.join(INDEX_FILE_NAME);
    let user_text = indexable_text(&update.user_text);
    let result = tokio::task::spawn_blocking(move || {
        with_recoverable_index(&path, &update.sessions_dir, |connection| {
            connection.execute(
                "INSERT INTO session_search (session_id, topic, user_text)
                 VALUES (?1, '', ?2)
                 ON CONFLICT(session_id) DO UPDATE SET user_text = excluded.user_text",
                params![update.session_id.as_str(), user_text],
            )?;
            Ok(())
        })
    })
    .await;
    if let Err(error) = flatten_join(result) {
        tracing::warn!(error = %error, "session search index update failed");
    }
}

fn flatten_join<T>(
    result: Result<Result<T, SearchIndexError>, tokio::task::JoinError>,
) -> Result<T, SearchIndexError> {
    match result {
        Ok(inner) => inner,
        Err(error) => Err(SearchIndexError::Join(error)),
    }
}

/// Runs `op` against the search index, rebuilding once if SQLite reports
/// genuine database corruption. Transient errors (such as `SQLITE_BUSY`) are
/// returned directly without deleting the database.
fn with_recoverable_index<T>(
    path: &Path,
    sessions_dir: &Path,
    mut op: impl FnMut(&mut Connection) -> Result<T, SearchIndexError>,
) -> Result<T, SearchIndexError> {
    match open_or_create_index(path, sessions_dir).and_then(|mut connection| op(&mut connection)) {
        Ok(value) => Ok(value),
        Err(error) if error.is_corruption() => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "rebuilding corrupt session search index"
            );
            let mut connection = reset_and_rebuild_index(path, sessions_dir)?;
            op(&mut connection)
        }
        Err(error) => Err(error),
    }
}

fn search_ids(
    connection: &mut Connection,
    fts_query: &str,
) -> Result<Vec<SessionId>, SearchIndexError> {
    let mut statement = connection.prepare(
        "SELECT session_search.session_id
         FROM session_fts
         JOIN session_search ON session_search.rowid = session_fts.rowid
         WHERE session_fts MATCH ?1
         ORDER BY rank",
    )?;
    let rows = statement.query_map(params![fts_query], |row| row.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        let session_id = row?;
        if let Ok(session_id) = SessionId::new(&session_id) {
            ids.push(session_id);
        }
    }
    Ok(ids)
}

fn open_or_create_index(path: &Path, sessions_dir: &Path) -> Result<Connection, SearchIndexError> {
    if !path.exists() {
        return reset_and_rebuild_index(path, sessions_dir);
    }
    let connection = Connection::open(path)?;
    configure(&connection)?;
    let version = schema_version(&connection)?;
    if version == SCHEMA_VERSION {
        return Ok(connection);
    }
    drop(connection);
    reset_and_rebuild_index(path, sessions_dir)
}

fn reset_and_rebuild_index(
    path: &Path,
    sessions_dir: &Path,
) -> Result<Connection, SearchIndexError> {
    if path.exists() {
        remove_index_files(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut connection = Connection::open(path)?;
    configure(&connection)?;
    connection.execute_batch(SCHEMA)?;
    rebuild(&mut connection, sessions_dir)?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(connection)
}

fn schema_version(connection: &Connection) -> Result<i64, SearchIndexError> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn configure(connection: &Connection) -> Result<(), SearchIndexError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    Ok(())
}

fn rebuild(connection: &mut Connection, sessions_dir: &Path) -> Result<(), SearchIndexError> {
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(SearchIndexError::Io(error)),
    };
    let tx = connection.transaction()?;
    tx.execute("DELETE FROM session_search", [])?;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(session_id) = SessionId::new(&name) else {
            continue;
        };
        let directory = entry.path();
        let journal_path = directory.join(JOURNAL_FILE_NAME);
        if !journal_path.is_file() {
            continue;
        }
        let bytes = match std::fs::read(&journal_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    path = %journal_path.display(),
                    error = %error,
                    "session search skipped a journal"
                );
                continue;
            }
        };
        let snapshot = match crate::session::extract_search_snapshot(&bytes) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(
                    path = %journal_path.display(),
                    error = %error,
                    "session search skipped a journal"
                );
                continue;
            }
        };
        let topic = meta_topic(&directory.join("meta.json"))
            .or(snapshot.topic)
            .unwrap_or_default();
        tx.execute(
            "INSERT INTO session_search (session_id, topic, user_text)
             VALUES (?1, ?2, ?3)",
            params![
                session_id.as_str(),
                indexable_text(&topic),
                indexable_text(&snapshot.user_text)
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn meta_topic(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<MetaLabels>(&bytes)
        .ok()
        .and_then(|labels| labels.topic)
}

fn remove_index_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
}

/// Inserts spaces around CJK characters so FTS5's `unicode61` tokenizer indexes
/// each ideograph at its position while keeping Latin/ASCII runs intact.
fn indexable_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut prev_cjk = false;
    for ch in raw.chars() {
        let cjk = is_cjk(ch);
        if (cjk || prev_cjk) && !out.is_empty() && !out.ends_with(' ') && !ch.is_whitespace() {
            out.push(' ');
        }
        out.push(ch);
        prev_cjk = cjk;
    }
    out
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30FF // Hiragana & Katakana
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7AF // Hangul Syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x2A6DF // CJK Extension B
    )
}

/// Builds an FTS5 query matching all non-punctuation runs in `raw`.
///
/// Adjacent CJK runs become exact FTS5 phrase matches (e.g. `落盘` -> `"落 盘"`),
/// while Latin/numeric tokens stay single-token matches.
fn fts_query(raw: &str) -> Option<String> {
    let mut clauses = Vec::new();
    for segment in raw.split(|ch: char| !ch.is_alphanumeric()) {
        if segment.is_empty() {
            continue;
        }
        let mut current_ascii = String::new();
        let mut current_cjk = Vec::new();
        for ch in segment.chars() {
            if is_cjk(ch) {
                if !current_ascii.is_empty() {
                    clauses.push(format!("\"{}\"", current_ascii.replace('"', "")));
                    current_ascii.clear();
                }
                current_cjk.push(ch.to_string());
            } else {
                if !current_cjk.is_empty() {
                    clauses.push(format!("\"{}\"", current_cjk.join(" ")));
                    current_cjk.clear();
                }
                if ch != '"' {
                    current_ascii.push(ch);
                }
            }
        }
        if !current_ascii.is_empty() {
            clauses.push(format!("\"{current_ascii}\""));
        }
        if !current_cjk.is_empty() {
            clauses.push(format!("\"{}\"", current_cjk.join(" ")));
        }
    }
    if clauses.is_empty() {
        None
    } else {
        Some(clauses.join(" AND "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionState;

    #[tokio::test]
    async fn search_index_finds_english_and_cjk_and_rebuilds_topic_from_journal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("search-session").expect("valid session id");
        let mut session = SessionState::new(session_id.clone());
        let turn_id = session.begin_model_turn().expect("turn begins");
        session
            .record_user_message_body(turn_id, "alpha unique-token 优化落盘策略")
            .expect("user message records");
        session
            .close_model_response(turn_id, false)
            .expect("turn closes");
        session.set_generated_topic("topic-token 日志优化".to_owned());
        session.save_to(&store).await.expect("session saves");
        store
            .update_session_search_labels(&session_id, Some("topic-token 日志优化"))
            .await;

        assert_eq!(
            store.search_session_ids("unique-token").await,
            vec![session_id.clone()]
        );
        assert_eq!(
            store.search_session_ids("落盘").await,
            vec![session_id.clone()]
        );
        assert_eq!(
            store.search_session_ids("topic-token").await,
            vec![session_id.clone()]
        );
        assert_eq!(
            store.search_session_ids("日志优化").await,
            vec![session_id.clone()]
        );
        assert!(store.search_session_ids("策略优化").await.is_empty());
        assert!(store.search_session_ids("missing-token").await.is_empty());

        for entry in std::fs::read_dir(temp.path()).expect("sessions dir reads") {
            let path = entry.expect("entry").path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with(INDEX_FILE_NAME) {
                std::fs::remove_file(path).expect("index file removes");
            }
        }

        // Rebuilding without meta.json still recovers both user text and journal topic.
        assert_eq!(
            store.search_session_ids("unique-token").await,
            vec![session_id.clone()]
        );
        assert_eq!(
            store.search_session_ids("落盘").await,
            vec![session_id.clone()]
        );
        assert_eq!(
            store.search_session_ids("topic-token").await,
            vec![session_id]
        );
    }

    #[tokio::test]
    async fn search_index_recovers_from_corrupt_sqlite_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let session_id = SessionId::new("corrupt-search-session").expect("valid session id");
        let mut session = SessionState::new(session_id.clone());
        let turn_id = session.begin_model_turn().expect("turn begins");
        session
            .record_user_message_body(turn_id, "recover-me")
            .expect("user message records");
        session
            .close_model_response(turn_id, false)
            .expect("turn closes");
        session.save_to(&store).await.expect("session saves");

        std::fs::write(temp.path().join(INDEX_FILE_NAME), b"not a sqlite database")
            .expect("corrupt index writes");

        assert_eq!(
            store.search_session_ids("recover-me").await,
            vec![session_id]
        );
    }

    #[test]
    fn fts_query_quotes_latin_and_cjk_phrases() {
        assert_eq!(
            fts_query("  plans: alpha, beta! ").as_deref(),
            Some("\"plans\" AND \"alpha\" AND \"beta\"")
        );
        assert_eq!(
            fts_query("优化Rust落盘").as_deref(),
            Some("\"优 化\" AND \"Rust\" AND \"落 盘\"")
        );
        assert_eq!(fts_query("..."), None);
    }
}
