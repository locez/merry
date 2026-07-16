use super::input::{normalize_input_history, push_input_history_entry};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const HISTORY_HASH_DOMAIN: &[u8] = b"merry-input-history-v1\0";
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputHistoryStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl InputHistoryStore {
    pub(crate) fn for_workspace(state_dir: &Path, workspace_root: &Path) -> Self {
        let hash = workspace_hash(workspace_root);
        let directory = state_dir.join("input-history");
        Self {
            path: directory.join(format!("{hash}.jsonl")),
            lock_path: directory.join(format!("{hash}.lock")),
        }
    }

    pub(crate) async fn load(&self) -> Vec<String> {
        let path = self.path.clone();
        match tokio::task::spawn_blocking(move || load_history(&path)).await {
            Ok(Ok(entries)) => entries,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "could not load TUI input history");
                Vec::new()
            }
            Err(error) => {
                tracing::warn!(error = %error, "TUI input history load task failed");
                Vec::new()
            }
        }
    }

    pub(crate) async fn record(&self, text: &str) -> Result<Vec<String>, InputHistoryError> {
        if text.trim().is_empty() {
            return Ok(self.load().await);
        }
        let path = self.path.clone();
        let lock_path = self.lock_path.clone();
        let text = text.to_owned();
        tokio::task::spawn_blocking(move || record_history(&path, &lock_path, &text))
            .await
            .map_err(InputHistoryError::TaskJoin)?
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

fn workspace_hash(workspace_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(HISTORY_HASH_DOMAIN);
    hasher.update(workspace_root.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(32);
    for byte in &digest[..16] {
        write!(&mut hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    hash
}

fn record_history(
    path: &Path,
    lock_path: &Path,
    text: &str,
) -> Result<Vec<String>, InputHistoryError> {
    let parent = path
        .parent()
        .ok_or_else(|| InputHistoryError::InvalidPath {
            path: path.to_path_buf(),
        })?;
    ensure_private_directory(parent)?;
    let lock_file = open_private_lock_file(lock_path)?;
    lock_file
        .lock()
        .map_err(|source| io_error("lock", lock_path, source))?;

    let mut entries = load_history(path)?;
    push_input_history_entry(&mut entries, text);
    write_history_atomically(path, &entries)?;
    Ok(entries)
}

fn load_history(path: &Path) -> Result<Vec<String>, InputHistoryError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error("read", path, source)),
    };
    let mut entries = Vec::new();
    let mut skipped_lines = 0_usize;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(|source| io_error("read", path, source))?
            == 0
        {
            break;
        }
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        match serde_json::from_slice::<String>(&line) {
            Ok(entry) if !entry.trim().is_empty() => entries.push(entry),
            Ok(_) | Err(_) => skipped_lines += 1,
        }
    }
    if skipped_lines > 0 {
        tracing::warn!(
            path = %path.display(),
            skipped_lines,
            "skipped invalid TUI input history lines"
        );
    }
    Ok(normalize_input_history(entries))
}

fn ensure_private_directory(path: &Path) -> Result<(), InputHistoryError> {
    fs::create_dir_all(path).map_err(|source| io_error("create directory", path, source))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error("set directory permissions", path, source))?;
    Ok(())
}

fn open_private_lock_file(path: &Path) -> Result<File, InputHistoryError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(path)
        .map_err(|source| io_error("open lock file", path, source))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error("set lock file permissions", path, source))?;
    Ok(file)
}

fn write_history_atomically(path: &Path, entries: &[String]) -> Result<(), InputHistoryError> {
    let parent = path
        .parent()
        .ok_or_else(|| InputHistoryError::InvalidPath {
            path: path.to_path_buf(),
        })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| InputHistoryError::InvalidPath {
            path: path.to_path_buf(),
        })?;
    let temp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temp_path)
            .map_err(|source| io_error("create temporary file", &temp_path, source))?;
        for entry in entries {
            serde_json::to_writer(&mut file, entry).map_err(InputHistoryError::Serialize)?;
            file.write_all(b"\n")
                .map_err(|source| io_error("write temporary file", &temp_path, source))?;
        }
        file.sync_all()
            .map_err(|source| io_error("sync temporary file", &temp_path, source))?;
        drop(file);
        fs::rename(&temp_path, path).map_err(|source| io_error("replace", path, source))?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error("sync directory", parent, source))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> InputHistoryError {
    InputHistoryError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Error)]
pub(crate) enum InputHistoryError {
    #[error("could not {operation} input history path {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("input history path {} is invalid", path.display())]
    InvalidPath { path: PathBuf },
    #[error("could not serialize input history: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("input history persistence task failed: {0}")]
    TaskJoin(#[source] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn history_store(temp: &tempfile::TempDir, workspace: &str) -> InputHistoryStore {
        InputHistoryStore::for_workspace(&temp.path().join("merry"), Path::new(workspace))
    }

    fn write_fixture(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("history parent")).expect("create parent");
        fs::write(path, text).expect("write history fixture");
    }

    #[tokio::test]
    async fn round_trips_multiline_unicode_entries_and_loads_empty_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = history_store(&temp, "/workspace/merry");
        for entry in ["first", "second\nline", "第三条"] {
            store.record(entry).await.expect("record history");
        }
        assert_eq!(store.load().await, vec!["first", "second\nline", "第三条"]);

        let empty = history_store(&temp, "/workspace/empty");
        write_fixture(empty.path(), "");
        assert!(empty.load().await.is_empty());
    }

    #[tokio::test]
    async fn salvages_valid_lines_from_partially_or_fully_corrupt_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let partial = history_store(&temp, "/workspace/partial");
        write_fixture(partial.path(), "\"first\"\nnot-json\n42\n\"second\"\n");
        assert_eq!(partial.load().await, vec!["first", "second"]);

        let invalid_utf8 = history_store(&temp, "/workspace/invalid-utf8");
        fs::create_dir_all(invalid_utf8.path().parent().expect("history parent"))
            .expect("create parent");
        fs::write(invalid_utf8.path(), b"\"first\"\n\xff\n\"second\"\n")
            .expect("write invalid UTF-8 fixture");
        assert_eq!(invalid_utf8.load().await, vec!["first", "second"]);

        let corrupt = history_store(&temp, "/workspace/corrupt");
        write_fixture(corrupt.path(), "not-json\n{}\n");
        assert!(corrupt.load().await.is_empty());
    }

    #[tokio::test]
    async fn scopes_paths_by_workspace_without_leaking_the_raw_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = history_store(&temp, "/private/workspace/first");
        let second = history_store(&temp, "/private/workspace/second");
        assert_ne!(first.path(), second.path());
        assert_eq!(
            first.path().parent(),
            Some(temp.path().join("merry/input-history").as_path())
        );
        let file_name = first
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 hash file name");
        assert_eq!(file_name.len(), 32 + ".jsonl".len());
        assert!(file_name.ends_with(".jsonl"));
        assert!(!first.path().display().to_string().contains("private"));
        assert_eq!(
            history_store(&temp, "/workspace/merry")
                .path()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("d4d8319782c356e98a4473aa3c7cc170.jsonl")
        );

        first.record("first only").await.expect("first record");
        second.record("second only").await.expect("second record");
        assert_eq!(first.load().await, vec!["first only"]);
        assert_eq!(second.load().await, vec!["second only"]);
    }

    #[tokio::test]
    async fn normalizes_duplicates_blanks_and_capacity_on_load_and_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = history_store(&temp, "/workspace/merry");
        let fixture = (0..=200)
            .map(|index| serde_json::to_string(&format!("entry-{index}")))
            .collect::<Result<Vec<_>, _>>()
            .expect("serialize fixture")
            .join("\n");
        write_fixture(store.path(), &fixture);
        let loaded = store.load().await;
        assert_eq!(loaded.len(), 200);
        assert_eq!(loaded.first().map(String::as_str), Some("entry-1"));

        store
            .record("entry-200")
            .await
            .expect("deduplicated record");
        store.record("   ").await.expect("blank record");
        assert_eq!(store.load().await, loaded);
        store
            .record("entry-1")
            .await
            .expect("non-adjacent duplicate");
        let loaded = store.load().await;
        assert_eq!(loaded.last().map(String::as_str), Some("entry-1"));
    }

    #[tokio::test]
    async fn concurrent_stores_merge_records_under_the_workspace_lock() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = history_store(&temp, "/workspace/merry");
        let second = first.clone();
        let (first_result, second_result) =
            tokio::join!(first.record("from first"), second.record("from second"));
        first_result.expect("first record");
        second_result.expect("second record");
        assert_eq!(
            first.load().await.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from(["from first".to_owned(), "from second".to_owned()])
        );
    }

    #[tokio::test]
    async fn atomic_replace_leaves_no_temporary_file_and_uses_private_permissions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = history_store(&temp, "/workspace/merry");
        store.record("secret-ish input").await.expect("record");
        let parent = store.path().parent().expect("history parent");
        assert!(store.path().is_file());
        assert!(store.lock_path().is_file());
        assert!(
            fs::read_dir(parent)
                .expect("read history directory")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp-"))
        );

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(parent)
                    .expect("directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(store.path())
                    .expect("history metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(store.lock_path())
                    .expect("lock metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
