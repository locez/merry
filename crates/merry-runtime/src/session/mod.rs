//! Runtime session state and state-before-event helpers.

use crate::{
    context::{CompactedCheckpoint, ContextEntry, ProjectRules, TaskAnchor},
    memory::{ActivatedMemory, MemoryStore},
    plan::PlanSessionState,
    skill::SkillCatalog,
    todo::TodoListState,
};
use merry_core::{PendingToolCall, SessionId, ToolCallId};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

mod checkpoint_window;
mod context_state;
mod events;
mod history;
mod journal;
mod messages;
mod model_turns;
mod persistence;
mod recording;
mod tool_calls;
mod transcript;
mod usage;

pub(crate) use self::checkpoint_window::PreparedCompactionInstall;
pub(crate) use self::journal::{
    JournalCursor, JournalImageFile, PreparedJournalRotation, PrunedCompactionSegment,
    commit_journal_rotation, extract_search_snapshot, is_image_id, list_archived_segment_numbers,
    read_archived_segment_transcript, read_archived_tool_output,
};
pub(crate) use self::{
    model_turns::{ModelTurn, ModelTurnId, ModelTurnStatus, PromptHistoryProjection},
    persistence::journal_destination_has_header,
    transcript::{Transcript, TranscriptItem, TranscriptItemSnapshot, UserInputOrigin},
};

#[cfg(test)]
pub(crate) use self::journal::last_commit_json;

/// Mutable runtime state for one session.
#[derive(Debug)]
pub(crate) struct SessionState {
    session_id: SessionId,
    next_sequence: u64,
    session_started: bool,
    memory_store: MemoryStore,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    compacted_checkpoint: Option<CompactedCheckpoint>,
    archived_ref_manifest: checkpoint_window::ArchivedRefManifest,
    prompt_history_projection: PromptHistoryProjection,
    context_entries: Vec<ContextEntry>,
    /// Context summary ids seeded by runtime construction; rebuilt on resume and not persisted.
    construction_context_ids: BTreeSet<String>,
    activated_memories: Vec<ActivatedMemory>,
    plan_state: PlanSessionState,
    todo_list: TodoListState,
    transcript: Transcript,
    pending_tool_calls: Vec<PendingToolCall>,
    resolved_tool_calls: BTreeSet<ToolCallId>,
    usage: Option<merry_core::SessionUsage>,
    request_token_calibration: Option<crate::token_estimate::RequestTokenCalibration>,
    external_tool_catalog: merry_core::SessionToolCatalog,
    /// Model-written list label. Empty until the first-turn side call succeeds.
    topic: Option<String>,
    /// In-flight topic call. Not persisted; aborted on drop.
    topic_task: Option<crate::runtime::TopicTask>,
    /// Durable subagent summaries persisted alongside the parent journal.
    persisted_subagents: Vec<crate::subagent::PersistedSubagentRecord>,
    /// Store directory that `journal_cursor` is currently tracking incrementally.
    tracked_store_dir: Option<PathBuf>,
    journal_cursor: journal::JournalCursor,
}

impl SessionState {
    #[cfg(test)]
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self::with_external_tool_catalog(session_id, merry_core::SessionToolCatalog::default())
    }

    pub(crate) fn with_external_tool_catalog(
        session_id: SessionId,
        external_tool_catalog: merry_core::SessionToolCatalog,
    ) -> Self {
        Self {
            session_id,
            next_sequence: 0,
            session_started: false,
            memory_store: MemoryStore::new(),
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            compacted_checkpoint: None,
            archived_ref_manifest: checkpoint_window::ArchivedRefManifest::default(),
            prompt_history_projection: PromptHistoryProjection::new(),
            context_entries: Vec::new(),
            construction_context_ids: BTreeSet::new(),
            activated_memories: Vec::new(),
            plan_state: PlanSessionState::default(),
            todo_list: TodoListState::default(),
            transcript: Transcript::new(),
            pending_tool_calls: Vec::new(),
            resolved_tool_calls: BTreeSet::new(),
            usage: None,
            request_token_calibration: None,
            external_tool_catalog,
            topic: None,
            topic_task: None,
            persisted_subagents: Vec::new(),
            tracked_store_dir: None,
            journal_cursor: journal::JournalCursor::default(),
        }
    }

    pub(crate) fn persisted_subagents(&self) -> &[crate::subagent::PersistedSubagentRecord] {
        &self.persisted_subagents
    }

    pub(crate) fn upsert_persisted_subagent(
        &mut self,
        record: crate::subagent::PersistedSubagentRecord,
    ) {
        if let Some(existing) = self
            .persisted_subagents
            .iter_mut()
            .find(|entry| entry.agent_id == record.agent_id)
        {
            *existing = record;
        } else {
            self.persisted_subagents.push(record);
        }
    }

    pub(crate) fn tracked_store_dir(&self) -> Option<&Path> {
        self.tracked_store_dir.as_deref()
    }

    pub(crate) fn set_tracked_store_dir(&mut self, dir: PathBuf) {
        self.tracked_store_dir = Some(dir);
    }

    pub(crate) fn topic(&self) -> Option<&str> {
        self.topic.as_deref()
    }

    pub(crate) fn set_generated_topic(&mut self, topic: String) {
        if self.topic.is_none() {
            self.topic = Some(topic);
        }
    }

    pub(crate) fn set_topic_task(&mut self, task: crate::runtime::TopicTask) {
        self.topic_task = Some(task);
    }

    pub(crate) fn take_topic_task(&mut self) -> Option<crate::runtime::TopicTask> {
        self.topic_task.take()
    }

    pub(crate) fn user_message_count(&self) -> usize {
        self.transcript
            .items()
            .iter()
            .filter(|item| matches!(item, transcript::TranscriptItem::UserMessage { .. }))
            .count()
    }

    pub(crate) fn first_user_message_text(&self) -> Option<String> {
        self.transcript.items().iter().find_map(|item| match item {
            transcript::TranscriptItem::UserMessage { text, .. } => Some(text.to_string()),
            _ => None,
        })
    }

    pub(crate) fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub(crate) const fn pruned_external_turns(&self) -> u32 {
        self.transcript.pruned_external_turns()
    }

    pub(crate) fn external_tool_catalog(&self) -> &merry_core::SessionToolCatalog {
        &self.external_tool_catalog
    }

    pub(crate) fn set_external_tool_catalog(&mut self, catalog: merry_core::SessionToolCatalog) {
        self.external_tool_catalog = catalog;
    }

    pub(crate) fn plan_state(&self) -> &PlanSessionState {
        &self.plan_state
    }

    pub(crate) fn plan_state_mut(&mut self) -> &mut PlanSessionState {
        &mut self.plan_state
    }

    pub(crate) fn todo_list(&self) -> &TodoListState {
        &self.todo_list
    }

    pub(crate) fn todo_list_mut(&mut self) -> &mut TodoListState {
        &mut self.todo_list
    }

    /// Renders optional dynamic Plan Mode and Todo context placed after the stable prefix.
    ///
    /// Returns `None` during normal inactive turns with no open todos so prompt
    /// and KV cache stability incur zero overhead.
    pub(crate) fn dynamic_plan_and_todo_context(&self) -> Option<String> {
        let mut sections = Vec::new();
        if let Some(plan) = self.plan_state.dynamic_prompt_reminder() {
            sections.push(plan);
        }
        if let Some(todos) = self.todo_list.dynamic_prompt_context() {
            sections.push(todos);
        }
        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }

    #[cfg(test)]
    pub(crate) fn transcript_items_for_tests(&self) -> Vec<String> {
        self.transcript
            .items()
            .iter()
            .map(|item| match item {
                transcript::TranscriptItem::UserMessage { text, .. } => {
                    format!("user:{text}")
                }
                transcript::TranscriptItem::AssistantText { text, .. } => {
                    format!("assistant:{text}")
                }
                transcript::TranscriptItem::ToolCall { call, .. } => {
                    format!("tool_call:{}", call.id().as_str())
                }
                transcript::TranscriptItem::ToolResult {
                    call_id, payload, ..
                } => {
                    format!(
                        "tool_result:{}:{}",
                        call_id.as_str(),
                        payload.full().as_str()
                    )
                }
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn transcript_model_turn_ids_for_tests(&self) -> Vec<ModelTurnId> {
        self.transcript
            .items()
            .iter()
            .map(transcript::TranscriptItem::model_turn_id)
            .collect()
    }
}

#[cfg(test)]
mod tests;
