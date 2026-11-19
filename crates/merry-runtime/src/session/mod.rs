//! Runtime session state and state-before-event helpers.

use crate::{
    context::{CompactedCheckpoint, ContextEntry, ProjectRules, TaskAnchor},
    memory::{ActivatedMemory, MemoryStore},
    plan::PlanState,
    skill::SkillCatalog,
};
use merry_core::{PendingToolCall, PlanSnapshot, SessionId, ToolCallId};
use std::collections::BTreeSet;

mod checkpoint_window;
mod context_state;
mod events;
mod history;
mod journal;
mod messages;
mod model_turns;
mod persistence;
mod plan_tool_commit;
mod recording;
mod tool_calls;
mod transcript;
mod usage;

pub(crate) use self::checkpoint_window::PreparedCompactionInstall;
pub(crate) use self::journal::{
    JournalCursor, JournalImageFile, extract_search_snapshot, is_image_id,
};
pub(crate) use self::{
    model_turns::{ModelTurn, ModelTurnId, ModelTurnStatus, PromptHistoryProjection},
    persistence::{PersistableSessionBundle, journal_destination_has_header},
    plan_tool_commit::PreparedPlanToolCommit,
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
    activated_memories: Vec<ActivatedMemory>,
    active_plan: Option<PlanState>,
    terminal_plans: Vec<PlanSnapshot>,
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
            activated_memories: Vec::new(),
            active_plan: None,
            terminal_plans: Vec::new(),
            transcript: Transcript::new(),
            pending_tool_calls: Vec::new(),
            resolved_tool_calls: BTreeSet::new(),
            usage: None,
            request_token_calibration: None,
            external_tool_catalog,
            topic: None,
            topic_task: None,
            journal_cursor: journal::JournalCursor::default(),
        }
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

    pub(crate) fn external_tool_catalog(&self) -> &merry_core::SessionToolCatalog {
        &self.external_tool_catalog
    }

    pub(crate) fn active_plan(&self) -> Option<&PlanState> {
        self.active_plan.as_ref()
    }

    pub(crate) fn active_plan_mut(&mut self) -> Option<&mut PlanState> {
        self.active_plan.as_mut()
    }

    pub(crate) fn set_active_plan(&mut self, plan: PlanState) {
        self.active_plan = Some(plan);
    }

    pub(crate) fn take_active_plan(&mut self) -> Option<PlanState> {
        self.active_plan.take()
    }

    pub(crate) fn terminal_plans(&self) -> &[PlanSnapshot] {
        &self.terminal_plans
    }

    pub(crate) fn push_terminal_plan(&mut self, snapshot: PlanSnapshot) {
        const MAX_TERMINAL_PLANS: usize = 8;
        if self.terminal_plans.len() == MAX_TERMINAL_PLANS {
            self.terminal_plans.remove(0);
        }
        self.terminal_plans.push(snapshot);
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
