//! Runtime session state and state-before-event helpers.

use crate::{
    action_audit::ActionAuditRegistry,
    artifact::ArtifactRegistry,
    context::{CompactedCheckpoint, ContextEntry, ProjectRules, TaskAnchor},
    judgment::JudgmentRegistry,
    ledger::TaskLedger,
    memory::{ActivatedMemory, MemoryStore},
    plan::PlanState,
    skill::SkillCatalog,
    summary_draft_promotion::SummaryDraftPromotionRegistry,
};
use merry_core::{PendingToolCall, PlanSnapshot, SessionId, ToolCallId};
use std::collections::BTreeSet;

mod artifacts;
mod checkpoint_window;
mod context_state;
mod events;
mod history;
mod judgments;
mod messages;
mod model_turns;
mod persistence;
mod plan_persistence;
mod plan_tool_commit;
mod recording;
mod tool_calls;
mod tool_result;
mod transcript;
mod usage;

pub(crate) use self::checkpoint_window::PreparedCompactionInstall;
pub(crate) use self::{
    artifacts::is_runtime_reserved_artifact_id,
    model_turns::{ModelTurn, ModelTurnId, ModelTurnStatus, PromptHistoryProjection},
    persistence::PersistableSessionBundle,
    plan_tool_commit::PreparedPlanToolCommit,
    tool_result::{ProposedToolExecutionOutcome, ToolResultLedgerObservation},
    transcript::{Transcript, TranscriptItemSnapshot, UserInputOrigin},
};

#[cfg(test)]
pub(crate) use self::transcript::TranscriptItem;

/// Mutable runtime state for one session.
#[derive(Debug)]
pub(crate) struct SessionState {
    session_id: SessionId,
    next_sequence: u64,
    session_started: bool,
    ledger: TaskLedger,
    artifacts: ArtifactRegistry,
    memory_store: MemoryStore,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    compacted_checkpoint: Option<CompactedCheckpoint>,
    archived_ref_manifest: checkpoint_window::ArchivedRefManifest,
    prompt_history_projection: PromptHistoryProjection,
    context_entries: Vec<ContextEntry>,
    activated_memories: Vec<ActivatedMemory>,
    #[allow(dead_code)]
    judgments: JudgmentRegistry,
    summary_draft_promotions: SummaryDraftPromotionRegistry,
    action_audits: ActionAuditRegistry,
    active_plan: Option<PlanState>,
    terminal_plans: Vec<PlanSnapshot>,
    transcript: Transcript,
    pending_tool_calls: Vec<PendingToolCall>,
    resolved_tool_calls: BTreeSet<ToolCallId>,
    usage: Option<merry_core::SessionUsage>,
}

impl SessionState {
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            next_sequence: 0,
            session_started: false,
            ledger: TaskLedger::default(),
            artifacts: ArtifactRegistry::default(),
            memory_store: MemoryStore::new(),
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            compacted_checkpoint: None,
            archived_ref_manifest: checkpoint_window::ArchivedRefManifest::default(),
            prompt_history_projection: PromptHistoryProjection::new(),
            context_entries: Vec::new(),
            activated_memories: Vec::new(),
            judgments: JudgmentRegistry::default(),
            summary_draft_promotions: SummaryDraftPromotionRegistry::default(),
            action_audits: ActionAuditRegistry::default(),
            active_plan: None,
            terminal_plans: Vec::new(),
            transcript: Transcript::new(),
            pending_tool_calls: Vec::new(),
            resolved_tool_calls: BTreeSet::new(),
            usage: None,
        }
    }

    pub(crate) fn ledger_projection(&self) -> crate::ledger::LedgerProjectionSnapshot {
        self.ledger.project()
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
    pub(crate) fn action_audit_snapshot(&self) -> crate::action_audit::ActionAuditRegistrySnapshot {
        self.action_audits.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn transcript_items_for_tests(&self) -> Vec<String> {
        self.transcript
            .items()
            .iter()
            .map(|item| match item {
                transcript::TranscriptItem::UserMessage { artifact_id, .. } => {
                    let content = self
                        .read_artifact_content(artifact_id)
                        .expect("user transcript artifact should be readable");
                    let text = content
                        .as_text()
                        .expect("user transcript artifact should be text");
                    format!("user:{text}")
                }
                transcript::TranscriptItem::AssistantText { artifact_id, .. } => {
                    let content = self
                        .read_artifact_content(artifact_id)
                        .expect("assistant transcript artifact should be readable");
                    let text = content
                        .as_text()
                        .expect("assistant transcript artifact should be text");
                    format!("assistant:{text}")
                }
                transcript::TranscriptItem::ToolCall { call, .. } => {
                    format!("tool_call:{}", call.id().as_str())
                }
                transcript::TranscriptItem::ToolResult {
                    call_id,
                    artifact_id,
                    ..
                } => {
                    let content = self
                        .read_artifact_content(artifact_id)
                        .expect("tool result transcript artifact should be readable");
                    let text = content
                        .as_text()
                        .expect("tool result transcript artifact should be text");
                    format!("tool_result:{}:{text}", call_id.as_str())
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

    #[cfg(test)]
    pub(crate) fn assistant_transcript_artifact_ids_for_tests(
        &self,
    ) -> Vec<merry_core::ArtifactId> {
        self.transcript
            .items()
            .iter()
            .filter_map(|item| match item {
                transcript::TranscriptItem::AssistantText { artifact_id, .. } => {
                    Some(artifact_id.clone())
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
