//! Append-only session journal.
//!
//! `journal.jsonl` is the only durable session fact. Each save appends one
//! incremental commit of new transcript bodies plus the delta of side state
//! that cannot be derived from them. Prompt projections, the archived-ref
//! manifest, and the in-memory trajectory are rebuilt on load.

mod append;
mod archive;
mod images;
mod markdown_archive;
mod replay;
mod wire;

use super::SessionState;

pub(crate) use self::append::{
    JournalAppendInput, JournalCursor, PreparedJournalAppend, prepare_journal_append,
};
pub(crate) use self::archive::{
    PreparedJournalRotation, PrunedCompactionSegment, commit_journal_rotation,
    prepare_journal_rotation, read_archived_tool_output,
};
pub(crate) use self::images::{JournalImageFile, images_dir, is_image_id};
pub(crate) use self::replay::{
    ReplayedJournal, extract_search_snapshot, journal_file_has_header, session_from_journal,
};

#[cfg(test)]
pub(crate) fn last_commit_json(bytes: &[u8]) -> serde_json::Value {
    let mut chosen = None;
    for line in self::wire::complete_journal_lines(bytes) {
        if !line.is_empty() {
            chosen = Some(line);
        }
    }
    let line = chosen.expect("journal has a commit line");
    serde_json::from_slice(line).expect("journal commit is JSON")
}

impl SessionState {
    pub(crate) fn adopt_journal_cursor(&mut self, cursor: JournalCursor) {
        self.journal_cursor.merge_committed(cursor);
    }

    /// Replaces the tracked cursor after a full journal rotation rewrites `journal.jsonl`.
    pub(crate) fn replace_journal_cursor(&mut self, cursor: JournalCursor) {
        self.journal_cursor = cursor;
    }
}
