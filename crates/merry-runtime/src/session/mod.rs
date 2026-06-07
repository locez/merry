//! Runtime session state and state-before-event helpers.

use crate::{
    action_audit::ActionAuditRegistry,
    artifact::ArtifactRegistry,
    context::{CompactedCheckpoint, ContextEntry, ProjectRules, TaskAnchor},
    judgment::JudgmentRegistry,
    ledger::TaskLedger,
    memory::{ActivatedMemory, MemoryStore},
    skill::SkillCatalog,
    summary_draft_promotion::SummaryDraftPromotionRegistry,
};
use merry_core::{PendingToolCall, SessionId, ToolCallId};
use std::collections::BTreeSet;

mod artifacts;
mod checkpoint_window;
mod context_state;
mod events;
mod history;
mod judgments;
mod messages;
mod recording;
mod tool_calls;
mod tool_result;

use self::history::{ResolvedToolContinuation, SessionMessage};
pub(crate) use self::{
    artifacts::is_runtime_reserved_artifact_id,
    history::ResolvedToolContinuationSnapshot,
    tool_result::{ProposedToolExecutionOutcome, ToolResultLedgerObservation},
};

/// Mutable runtime state for one session.
#[derive(Debug)]
pub(crate) struct SessionState {
    session_id: SessionId,
    next_sequence: u64,
    session_started: bool,
    next_history_id: u64,
    ledger: TaskLedger,
    artifacts: ArtifactRegistry,
    memory_store: MemoryStore,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    compacted_checkpoint: Option<CompactedCheckpoint>,
    context_entries: Vec<ContextEntry>,
    activated_memories: Vec<ActivatedMemory>,
    #[allow(dead_code)]
    judgments: JudgmentRegistry,
    summary_draft_promotions: SummaryDraftPromotionRegistry,
    action_audits: ActionAuditRegistry,
    append_only_body: Vec<SessionMessage>,
    pending_tool_calls: Vec<PendingToolCall>,
    resolved_tool_calls: BTreeSet<ToolCallId>,
    uncheckpointed_tool_continuations: Vec<ResolvedToolContinuation>,
}

impl SessionState {
    pub(crate) fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            next_sequence: 0,
            session_started: false,
            next_history_id: 0,
            ledger: TaskLedger::default(),
            artifacts: ArtifactRegistry::default(),
            memory_store: MemoryStore::new(),
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            compacted_checkpoint: None,
            context_entries: Vec::new(),
            activated_memories: Vec::new(),
            judgments: JudgmentRegistry::default(),
            summary_draft_promotions: SummaryDraftPromotionRegistry::default(),
            action_audits: ActionAuditRegistry::default(),
            append_only_body: Vec::new(),
            pending_tool_calls: Vec::new(),
            resolved_tool_calls: BTreeSet::new(),
            uncheckpointed_tool_continuations: Vec::new(),
        }
    }

    pub(crate) fn ledger_projection(&self) -> crate::ledger::LedgerProjectionSnapshot {
        self.ledger.project()
    }

    #[cfg(test)]
    pub(crate) fn action_audit_snapshot(&self) -> crate::action_audit::ActionAuditRegistrySnapshot {
        self.action_audits.snapshot()
    }
}

#[cfg(test)]
mod tests;
