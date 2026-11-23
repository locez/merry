//! Compaction journal rotation, segmented archive retention, orphan image GC,
//! and on-demand archived tool-output lookup.

use super::{
    append::encode_pruned_segment_journal,
    images::JournalImageFile,
    markdown_archive::{PreparedSegmentMarkdown, parse_index_markdown, render_index_markdown},
    wire::{collect_journal_image_ids, find_tool_output_in_journal_bytes},
};
use crate::{
    FileSessionStore, HistoryRetention, SegmentedArchiveConfig, SessionStoreError,
    session::{ModelTurnId, transcript::TranscriptItem},
};
use merry_core::{SessionId, ToolCallId, ToolOutput};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

const ARCHIVE_DIR_NAME: &str = "archive";
const INDEX_FILE_NAME: &str = "INDEX.md";
const IMAGES_DIR_NAME: &str = "images";

/// Pruned transcript slice produced when a checkpoint compaction evicts turns
/// `<= compacted_through` under `ContextOnly` or `SegmentedArchive`.
#[derive(Debug)]
pub(crate) struct PrunedCompactionSegment {
    pub(crate) pruned_items: Vec<TranscriptItem>,
    pub(crate) compacted_through: ModelTurnId,
    pub(crate) summary: String,
}

/// Prepared on-disk rotation payload executed atomically under the store write lock.
#[derive(Debug)]
pub(crate) struct PreparedJournalRotation {
    pub(crate) session_id: SessionId,
    pub(crate) archive_config: Option<SegmentedArchiveConfig>,
    pub(crate) archive_jsonl_bytes: Option<Vec<u8>>,
    pub(crate) archive_images: Vec<JournalImageFile>,
    pub(crate) markdown: Option<PreparedSegmentMarkdown>,
}

/// Prepares the optional archive payload (JSONL segment + Markdown document)
/// for a compaction rotation before entering the store write lock.
pub(crate) fn prepare_journal_rotation(
    session_id: &SessionId,
    pruned: &PrunedCompactionSegment,
    retention: &HistoryRetention,
) -> Result<PreparedJournalRotation, SessionStoreError> {
    let Some(archive_config) = retention.segmented_archive_config().copied() else {
        return Ok(PreparedJournalRotation {
            session_id: session_id.clone(),
            archive_config: None,
            archive_jsonl_bytes: None,
            archive_images: Vec::new(),
            markdown: None,
        });
    };

    let (archive_jsonl_bytes, archive_images) = encode_pruned_segment_journal(
        session_id,
        &pruned.pruned_items,
        pruned.compacted_through,
        &pruned.summary,
    )?;
    let markdown = archive_config.export_markdown().then(|| {
        PreparedSegmentMarkdown::build(
            session_id.clone(),
            &pruned.pruned_items,
            pruned.compacted_through.as_u64(),
            &pruned.summary,
        )
    });

    Ok(PreparedJournalRotation {
        session_id: session_id.clone(),
        archive_config: Some(archive_config),
        archive_jsonl_bytes: Some(archive_jsonl_bytes),
        archive_images,
        markdown,
    })
}

/// Atomically rotates `journal.jsonl`, writes optional archive segment files,
/// enforces archive retention caps, and garbage-collects unreferenced images.
///
/// Caller must hold `FileSessionStore::write_lock`.
pub(crate) async fn commit_journal_rotation(
    final_journal_path: &Path,
    header_bytes: &[u8],
    commit_bytes: &[u8],
    rotation: PreparedJournalRotation,
) -> Result<(), SessionStoreError> {
    let session_dir =
        final_journal_path
            .parent()
            .ok_or_else(|| SessionStoreError::InvalidPath {
                path: final_journal_path.to_path_buf(),
                reason: "session journal path has no parent directory",
            })?;

    let mut fresh_journal_bytes = Vec::with_capacity(header_bytes.len() + commit_bytes.len());
    fresh_journal_bytes.extend_from_slice(header_bytes);
    fresh_journal_bytes.extend_from_slice(commit_bytes);

    let tmp_journal_path = session_dir.join(".journal.jsonl.tmp");
    write_and_sync_file(&tmp_journal_path, &fresh_journal_bytes).await?;

    let archive_dir = session_dir.join(ARCHIVE_DIR_NAME);
    if let Some(config) = rotation.archive_config {
        tokio::fs::create_dir_all(&archive_dir)
            .await
            .map_err(|source| io_error(archive_dir.clone(), source))?;
        let existing_segments = list_archive_segments(&archive_dir).await?;
        let next_segment = existing_segments
            .last()
            .map_or(1, |seg| seg.number.saturating_add(1));

        if let Some(archive_bytes) = rotation.archive_jsonl_bytes.as_deref() {
            let seg_jsonl_path = archive_dir.join(format!("seg_{next_segment:04}.jsonl"));
            let seg_jsonl_tmp = archive_dir.join(format!(".seg_{next_segment:04}.jsonl.tmp"));
            write_and_sync_file(&seg_jsonl_tmp, archive_bytes).await?;
            tokio::fs::rename(&seg_jsonl_tmp, &seg_jsonl_path)
                .await
                .map_err(|source| io_error(seg_jsonl_path.clone(), source))?;
        }

        let mut new_index_row = None;
        if let Some(markdown) = rotation.markdown {
            let doc = markdown.render_document(next_segment);
            let seg_md_path = archive_dir.join(format!("seg_{next_segment:04}.md"));
            let seg_md_tmp = archive_dir.join(format!(".seg_{next_segment:04}.md.tmp"));
            write_and_sync_file(&seg_md_tmp, doc.as_bytes()).await?;
            tokio::fs::rename(&seg_md_tmp, &seg_md_path)
                .await
                .map_err(|source| io_error(seg_md_path.clone(), source))?;
            new_index_row = Some(markdown.index_row(next_segment));
        }

        let surviving_numbers = enforce_archive_retention(&archive_dir, &config).await?;
        if config.export_markdown() {
            update_index_markdown(
                &archive_dir,
                &rotation.session_id,
                new_index_row,
                &surviving_numbers,
            )
            .await?;
        }
    }

    tokio::fs::rename(&tmp_journal_path, final_journal_path)
        .await
        .map_err(|source| io_error(final_journal_path.to_path_buf(), source))?;

    if let Err(error) = gc_orphan_images(session_dir, &fresh_journal_bytes, &archive_dir).await {
        tracing::warn!(
            session_id = %rotation.session_id,
            error = %error,
            "failed to garbage-collect orphan session images after rotation"
        );
    }

    Ok(())
}

/// Scans archived `seg_NNNN.jsonl` files from newest to oldest to retrieve the
/// full output of a pruned tool call on demand.
pub(crate) async fn read_archived_tool_output(
    store: &FileSessionStore,
    session_id: &SessionId,
    call_id: &ToolCallId,
) -> Result<Option<ToolOutput>, SessionStoreError> {
    let archive_dir = store.session_dir(session_id).join(ARCHIVE_DIR_NAME);
    let segments = list_archive_segments(&archive_dir).await?;
    for segment in segments.into_iter().rev() {
        let bytes = match tokio::fs::read(&segment.jsonl_path).await {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_error(segment.jsonl_path, source)),
        };
        if let Some(output) = find_tool_output_in_journal_bytes(&bytes, call_id) {
            return Ok(Some(output));
        }
    }
    Ok(None)
}

#[derive(Debug)]
struct ArchiveSegmentFile {
    number: u32,
    jsonl_path: PathBuf,
    md_path: PathBuf,
    total_bytes: u64,
}

async fn list_archive_segments(
    archive_dir: &Path,
) -> Result<Vec<ArchiveSegmentFile>, SessionStoreError> {
    let mut entries = match tokio::fs::read_dir(archive_dir).await {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error(archive_dir.to_path_buf(), source)),
    };
    let mut segments = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| io_error(archive_dir.to_path_buf(), source))?
    {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(number) = parse_segment_jsonl_name(name) else {
            continue;
        };
        let jsonl_path = entry.path();
        let jsonl_len = entry
            .metadata()
            .await
            .map_err(|source| io_error(jsonl_path.clone(), source))?
            .len();
        let md_path = archive_dir.join(format!("seg_{number:04}.md"));
        let md_len = match tokio::fs::metadata(&md_path).await {
            Ok(meta) => meta.len(),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => 0,
            Err(source) => return Err(io_error(md_path.clone(), source)),
        };
        segments.push(ArchiveSegmentFile {
            number,
            jsonl_path,
            md_path,
            total_bytes: jsonl_len.saturating_add(md_len),
        });
    }
    segments.sort_by_key(|segment| segment.number);
    Ok(segments)
}

async fn enforce_archive_retention(
    archive_dir: &Path,
    config: &SegmentedArchiveConfig,
) -> Result<BTreeSet<u32>, SessionStoreError> {
    let segments = list_archive_segments(archive_dir).await?;
    let mut total_bytes: u64 = segments.iter().map(|seg| seg.total_bytes).sum();
    let mut remaining = segments.len();
    let mut evicted = BTreeSet::new();

    for segment in &segments {
        let exceeds_count = config
            .max_segments()
            .is_some_and(|limit| remaining > limit.get());
        let exceeds_bytes = config
            .max_bytes()
            .is_some_and(|limit| total_bytes > limit.get());
        if !exceeds_count && !exceeds_bytes {
            break;
        }
        remove_if_exists(&segment.jsonl_path).await?;
        remove_if_exists(&segment.md_path).await?;
        total_bytes = total_bytes.saturating_sub(segment.total_bytes);
        remaining = remaining.saturating_sub(1);
        evicted.insert(segment.number);
    }

    Ok(segments
        .into_iter()
        .filter(|segment| !evicted.contains(&segment.number))
        .map(|segment| segment.number)
        .collect())
}

async fn update_index_markdown(
    archive_dir: &Path,
    session_id: &SessionId,
    new_row: Option<super::markdown_archive::IndexRow>,
    surviving_numbers: &BTreeSet<u32>,
) -> Result<(), SessionStoreError> {
    let index_path = archive_dir.join(INDEX_FILE_NAME);
    let existing_content = match tokio::fs::read_to_string(&index_path).await {
        Ok(content) => content,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(io_error(index_path.clone(), source)),
    };
    let mut rows = parse_index_markdown(&existing_content)
        .into_iter()
        .filter(|row| surviving_numbers.contains(&row.segment_number))
        .collect::<Vec<_>>();
    if let Some(row) = new_row
        && surviving_numbers.contains(&row.segment_number)
    {
        rows.retain(|existing| existing.segment_number != row.segment_number);
        rows.push(row);
    }
    rows.sort_by_key(|row| row.segment_number);
    let rendered = render_index_markdown(session_id, &rows);
    let tmp_path = archive_dir.join(".INDEX.md.tmp");
    write_and_sync_file(&tmp_path, rendered.as_bytes()).await?;
    tokio::fs::rename(&tmp_path, &index_path)
        .await
        .map_err(|source| io_error(index_path, source))?;
    Ok(())
}

async fn gc_orphan_images(
    session_dir: &Path,
    live_journal_bytes: &[u8],
    archive_dir: &Path,
) -> Result<(), SessionStoreError> {
    let images_dir = session_dir.join(IMAGES_DIR_NAME);
    let mut entries = match tokio::fs::read_dir(&images_dir).await {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error(images_dir.clone(), source)),
    };

    let mut referenced = collect_journal_image_ids(live_journal_bytes);
    for segment in list_archive_segments(archive_dir).await? {
        if let Ok(bytes) = tokio::fs::read(&segment.jsonl_path).await {
            referenced.extend(collect_journal_image_ids(&bytes));
        }
    }

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| io_error(images_dir.clone(), source))?
    {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".png") else {
            continue;
        };
        if !referenced.contains(stem) {
            remove_if_exists(&entry.path()).await?;
        }
    }
    Ok(())
}

fn parse_segment_jsonl_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("seg_")?.strip_suffix(".jsonl")?;
    rest.parse::<u32>().ok()
}

async fn write_and_sync_file(path: &Path, bytes: &[u8]) -> Result<(), SessionStoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .await
        .map_err(|source| io_error(path.to_path_buf(), source))?;
    file.write_all(bytes)
        .await
        .map_err(|source| io_error(path.to_path_buf(), source))?;
    file.sync_all()
        .await
        .map_err(|source| io_error(path.to_path_buf(), source))?;
    Ok(())
}

async fn remove_if_exists(path: &Path) -> Result<(), SessionStoreError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(path.to_path_buf(), source)),
    }
}

fn io_error(path: PathBuf, source: std::io::Error) -> SessionStoreError {
    SessionStoreError::Io { path, source }
}
