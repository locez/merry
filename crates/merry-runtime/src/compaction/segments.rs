//! On-disk verbatim record of history a prose summary replaced.
//!
//! The summary is navigation. The segment file is the exact covered transcript
//! the later turn can read with `read_file`. A write failure omits the pointer
//! and still lets the summary install.

use crate::session_store::FileSessionStore;
use merry_core::SessionId;
use std::path::{Path, PathBuf};

const INDEX_FILE: &str = "INDEX.md";
const INDEX_HEADER: &str =
    "# Compaction segments\n\n| Segment | File | Bytes |\n| --- | --- | --- |\n";
/// One segment stays within the same order of magnitude as a single read.
const SEGMENT_MAX_BYTES: usize = 5 * 1024 * 1024;

/// Writes one segment for `markdown` and returns the hint appended to the summary.
pub(crate) async fn persist_covered_segment(
    store: &FileSessionStore,
    session_id: &SessionId,
    markdown: &str,
) -> Option<String> {
    let markdown = markdown.trim();
    if markdown.is_empty() {
        return None;
    }
    let directory = store.session_dir(session_id).join("compaction");
    if tokio::fs::create_dir_all(&directory).await.is_err() {
        return None;
    }
    let next_index = next_segment_index(&directory).await?;
    let file_name = format!("segment_{next_index}.md");
    let body = truncate_segment(markdown);
    if tokio::fs::write(directory.join(&file_name), &body)
        .await
        .is_err()
    {
        return None;
    }
    let row = format!("| {next_index} | {file_name} | {} |\n", body.len());
    if append_index(&directory, &row).await.is_err() {
        return None;
    }
    let directory = directory.display();
    Some(format!(
        "Full verbatim rollouts of compacted history are in {directory}/segment_*.md. \
         See {directory}/{INDEX_FILE} for the list. Use read_file to recover exact code, \
         paths, or tool output when this summary is not enough. Do not modify these files."
    ))
}

fn truncate_segment(markdown: &str) -> String {
    if markdown.len() <= SEGMENT_MAX_BYTES {
        return markdown.to_owned();
    }
    let mut end = SEGMENT_MAX_BYTES;
    while !markdown.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[... truncated at {SEGMENT_MAX_BYTES} bytes ...]\n",
        &markdown[..end]
    )
}

async fn next_segment_index(directory: &PathBuf) -> Option<u32> {
    let mut highest = 0_u32;
    let mut entries = tokio::fs::read_dir(directory).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(index) = name
            .strip_prefix("segment_")
            .and_then(|rest| rest.strip_suffix(".md"))
            .and_then(|index| index.parse::<u32>().ok())
        else {
            continue;
        };
        highest = highest.max(index);
    }
    highest.checked_add(1)
}

async fn append_index(directory: &Path, row: &str) -> Result<(), std::io::Error> {
    let path = directory.join(INDEX_FILE);
    if !tokio::fs::try_exists(&path).await? {
        tokio::fs::write(&path, INDEX_HEADER).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .await?;
    use tokio::io::AsyncWriteExt;
    file.write_all(row.as_bytes()).await?;
    Ok(())
}
