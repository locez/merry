//! Incremental journal commit preparation and cursor tracking.
//!
//! Reads directly from borrowed runtime state without cloning previously flushed
//! transcript items or unchanging catalogs.

use super::{
    images::{JournalImageFile, image_refs},
    wire::{
        FieldPatch, JournalCompaction, JournalContextEntry, JournalItem, JournalModelTurn,
        JournalRecord, JournalStateDelta, JournalTaskAnchor, encode_header_line,
        encode_record_line,
    },
};
use crate::{
    ContextEntry, SessionStoreError, TaskAnchor,
    plan::{PersistedPlanModeState, PlanSessionState},
    session::{
        ModelTurnId, ModelTurnStatus, PromptHistoryProjection, SessionState,
        transcript::{ToolResultPromptProjection, Transcript, TranscriptItem},
    },
    todo::TodoListState,
    token_estimate::RequestTokenCalibration,
};
use merry_core::{SessionId, SessionUsage, TodoItem, ToolCallId, ToolOutput};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct JournalCursor {
    committed_any: bool,
    flushed_item_ids: BTreeSet<u64>,
    flushed_collapsed: BTreeSet<ToolCallId>,
    flushed_summary: Option<String>,
    flushed_compacted_through: Option<ModelTurnId>,
    flushed_next_sequence: u64,
    flushed_session_started: Option<bool>,
    flushed_usage: Option<SessionUsage>,
    flushed_calibration: Option<RequestTokenCalibration>,
    flushed_task_anchor: Option<String>,
    flushed_plan_mode: Option<PersistedPlanModeState>,
    flushed_todos: Vec<TodoItem>,
    flushed_context_entries: Vec<JournalContextEntry>,
    flushed_next_item_id: Option<u64>,
    flushed_model_turns: BTreeMap<ModelTurnId, (ModelTurnStatus, Option<u64>)>,
    flushed_next_model_turn_id: Option<u64>,
    flushed_topic: Option<String>,
    flushed_subagents: Vec<crate::subagent::PersistedSubagentRecord>,
    flushed_user_message_count: usize,
}

impl JournalCursor {
    /// Fresh cursor for a destination this process is not incrementally tracking.
    ///
    /// Leaving every flushed set empty produces a complete commit. Header
    /// deduplication is enforced atomically at file append time, and replay
    /// deduplicates repeated ids/orders.
    pub(crate) fn untracked() -> Self {
        Self::default()
    }

    /// Monotonically folds a newly durable commit cursor into this session's
    /// cursor so overlapping in-flight saves can never regress flushed progress.
    pub(crate) fn merge_committed(&mut self, committed: Self) {
        self.committed_any |= committed.committed_any;
        self.flushed_item_ids.extend(committed.flushed_item_ids);
        self.flushed_collapsed.extend(committed.flushed_collapsed);
        if committed.flushed_summary.is_some() {
            self.flushed_summary = committed.flushed_summary;
        }
        if committed.flushed_compacted_through > self.flushed_compacted_through {
            self.flushed_compacted_through = committed.flushed_compacted_through;
        }
        self.flushed_next_sequence = self
            .flushed_next_sequence
            .max(committed.flushed_next_sequence);
        self.flushed_session_started = match (
            self.flushed_session_started,
            committed.flushed_session_started,
        ) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (left, right) => right.or(left),
        };
        self.flushed_usage = committed.flushed_usage;
        self.flushed_calibration = committed.flushed_calibration;
        self.flushed_task_anchor = committed.flushed_task_anchor;
        self.flushed_plan_mode = committed.flushed_plan_mode;
        self.flushed_todos = committed.flushed_todos;
        self.flushed_context_entries = committed.flushed_context_entries;
        self.flushed_next_item_id = self
            .flushed_next_item_id
            .max(committed.flushed_next_item_id);
        for (turn_id, state) in committed.flushed_model_turns {
            self.flushed_model_turns.insert(turn_id, state);
        }
        self.flushed_next_model_turn_id = self
            .flushed_next_model_turn_id
            .max(committed.flushed_next_model_turn_id);
        if committed.flushed_topic.is_some() {
            self.flushed_topic = committed.flushed_topic;
        }
        if !committed.flushed_subagents.is_empty() {
            self.flushed_subagents = committed.flushed_subagents;
        }
        self.flushed_user_message_count = self
            .flushed_user_message_count
            .max(committed.flushed_user_message_count);
    }
}

#[derive(Debug)]
pub(crate) struct PreparedJournalAppend {
    pub(crate) header_bytes: Vec<u8>,
    pub(crate) commit_bytes: Vec<u8>,
    pub(crate) images: Vec<JournalImageFile>,
    pub(crate) cursor: JournalCursor,
    pub(crate) search_user_text: Option<String>,
}

pub(crate) struct JournalAppendInput<'a> {
    pub(crate) session_id: &'a SessionId,
    pub(crate) cursor: &'a JournalCursor,
    pub(crate) transcript: &'a Transcript,
    pub(crate) prompt_history_projection: PromptHistoryProjection,
    pub(crate) compacted_checkpoint: Option<&'a crate::CompactedCheckpoint>,
    pub(crate) next_sequence: u64,
    pub(crate) session_started: bool,
    pub(crate) plan_state: &'a PlanSessionState,
    pub(crate) todo_list: &'a TodoListState,
    pub(crate) usage: Option<&'a SessionUsage>,
    pub(crate) request_token_calibration: Option<&'a RequestTokenCalibration>,
    pub(crate) task_anchor: Option<&'a TaskAnchor>,
    pub(crate) context_entries: &'a [&'a ContextEntry],
    pub(crate) topic: Option<&'a str>,
    pub(crate) subagents: &'a [crate::subagent::PersistedSubagentRecord],
}

pub(crate) fn prepare_journal_append(
    input: JournalAppendInput<'_>,
) -> Result<PreparedJournalAppend, SessionStoreError> {
    let cursor = input.cursor;
    let mut images = Vec::new();
    let mut items = Vec::new();
    let mut flushed_item_ids = cursor.flushed_item_ids.clone();
    let mut user_message_count = 0usize;

    for item in input.transcript.items() {
        if matches!(item, TranscriptItem::UserMessage { .. }) {
            user_message_count += 1;
        }
        let id = item.id().as_u64();
        if !flushed_item_ids.insert(id) {
            continue;
        }
        items.push(journal_item_from_transcript(item, &mut images));
    }

    let summary = input
        .compacted_checkpoint
        .map(crate::CompactedCheckpoint::text)
        .map(str::to_owned);
    let compacted_through = input.prompt_history_projection.compacted_through();
    let collapsed = collapsed_call_ids(input.transcript);
    let archived_tool_call_ids = collapsed
        .iter()
        .filter(|call_id| !cursor.flushed_collapsed.contains(call_id))
        .cloned()
        .collect::<Vec<_>>();
    let summary_changed = summary != cursor.flushed_summary;
    let through_changed = compacted_through != cursor.flushed_compacted_through;
    let compaction = if archived_tool_call_ids.is_empty() && !summary_changed && !through_changed {
        None
    } else {
        Some(JournalCompaction {
            summary: summary_changed.then(|| summary.clone()).flatten(),
            compacted_through: through_changed.then_some(compacted_through).flatten(),
            archived_tool_call_ids,
        })
    };

    let (delta, next_cursor) = build_state_delta(
        &input,
        flushed_item_ids,
        collapsed,
        summary,
        compacted_through,
        user_message_count,
    );

    let changed =
        !cursor.committed_any || !items.is_empty() || compaction.is_some() || !delta.is_empty();
    let header_bytes = encode_header_line(input.session_id)?;
    let commit_bytes = if changed {
        encode_record_line(&JournalRecord::Commit {
            items,
            compaction,
            state: Box::new(delta),
        })?
    } else {
        Vec::new()
    };

    let search_user_text = (user_message_count > cursor.flushed_user_message_count)
        .then(|| user_search_text(input.transcript));

    Ok(PreparedJournalAppend {
        header_bytes,
        commit_bytes,
        images,
        cursor: next_cursor,
        search_user_text,
    })
}

pub(super) fn cursor_after_load(session: &SessionState) -> JournalCursor {
    let summary = session
        .compacted_checkpoint
        .as_ref()
        .map(crate::CompactedCheckpoint::text)
        .map(str::to_owned);
    let mut flushed_item_ids = BTreeSet::new();
    let mut user_message_count = 0usize;
    for item in session.transcript.items() {
        flushed_item_ids.insert(item.id().as_u64());
        if matches!(item, TranscriptItem::UserMessage { .. }) {
            user_message_count += 1;
        }
    }
    JournalCursor {
        committed_any: true,
        flushed_item_ids,
        flushed_collapsed: collapsed_call_ids(&session.transcript),
        flushed_summary: summary,
        flushed_compacted_through: session.prompt_history_projection.compacted_through(),
        flushed_next_sequence: session.next_sequence,
        flushed_session_started: Some(session.session_started),
        flushed_usage: session.usage.clone(),
        flushed_calibration: session.request_token_calibration.clone(),
        flushed_task_anchor: session
            .task_anchor
            .as_ref()
            .map(|anchor| anchor.objective().to_owned()),
        flushed_plan_mode: session.plan_state.persisted(),
        flushed_todos: session.todo_list.items().to_vec(),
        flushed_context_entries: session
            .context_entries
            .iter()
            .map(journal_context_entry)
            .collect(),
        flushed_next_item_id: Some(session.transcript.next_id().as_u64()),
        flushed_model_turns: snapshot_model_turns(&session.transcript),
        flushed_next_model_turn_id: Some(session.transcript.next_model_turn_id().as_u64()),
        flushed_topic: session.topic.clone(),
        flushed_subagents: session.persisted_subagents.clone(),
        flushed_user_message_count: user_message_count,
    }
}

fn build_state_delta(
    input: &JournalAppendInput<'_>,
    flushed_item_ids: BTreeSet<u64>,
    flushed_collapsed: BTreeSet<ToolCallId>,
    flushed_summary: Option<String>,
    flushed_compacted_through: Option<ModelTurnId>,
    user_message_count: usize,
) -> (JournalStateDelta, JournalCursor) {
    let cursor = input.cursor;
    let next_sequence =
        (cursor.flushed_next_sequence != input.next_sequence).then_some(input.next_sequence);
    let derived_started = input.next_sequence > 0 || !input.transcript.items().is_empty();
    let session_started = (input.session_started != derived_started
        && cursor.flushed_session_started != Some(input.session_started))
    .then_some(input.session_started);

    let usage_current = input.usage.cloned();
    let usage = optional_patch(&cursor.flushed_usage, &usage_current);

    let calibration_current = input.request_token_calibration.cloned();
    let request_token_calibration =
        optional_patch(&cursor.flushed_calibration, &calibration_current);

    let anchor_current = input
        .task_anchor
        .map(|anchor| anchor.objective().to_owned());
    let task_anchor =
        optional_patch(&cursor.flushed_task_anchor, &anchor_current).map(|patch| match patch {
            FieldPatch::Set(objective) => FieldPatch::Set(JournalTaskAnchor { objective }),
            FieldPatch::Clear => FieldPatch::Clear,
        });

    let plan_mode_current = input.plan_state.persisted();
    let plan_mode = optional_patch(&cursor.flushed_plan_mode, &plan_mode_current);

    let todos_current = input.todo_list.items().to_vec();
    let todos = (cursor.flushed_todos != todos_current).then(|| todos_current.clone());

    let context_entries_current = input
        .context_entries
        .iter()
        .map(|entry| journal_context_entry(entry))
        .collect::<Vec<_>>();
    let context_entries = (cursor.flushed_context_entries != context_entries_current)
        .then(|| context_entries_current.clone());

    let derived_next_item_id = input
        .transcript
        .items()
        .iter()
        .map(|item| item.id().as_u64() + 1)
        .max()
        .unwrap_or(0);
    let next_item_id_val = input.transcript.next_id().as_u64();
    let next_item_id = (next_item_id_val != derived_next_item_id
        && cursor.flushed_next_item_id != Some(next_item_id_val))
    .then_some(next_item_id_val);

    let current_model_turns = snapshot_model_turns(input.transcript);
    let model_turns = current_model_turns
        .iter()
        .filter(|(id, state)| cursor.flushed_model_turns.get(id) != Some(*state))
        .map(|(id, (status, sequence))| JournalModelTurn {
            id: *id,
            status: *status,
            sequence: *sequence,
        })
        .collect::<Vec<_>>();

    let derived_next_turn_id = current_model_turns
        .keys()
        .map(|id| id.as_u64() + 1)
        .max()
        .unwrap_or(1);
    let next_model_turn_id_val = input.transcript.next_model_turn_id().as_u64();
    let next_model_turn_id = (next_model_turn_id_val != derived_next_turn_id
        && cursor.flushed_next_model_turn_id != Some(next_model_turn_id_val))
    .then_some(next_model_turn_id_val);

    let topic_current = input.topic.map(str::to_owned);
    let topic = optional_patch(&cursor.flushed_topic, &topic_current);

    let subagents_current = input.subagents.to_vec();
    let subagents =
        (cursor.flushed_subagents != subagents_current).then(|| subagents_current.clone());

    let delta = JournalStateDelta {
        next_sequence,
        session_started,
        usage,
        request_token_calibration,
        task_anchor,
        plan_mode,
        todos,
        context_entries,
        next_item_id,
        model_turns,
        next_model_turn_id,
        topic,
        subagents,
    };

    let next_cursor = JournalCursor {
        committed_any: true,
        flushed_item_ids,
        flushed_collapsed,
        flushed_summary,
        flushed_compacted_through,
        flushed_next_sequence: input.next_sequence,
        flushed_session_started: Some(input.session_started),
        flushed_usage: usage_current,
        flushed_calibration: calibration_current,
        flushed_task_anchor: anchor_current,
        flushed_plan_mode: plan_mode_current,
        flushed_todos: todos_current,
        flushed_context_entries: context_entries_current,
        flushed_next_item_id: Some(next_item_id_val),
        flushed_model_turns: current_model_turns,
        flushed_next_model_turn_id: Some(next_model_turn_id_val),
        flushed_topic: topic_current,
        flushed_subagents: subagents_current,
        flushed_user_message_count: user_message_count,
    };

    (delta, next_cursor)
}

fn optional_patch<T: Clone + PartialEq>(
    previous: &Option<T>,
    current: &Option<T>,
) -> Option<FieldPatch<T>> {
    if previous == current {
        return None;
    }
    match current {
        Some(value) => Some(FieldPatch::Set(value.clone())),
        None => Some(FieldPatch::Clear),
    }
}

fn snapshot_model_turns(
    transcript: &Transcript,
) -> BTreeMap<ModelTurnId, (ModelTurnStatus, Option<u64>)> {
    let sequences = transcript.model_turn_sequences();
    transcript
        .model_turn_statuses()
        .iter()
        .map(|(id, status)| (*id, (*status, sequences.get(id).copied())))
        .collect()
}

fn journal_context_entry(entry: &ContextEntry) -> JournalContextEntry {
    let summary = entry.as_summary();
    JournalContextEntry::Summary {
        id: summary.id().to_owned(),
        text: summary.text().to_owned(),
    }
}

fn journal_item_from_transcript(
    item: &TranscriptItem,
    images: &mut Vec<JournalImageFile>,
) -> JournalItem {
    match item {
        TranscriptItem::UserMessage {
            id,
            model_turn_id,
            text,
            images: item_images,
            origin,
        } => JournalItem::UserMessage {
            id: id.as_u64(),
            model_turn_id: *model_turn_id,
            text: text.to_string(),
            images: image_refs(item_images, images),
            origin: (*origin).into(),
        },
        TranscriptItem::AssistantText {
            id,
            model_turn_id,
            text,
        } => JournalItem::AssistantText {
            id: id.as_u64(),
            model_turn_id: *model_turn_id,
            text: text.to_string(),
        },
        TranscriptItem::ToolCall {
            id,
            model_turn_id,
            call,
            ..
        } => JournalItem::ToolCall {
            id: id.as_u64(),
            model_turn_id: *model_turn_id,
            call: call.clone(),
        },
        TranscriptItem::ToolResult {
            id,
            model_turn_id,
            call_id,
            result,
            payload,
            prompt_projection,
        } => {
            let full_output = crate::process::sanitize_process_output_for_journal(payload.full());
            let model_output = retained_model_output(&full_output, payload.model());
            JournalItem::ToolResult {
                id: id.as_u64(),
                model_turn_id: *model_turn_id,
                call_id: call_id.clone(),
                status: result.status(),
                diagnostic: result.diagnostic().cloned(),
                full_output,
                model_output,
                images: image_refs(payload.images(), images),
                final_output: matches!(prompt_projection, ToolResultPromptProjection::Hidden),
            }
        }
    }
}

fn retained_model_output(full: &ToolOutput, model: Option<&ToolOutput>) -> Option<ToolOutput> {
    let model = model?;
    if model == full {
        return None;
    }
    if crate::process::derived_process_model_text(full.as_str()).as_deref() == Some(model.as_str())
    {
        return None;
    }
    Some(model.clone())
}

fn collapsed_call_ids(transcript: &Transcript) -> BTreeSet<ToolCallId> {
    transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolResult {
                call_id,
                prompt_projection: ToolResultPromptProjection::Collapsed,
                ..
            } => Some(call_id.clone()),
            _ => None,
        })
        .collect()
}

fn user_search_text(transcript: &Transcript) -> String {
    transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::UserMessage { text, .. } => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
