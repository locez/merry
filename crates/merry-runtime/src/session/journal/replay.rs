//! Replay of append-only `journal.jsonl` records into `SessionState`.

use super::{
    append::{JournalCursor, cursor_after_load},
    images::load_images,
    wire::{
        CURRENT_SESSION_JOURNAL_FORMAT_VERSION, FieldPatch, JournalContextEntry, JournalItem,
        JournalRecord, JournalTaskAnchor, complete_journal_lines, complete_journal_prefix,
    },
};
use crate::{
    ContextEntry, ContextSummary, SessionStoreError, TaskAnchor,
    plan::{PersistedPlanModeState, PlanSessionState},
    session::{
        ModelTurnId, ModelTurnStatus, PromptHistoryProjection, SessionState,
        checkpoint_window::{ArchivedRefManifest, archived_refs_after_projection},
        persistence::{invalid_document, runtime_error_to_invalid_document},
        transcript::{
            ToolCallPromptProjection, ToolResultPayload, ToolResultPromptProjection, Transcript,
            TranscriptItem, TranscriptItemId,
        },
    },
    todo::TodoListState,
    token_estimate::RequestTokenCalibration,
};
use merry_core::{
    SessionId, SessionToolCatalog, SessionUsage, TodoItem, ToolCallResult, ToolOutput,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(crate) struct ReplayedJournal {
    pub(crate) session: SessionState,
    /// When non-empty trailing bytes lacked a terminating `\n`, the valid
    /// prefix byte offset to truncate `journal.jsonl` back to before resuming.
    pub(crate) truncate_to_len: Option<u64>,
}

/// Searchable fields extracted from a session's `journal.jsonl`.
#[derive(Debug, Default)]
pub(crate) struct JournalSearchSnapshot {
    pub(crate) topic: Option<String>,
    pub(crate) user_text: String,
}

#[derive(Default)]
struct ReplayedSideState {
    next_sequence: u64,
    session_started: Option<bool>,
    usage: Option<SessionUsage>,
    request_token_calibration: Option<RequestTokenCalibration>,
    task_anchor: Option<JournalTaskAnchor>,
    plan_mode: Option<PersistedPlanModeState>,
    todos: Vec<TodoItem>,
    context_entries: Vec<JournalContextEntry>,
    next_item_id: Option<u64>,
    model_turns: BTreeMap<ModelTurnId, ModelTurnStatus>,
    model_turn_sequences: BTreeMap<ModelTurnId, u64>,
    next_model_turn_id: Option<u64>,
    topic: Option<String>,
    subagents: Vec<crate::subagent::PersistedSubagentRecord>,
}

impl ReplayedSideState {
    fn apply(&mut self, delta: super::wire::JournalStateDelta) -> Result<(), SessionStoreError> {
        if let Some(next_sequence) = delta.next_sequence {
            self.next_sequence = next_sequence;
        }
        if let Some(session_started) = delta.session_started {
            self.session_started = Some(session_started);
        }
        if let Some(patch) = delta.usage {
            patch.apply(&mut self.usage);
        }
        if let Some(patch) = delta.request_token_calibration {
            patch.apply(&mut self.request_token_calibration);
        }
        if let Some(patch) = delta.task_anchor {
            patch.apply(&mut self.task_anchor);
        }
        if let Some(patch) = delta.plan_mode {
            patch.apply(&mut self.plan_mode);
        }
        if let Some(todos) = delta.todos {
            self.todos = todos;
        }
        if let Some(context_entries) = delta.context_entries {
            self.context_entries = context_entries;
        }
        if let Some(next_item_id) = delta.next_item_id {
            self.next_item_id = Some(next_item_id);
        }
        let mut commit_turn_ids = BTreeSet::new();
        for turn in delta.model_turns {
            if !commit_turn_ids.insert(turn.id) {
                return Err(invalid_document("session journal repeats a model turn"));
            }
            self.model_turns.insert(turn.id, turn.status);
            if let Some(sequence) = turn.sequence {
                self.model_turn_sequences.insert(turn.id, sequence);
            }
        }
        if let Some(next_model_turn_id) = delta.next_model_turn_id {
            self.next_model_turn_id = Some(next_model_turn_id);
        }
        if let Some(patch) = delta.topic {
            patch.apply(&mut self.topic);
        }
        if let Some(subagents) = delta.subagents {
            self.subagents = subagents;
        }
        Ok(())
    }
}

pub(crate) async fn session_from_journal(
    session_id: &SessionId,
    bytes: &[u8],
    images_dir: &Path,
) -> Result<ReplayedJournal, SessionStoreError> {
    let (complete_bytes, truncate_to_len) = complete_journal_prefix(bytes);
    let records = parse_journal_records(complete_bytes)?;
    if records.is_empty() {
        return Err(invalid_document("session journal has no complete record"));
    }
    let JournalRecord::Header {
        format_version,
        session_id: stored_id,
    } = &records[0]
    else {
        return Err(invalid_document("session journal is missing its header"));
    };
    if *format_version != CURRENT_SESSION_JOURNAL_FORMAT_VERSION {
        return Err(SessionStoreError::UnsupportedFormatVersion {
            actual: *format_version,
        });
    }
    if stored_id != session_id {
        return Err(SessionStoreError::SessionIdMismatch {
            requested: session_id.clone(),
            actual: stored_id.clone(),
        });
    }

    let mut transcript_items = Vec::new();
    let mut seen_item_ids = BTreeSet::new();
    let mut compactions = Vec::new();
    let mut state = ReplayedSideState::default();
    let mut saw_commit = false;

    for record in records.into_iter().skip(1) {
        let JournalRecord::Commit {
            items,
            compaction,
            state: delta,
        } = record
        else {
            return Err(invalid_document("session journal has more than one header"));
        };
        saw_commit = true;
        for item in items {
            if !seen_item_ids.insert(item.id()) {
                continue;
            }
            transcript_items.push(transcript_item_from_journal(item, images_dir).await?);
        }
        if let Some(compaction) = compaction {
            compactions.push(compaction);
        }
        state.apply(*delta)?;
    }
    if !saw_commit {
        return Err(invalid_document("session journal has no commit"));
    }

    let mut hidden_calls = BTreeSet::new();
    for item in &transcript_items {
        if let TranscriptItem::ToolResult {
            call_id,
            prompt_projection: ToolResultPromptProjection::Hidden,
            ..
        } = item
        {
            hidden_calls.insert(call_id.clone());
        }
    }
    for item in &mut transcript_items {
        if let TranscriptItem::ToolCall {
            call,
            prompt_projection,
            ..
        } = item
            && hidden_calls.contains(call.id())
        {
            *prompt_projection = ToolCallPromptProjection::Hidden;
        }
    }

    let derived_next_item_id = transcript_items
        .iter()
        .map(|item| item.id().as_u64() + 1)
        .max()
        .unwrap_or(0);
    let next_item_id = state.next_item_id.unwrap_or(derived_next_item_id);
    let derived_next_model_turn_id = state
        .model_turns
        .keys()
        .map(|id| id.as_u64() + 1)
        .max()
        .unwrap_or(1);
    let next_model_turn_id = state
        .next_model_turn_id
        .unwrap_or(derived_next_model_turn_id);
    let session_started = state
        .session_started
        .unwrap_or(state.next_sequence > 0 || !transcript_items.is_empty());

    let mut transcript = Transcript::from_journal_parts(
        transcript_items,
        next_item_id,
        state.model_turns,
        state.model_turn_sequences,
        next_model_turn_id,
    )
    .map_err(runtime_error_to_invalid_document)?;

    let mut prompt_history_projection = PromptHistoryProjection::new();
    let mut compacted_checkpoint = None;
    for compaction in compactions {
        let archived = compaction
            .archived_tool_call_ids
            .into_iter()
            .filter(|call_id| {
                transcript.items().iter().any(|item| {
                    matches!(
                        item,
                        TranscriptItem::ToolResult {
                            call_id: candidate,
                            prompt_projection: ToolResultPromptProjection::Full,
                            ..
                        } if candidate == call_id
                    )
                })
            })
            .collect::<BTreeSet<_>>();
        if !archived.is_empty() {
            transcript
                .archive_tool_results(&archived)
                .map_err(runtime_error_to_invalid_document)?;
        }
        if let Some(summary) = compaction.summary {
            compacted_checkpoint = Some(
                crate::CompactedCheckpoint::new(summary)
                    .map_err(|_| invalid_document("stored compacted checkpoint is invalid"))?,
            );
        }
        if let Some(compacted_through) = compaction.compacted_through {
            prompt_history_projection = prompt_history_projection
                .advanced_through(&transcript, compacted_through)
                .map_err(runtime_error_to_invalid_document)?;
        }
    }

    let resolved_tool_calls = transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolResult { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let archived_refs = archived_refs_after_projection(&transcript, prompt_history_projection)
        .map_err(runtime_error_to_invalid_document)?;
    let archived_ref_manifest = ArchivedRefManifest::new(archived_refs)
        .map_err(|_| invalid_document("stored archived ref manifest is invalid"))?;
    let plan_state = state
        .plan_mode
        .map(|persisted| PlanSessionState::from_persisted(persisted, BTreeMap::new()))
        .unwrap_or_default();
    let todo_list = TodoListState::from_items(state.todos);
    let mut session = SessionState {
        session_id: session_id.clone(),
        next_sequence: state.next_sequence,
        session_started,
        memory_store: crate::memory::MemoryStore::new(),
        skill_catalog: None,
        project_rules: None,
        task_anchor: None,
        compacted_checkpoint,
        archived_ref_manifest,
        prompt_history_projection,
        context_entries: state
            .context_entries
            .into_iter()
            .map(context_entry_from_journal)
            .collect::<Result<Vec<_>, _>>()?,
        construction_context_ids: BTreeSet::new(),
        activated_memories: Vec::new(),
        plan_state,
        todo_list,
        transcript,
        pending_tool_calls: Vec::new(),
        resolved_tool_calls,
        usage: state.usage,
        request_token_calibration: state.request_token_calibration,
        external_tool_catalog: SessionToolCatalog::default(),
        topic: state.topic,
        topic_task: None,
        persisted_subagents: state.subagents,
        tracked_store_dir: None,
        journal_cursor: JournalCursor::default(),
    };
    if let Some(anchor) = state.task_anchor {
        session.set_task_anchor(
            TaskAnchor::new(anchor.objective)
                .map_err(|_| invalid_document("stored task anchor is invalid"))?,
        );
    }
    session.validate_persisted_transcript_for(
        &session.transcript,
        session.prompt_history_projection,
        session.compacted_checkpoint.as_ref(),
        &session.resolved_tool_calls,
    )?;
    session.validate_persisted_context_entries_with_checkpoint(
        session.compacted_checkpoint.as_ref(),
    )?;
    session
        .validate_archived_ref_manifest()
        .map_err(runtime_error_to_invalid_document)?;
    session.journal_cursor = cursor_after_load(&session);
    Ok(ReplayedJournal {
        session,
        truncate_to_len,
    })
}

/// Whether `bytes` already begin with this session's journal header.
///
/// An empty file has no header. A non-empty file whose first complete line is
/// not that header is corrupt and must fail rather than receive an append.
pub(crate) fn journal_file_has_header(
    bytes: &[u8],
    session_id: &SessionId,
) -> Result<bool, SessionStoreError> {
    let Some(line) = complete_journal_lines(bytes).find(|line| !line.is_empty()) else {
        if bytes.is_empty() || bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(false);
        }
        return Err(invalid_document("session journal is truncated"));
    };
    match serde_json::from_slice::<JournalRecord>(line)? {
        JournalRecord::Header {
            format_version,
            session_id: stored_id,
        } => {
            if format_version != CURRENT_SESSION_JOURNAL_FORMAT_VERSION {
                return Err(SessionStoreError::UnsupportedFormatVersion {
                    actual: format_version,
                });
            }
            if stored_id != *session_id {
                return Err(SessionStoreError::SessionIdMismatch {
                    requested: session_id.clone(),
                    actual: stored_id,
                });
            }
            Ok(true)
        }
        JournalRecord::Commit { .. } => {
            Err(invalid_document("session journal is missing its header"))
        }
    }
}

/// Extracts user message text and the latest stored topic from `journal.jsonl`
/// for full-text search index rebuilds.
pub(crate) fn extract_search_snapshot(
    bytes: &[u8],
) -> Result<JournalSearchSnapshot, SessionStoreError> {
    let (complete_bytes, _) = complete_journal_prefix(bytes);
    let mut seen_item_ids = BTreeSet::new();
    let mut texts = Vec::new();
    let mut topic = None;
    for record in parse_journal_records(complete_bytes)? {
        let JournalRecord::Commit { items, state, .. } = record else {
            continue;
        };
        for item in items {
            if !seen_item_ids.insert(item.id()) {
                continue;
            }
            if let JournalItem::UserMessage { text, .. } = item {
                texts.push(text);
            }
        }
        if let Some(patch) = state.topic {
            match patch {
                FieldPatch::Set(value) => topic = Some(value),
                FieldPatch::Clear => topic = None,
            }
        }
    }
    Ok(JournalSearchSnapshot {
        topic,
        user_text: texts.join("\n"),
    })
}

fn parse_journal_records(complete_bytes: &[u8]) -> Result<Vec<JournalRecord>, SessionStoreError> {
    let mut records = Vec::new();
    for line in complete_bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        records.push(serde_json::from_slice(line)?);
    }
    Ok(records)
}

async fn transcript_item_from_journal(
    item: JournalItem,
    images_dir: &Path,
) -> Result<TranscriptItem, SessionStoreError> {
    Ok(match item {
        JournalItem::UserMessage {
            id,
            model_turn_id,
            text,
            images,
            origin,
        } => TranscriptItem::UserMessage {
            id: TranscriptItemId::new(id),
            model_turn_id,
            text: text.into(),
            images: load_images(&images, images_dir).await?,
            origin: origin.into(),
        },
        JournalItem::AssistantText {
            id,
            model_turn_id,
            text,
        } => TranscriptItem::AssistantText {
            id: TranscriptItemId::new(id),
            model_turn_id,
            text: text.into(),
        },
        JournalItem::ToolCall {
            id,
            model_turn_id,
            call,
        } => TranscriptItem::ToolCall {
            id: TranscriptItemId::new(id),
            model_turn_id,
            call,
            prompt_projection: ToolCallPromptProjection::Full,
        },
        JournalItem::ToolResult {
            id,
            model_turn_id,
            call_id,
            status,
            diagnostic,
            full_output,
            model_output,
            images,
            final_output,
        } => {
            let result = ToolCallResult::new(call_id.clone(), status, diagnostic)
                .map_err(|_| invalid_document("stored tool call result is invalid"))?;
            let model_output = model_output.or_else(|| {
                crate::process::derived_process_model_text(full_output.as_str())
                    .map(ToolOutput::text)
            });
            TranscriptItem::ToolResult {
                id: TranscriptItemId::new(id),
                model_turn_id,
                call_id,
                result,
                payload: ToolResultPayload::new(
                    full_output,
                    model_output,
                    load_images(&images, images_dir).await?,
                ),
                prompt_projection: if final_output {
                    ToolResultPromptProjection::Hidden
                } else {
                    ToolResultPromptProjection::Full
                },
            }
        }
    })
}

fn context_entry_from_journal(
    entry: JournalContextEntry,
) -> Result<ContextEntry, SessionStoreError> {
    match entry {
        JournalContextEntry::Summary { id, text } => Ok(ContextEntry::summary(
            ContextSummary::new(id, text)
                .map_err(|_| invalid_document("stored context summary is invalid"))?,
        )),
    }
}
