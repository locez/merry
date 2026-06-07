//! Runtime session state and state-before-event helpers.

#[cfg(test)]
use crate::summary_draft_promotion::SummaryDraftPromotionRegistrySnapshot;
use crate::{
    ActionExecutionEvidence, ActionProposal, RuntimeError, ToolActionKind,
    action_audit::{ActionAuditPolicy, ActionAuditRegistry},
    action_policy::ActionPolicyDecision,
    artifact::{ArtifactContent, ArtifactError, ArtifactRegistry},
    context::{
        CompactedCheckpoint, ContextCompiler, ContextEntry, ContextError, ProjectRules,
        SessionContextSnapshot, TaskAnchor,
    },
    judgment::{
        JudgmentError, JudgmentEvidence, JudgmentOutcome, JudgmentRecord, JudgmentRegistry,
        JudgmentRequest, SummaryDraftPromotionError, SummaryDraftPromotionInput,
        context_summary_from_accepted_summary_draft, validate_summary_draft_record_purpose,
    },
    ledger::{LedgerFactKind, LedgerUpdateKind, TaskLedger},
    memory::{ActivatedMemory, MemoryError, MemoryItem, MemoryStore},
    skill::SkillCatalog,
    step::CompiledSessionMessage,
    summary_draft_promotion::{
        SummaryDraftPromotionAcceptanceResult, SummaryDraftPromotionAcceptanceStatus,
        SummaryDraftPromotionRegistry,
    },
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, EvidenceLocator, EvidenceRef,
    PendingToolCall, RuntimeEvent, RuntimeEventKind, SessionId, ToolCallId, ToolCallResult,
    ToolCallResultStatus,
};
use std::collections::BTreeSet;

mod artifacts;
mod compaction;
mod history;
mod tool_result;

pub(crate) use self::{
    artifacts::is_runtime_reserved_artifact_id,
    history::ResolvedToolContinuationSnapshot,
    tool_result::{ProposedToolExecutionOutcome, ToolResultLedgerObservation},
};
use self::{
    artifacts::{assistant_output_id, final_output_id, process_input_id, tool_result_id},
    history::{ResolvedToolContinuation, SessionMessage},
};

const WORKSPACE_READ_FILE_TOOL_NAME: &str = "workspace_read_file";

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

    pub(crate) fn seed_context_summary(
        &mut self,
        id: &str,
        text: &str,
    ) -> Result<(), RuntimeError> {
        let artifact_id = ArtifactId::new(&format!("context-seed-{id}"))?;
        let artifact = ArtifactRef::new(artifact_id.clone(), ArtifactKind::Text);
        let content = ArtifactContent::text(text);
        let recorded = self.artifacts.record(artifact, content)?;
        let evidence = self
            .artifacts
            .evidence_ref(recorded.id(), EvidenceLocator::whole_artifact())?;
        let summary = crate::ContextSummary::new(
            id,
            text,
            vec![crate::ContextEvidence::new(
                "seeded runtime context",
                evidence,
            )?],
        )?;
        self.record_checked_context_entry(ContextEntry::summary(summary))?;
        Ok(())
    }

    pub(crate) fn set_project_rules(&mut self, project_rules: ProjectRules) {
        self.project_rules = Some(project_rules);
    }

    pub(crate) fn project_rules(&self) -> Option<ProjectRules> {
        self.project_rules.clone()
    }

    pub(crate) fn set_skill_catalog(&mut self, skill_catalog: SkillCatalog) {
        self.skill_catalog = Some(skill_catalog);
    }

    pub(crate) fn skill_catalog(&self) -> Option<SkillCatalog> {
        self.skill_catalog.clone()
    }

    pub(crate) fn set_task_anchor(&mut self, task_anchor: TaskAnchor) {
        self.task_anchor = Some(task_anchor);
    }

    pub(crate) fn task_anchor(&self) -> Option<TaskAnchor> {
        self.task_anchor.clone()
    }

    pub(crate) fn record_artifact_state(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<ArtifactRef, ArtifactError> {
        self.artifacts.record(artifact, content)
    }

    pub(crate) fn record_artifact_events(
        &mut self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, ArtifactError> {
        let content_bytes = content.as_bytes().len();
        let recorded = self.record_artifact_state(artifact, content)?;
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        let mut events = Vec::with_capacity(if self.session_started { 1 } else { 2 });

        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));

        Ok(events)
    }

    pub(crate) fn record_process_input_artifact(
        &mut self,
        content: ArtifactContent,
    ) -> Result<(ArtifactRef, Vec<RuntimeEvent>), ArtifactError> {
        let artifact = ArtifactRef::new(process_input_id(self.next_sequence()), ArtifactKind::Json);
        let events = self.record_artifact_events(artifact.clone(), content)?;
        Ok((artifact, events))
    }

    pub(crate) fn record_assistant_text_output(
        &mut self,
        text: String,
    ) -> Result<RuntimeEvent, ArtifactError> {
        let artifact_sequence = self.next_sequence();
        let artifact = ArtifactRef::new(assistant_output_id(artifact_sequence), ArtifactKind::Text);
        let content = ArtifactContent::text(text);
        let content_bytes = content.as_bytes().len();
        let recorded = self.record_artifact_state(artifact, content)?;
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        let history_id = self.next_history_id();
        self.append_only_body
            .push(SessionMessage::assistant(history_id, recorded.id().clone()));
        Ok(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ))
    }

    pub(crate) fn record_final_output(
        &mut self,
        call_id: ToolCallId,
        json: String,
    ) -> Result<(crate::FinalOutput, Vec<RuntimeEvent>), RuntimeError> {
        let Some(pending_index) = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == &call_id)
        else {
            return if self.resolved_tool_calls.contains(&call_id) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id,
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id,
                })
            };
        };

        let artifact_sequence = self.next_sequence();
        let artifact = ArtifactRef::new(final_output_id(artifact_sequence), ArtifactKind::Json);
        let content = ArtifactContent::json(json.clone());
        let content_bytes = content.as_bytes().len();
        let recorded = self.record_artifact_state(artifact, content)?;
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        self.pending_tool_calls.remove(pending_index);
        self.resolved_tool_calls.insert(call_id.clone());

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }
        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded {
                artifact: recorded.clone(),
            },
            LedgerFactKind::ArtifactRecorded,
        ));
        events.push(self.record_event(
            RuntimeEventKind::FinalOutputRecorded {
                call_id: call_id.clone(),
                artifact: recorded.clone(),
            },
            LedgerFactKind::FinalOutputRecorded,
        ));

        Ok((crate::FinalOutput::new(call_id, recorded, json), events))
    }

    pub(crate) fn record_user_message_body(&mut self, text: &str) {
        let history_id = self.next_history_id();
        self.append_only_body
            .push(SessionMessage::user(history_id, text.to_owned()));
    }

    pub(crate) fn evidence_ref(
        &self,
        artifact_id: &ArtifactId,
        locator: EvidenceLocator,
    ) -> Result<EvidenceRef, ArtifactError> {
        self.artifacts.evidence_ref(artifact_id, locator)
    }

    pub(crate) fn record_context_entry(&mut self, entry: ContextEntry) {
        self.context_entries.push(entry);
    }

    fn record_checked_context_entry(&mut self, entry: ContextEntry) -> Result<(), ContextError> {
        let mut candidate_entries = self.context_entries.clone();
        candidate_entries.push(entry.clone());
        let candidate_snapshot = SessionContextSnapshot::new(
            candidate_entries,
            self.artifacts.clone(),
            self.activated_memories.clone(),
            self.compacted_checkpoint.clone(),
        );
        ContextCompiler::new().compile(&candidate_snapshot)?;

        self.context_entries.push(entry);
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn record_memory_item(&mut self, item: MemoryItem) -> Result<(), MemoryError> {
        self.memory_store.record(item)
    }

    pub(crate) fn memory_store(&self) -> &MemoryStore {
        &self.memory_store
    }

    #[allow(dead_code)]
    pub(crate) fn record_activated_memory(&mut self, memory: ActivatedMemory) {
        self.activated_memories.push(memory);
    }

    #[allow(dead_code)]
    pub(crate) fn record_activated_memories(&mut self, memories: Vec<ActivatedMemory>) {
        self.activated_memories.extend(memories);
    }

    pub(crate) fn replace_activated_memories(&mut self, memories: Vec<ActivatedMemory>) {
        self.activated_memories = memories;
    }

    pub(crate) fn context_snapshot(&self) -> SessionContextSnapshot {
        SessionContextSnapshot::new(
            self.context_entries.clone(),
            self.artifacts.clone(),
            self.activated_memories.clone(),
            self.compacted_checkpoint.clone(),
        )
    }

    pub(crate) fn ledger_projection(&self) -> crate::ledger::LedgerProjectionSnapshot {
        self.ledger.project()
    }

    #[cfg(test)]
    pub(crate) fn action_audit_snapshot(&self) -> crate::action_audit::ActionAuditRegistrySnapshot {
        self.action_audits.snapshot()
    }

    #[allow(dead_code)]
    pub(crate) fn preflight_judgment_request(
        &self,
        request: &JudgmentRequest,
    ) -> Result<(), JudgmentError> {
        self.validate_judgment_evidence_refs(request.evidence())
    }

    #[allow(dead_code)]
    pub(crate) fn record_judgment(
        &mut self,
        request: JudgmentRequest,
        outcome: JudgmentOutcome,
    ) -> Result<JudgmentRecord, JudgmentError> {
        self.validate_judgment_evidence(&request, &outcome)?;
        self.judgments.record_completed(request, outcome)
    }

    #[allow(dead_code)]
    pub(crate) fn record_summary_draft_judgment(
        &mut self,
        request: JudgmentRequest,
        outcome: JudgmentOutcome,
    ) -> Result<JudgmentRecord, JudgmentError> {
        validate_summary_draft_record_purpose(&request, &outcome)?;
        self.record_judgment(request, outcome)
    }

    #[allow(dead_code)]
    pub(crate) fn promote_summary_draft_to_context(
        &mut self,
        request: &JudgmentRequest,
        outcome: &JudgmentOutcome,
        input: SummaryDraftPromotionInput,
    ) -> Result<(), SummaryDraftPromotionError> {
        let summary = context_summary_from_accepted_summary_draft(request, outcome, &input)?;
        let acceptance_status = self.summary_draft_promotions.acceptance_status(&input)?;

        if self.context_entries.iter().any(|entry| {
            matches!(
                entry,
                ContextEntry::Summary(existing) if existing.id() == summary.id()
            )
        }) && acceptance_status != SummaryDraftPromotionAcceptanceStatus::AlreadyPromoted
        {
            return Err(SummaryDraftPromotionError::DuplicateSummaryId {
                summary_id: summary.id().to_owned(),
            });
        }

        let acceptance = self.summary_draft_promotions.accept(&input)?;
        let record_id = match acceptance {
            SummaryDraftPromotionAcceptanceResult::Accepted(record_id) => record_id,
            SummaryDraftPromotionAcceptanceResult::AlreadyPromoted => return Ok(()),
        };

        if let Err(error) = self.record_checked_context_entry(ContextEntry::summary(summary)) {
            self.summary_draft_promotions.mark_rejected(&record_id);
            return Err(error.into());
        }

        self.summary_draft_promotions.mark_promoted(&record_id);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn summary_draft_promotion_snapshot(&self) -> SummaryDraftPromotionRegistrySnapshot {
        self.summary_draft_promotions.snapshot()
    }

    #[allow(dead_code)]
    pub(crate) fn judgment_records(&self) -> Vec<JudgmentRecord> {
        self.judgments.snapshot().records().to_vec()
    }

    pub(crate) fn pending_tool_calls(&self) -> Vec<PendingToolCall> {
        self.pending_tool_calls.clone()
    }

    pub(crate) fn pending_tool_call(&self, call_id: &ToolCallId) -> Option<PendingToolCall> {
        self.pending_tool_calls
            .iter()
            .find(|call| call.id() == call_id)
            .cloned()
    }

    pub(crate) fn has_pending_tool_calls(&self) -> bool {
        !self.pending_tool_calls.is_empty()
    }

    /// Returns tool call/result pairs not yet covered by a checkpoint.
    ///
    /// These continuations are exact provider-visible protocol history for
    /// stateless calls. They are not ledger projection; future checkpointing
    /// owns when older entries can be removed from compiled context.
    pub(crate) fn uncheckpointed_tool_continuation_snapshots(
        &self,
    ) -> Result<Vec<ResolvedToolContinuationSnapshot>, ArtifactError> {
        self.uncheckpointed_tool_continuations
            .iter()
            .map(|continuation| {
                let content = self
                    .artifacts
                    .read_content(continuation.result.artifact().id())?
                    .clone();
                Ok(ResolvedToolContinuationSnapshot::new(
                    continuation.call.clone(),
                    continuation.result.clone(),
                    content,
                ))
            })
            .collect()
    }

    pub(crate) fn append_only_body_snapshot(
        &self,
    ) -> Result<Vec<CompiledSessionMessage>, ArtifactError> {
        self.append_only_body
            .iter()
            .map(|message| match message {
                SessionMessage::User { text, .. } => {
                    Ok(CompiledSessionMessage::User { text: text.clone() })
                }
                SessionMessage::Assistant { artifact_id, .. } => {
                    let content = self.read_artifact_content(artifact_id)?;
                    let text =
                        content
                            .as_text()
                            .ok_or_else(|| ArtifactError::InvalidEvidenceLocator {
                                id: artifact_id.clone(),
                                reason: "assistant history artifact is not textual",
                            })?;
                    Ok(CompiledSessionMessage::Assistant {
                        text: text.to_owned(),
                    })
                }
            })
            .collect()
    }

    pub(crate) fn read_artifact_content(
        &self,
        artifact_id: &ArtifactId,
    ) -> Result<ArtifactContent, ArtifactError> {
        self.artifacts.read_content(artifact_id).cloned()
    }

    pub(crate) fn record_session_started_if_needed(&mut self) -> Option<RuntimeEvent> {
        if self.session_started {
            return None;
        }

        self.session_started = true;
        Some(self.record_event(
            RuntimeEventKind::SessionStarted,
            LedgerFactKind::SessionStarted,
        ))
    }

    pub(crate) fn record_step_started(&mut self) -> RuntimeEvent {
        self.record_event(RuntimeEventKind::StepStarted, LedgerFactKind::StepStarted)
    }

    pub(crate) fn record_model_retry_event(&mut self, kind: RuntimeEventKind) -> RuntimeEvent {
        self.record_event(kind, LedgerFactKind::ModelRetry)
    }

    pub(crate) fn record_step_completed(&mut self) -> RuntimeEvent {
        self.record_event(
            RuntimeEventKind::StepCompleted,
            LedgerFactKind::StepCompleted,
        )
    }

    pub(crate) fn record_tool_call_pending(
        &mut self,
        call: PendingToolCall,
    ) -> Result<RuntimeEvent, ErrorInfo> {
        if self
            .pending_tool_calls
            .iter()
            .any(|pending| pending.id() == call.id())
        {
            return Err(duplicate_tool_call_diagnostic(call.id(), "already pending"));
        }

        if self.resolved_tool_calls.contains(call.id()) {
            return Err(duplicate_tool_call_diagnostic(
                call.id(),
                "already resolved",
            ));
        }

        self.pending_tool_calls.push(call.clone());
        Ok(self.record_event(
            RuntimeEventKind::ToolCallPending { call },
            LedgerFactKind::ToolCallPending,
        ))
    }

    pub(crate) fn record_bridge_tool_call_requested(
        &mut self,
        call: PendingToolCall,
    ) -> RuntimeEvent {
        self.record_event(
            RuntimeEventKind::BridgeToolCallRequested { call },
            LedgerFactKind::BridgeToolCallRequested,
        )
    }

    pub(crate) fn submit_tool_result(
        &mut self,
        result: ToolCallResult,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let Some(pending_index) = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == result.call_id())
        else {
            return if self.resolved_tool_calls.contains(result.call_id()) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id: result.call_id().clone(),
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id: result.call_id().clone(),
                })
            };
        };

        self.validate_tool_result_content(&result, &content)?;
        let content_bytes = content.as_bytes().len();
        let recorded = self.record_artifact_state(result.artifact().clone(), content)?;
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        debug_assert_eq!(&recorded, result.artifact());

        let pending = self.pending_tool_calls.remove(pending_index);
        let pending_for_skill_event = pending.clone();
        self.resolved_tool_calls.insert(result.call_id().clone());
        let history_id = self.next_history_id();
        self.uncheckpointed_tool_continuations
            .push(ResolvedToolContinuation::new(
                history_id,
                pending,
                result.clone(),
            ));

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));
        events.push(self.record_event(
            RuntimeEventKind::ToolCallResolved {
                result: result.clone(),
            },
            LedgerFactKind::ToolCallResolved,
        ));
        if let Some(event) = self.skill_used_event_for_read(&pending_for_skill_event, &result) {
            events.push(event);
        }

        Ok(events)
    }

    pub(crate) fn submit_tool_execution_outcome(
        &mut self,
        call_id: &ToolCallId,
        status: ToolCallResultStatus,
        content: ArtifactContent,
        diagnostic: Option<ErrorInfo>,
        execution_evidence: Option<ActionExecutionEvidence>,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        debug_assert!(execution_evidence.is_none());
        let artifact_kind = match &content {
            ArtifactContent::Text(_) => ArtifactKind::Text,
            ArtifactContent::Json(_) => ArtifactKind::Json,
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                return Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: self.next_tool_result_artifact_id(),
                    content_kind: content.kind(),
                });
            }
        };
        let artifact = ArtifactRef::new(self.next_tool_result_artifact_id(), artifact_kind);
        let result = match status {
            ToolCallResultStatus::Succeeded => ToolCallResult::new(
                call_id.clone(),
                ToolCallResultStatus::Succeeded,
                artifact,
                diagnostic,
            )?,
            ToolCallResultStatus::Failed => {
                let diagnostic = diagnostic.ok_or(RuntimeError::Core {
                    source: merry_core::CoreError::InvalidToolCallResult {
                        reason: "failed tool execution outcome must include a diagnostic",
                    },
                })?;
                ToolCallResult::failed(call_id.clone(), artifact, diagnostic)
            }
        };

        self.submit_tool_result(result, content)
    }

    pub(crate) fn submit_proposed_tool_execution_outcome(
        &mut self,
        proposal: ActionProposal,
        status: ToolCallResultStatus,
        content: ArtifactContent,
        diagnostic: Option<ErrorInfo>,
        execution_evidence: Option<ActionExecutionEvidence>,
        policy: ActionAuditPolicy,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        self.submit_proposed_tool_execution_outcome_record(ProposedToolExecutionOutcome::new(
            proposal,
            status,
            content,
            diagnostic,
            execution_evidence,
            policy,
        ))
    }

    pub(crate) fn submit_proposed_tool_execution_outcome_record(
        &mut self,
        outcome: ProposedToolExecutionOutcome,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let ProposedToolExecutionOutcome {
            proposal,
            status,
            content,
            diagnostic,
            execution_evidence,
            policy,
            observation,
        } = outcome;
        let call_id = proposal.tool_call_id().clone();
        let Some(pending_index) = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == &call_id)
        else {
            return if self.resolved_tool_calls.contains(&call_id) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id,
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id,
                })
            };
        };

        let artifact_kind = match &content {
            ArtifactContent::Text(_) => ArtifactKind::Text,
            ArtifactContent::Json(_) => ArtifactKind::Json,
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                return Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: self.next_tool_result_artifact_id(),
                    content_kind: content.kind(),
                });
            }
        };
        let artifact = ArtifactRef::new(self.next_tool_result_artifact_id(), artifact_kind);
        let result = match status {
            ToolCallResultStatus::Succeeded => ToolCallResult::new(
                call_id,
                ToolCallResultStatus::Succeeded,
                artifact,
                diagnostic,
            )?,
            ToolCallResultStatus::Failed => {
                let diagnostic = diagnostic.ok_or(RuntimeError::Core {
                    source: merry_core::CoreError::InvalidToolCallResult {
                        reason: "failed tool execution outcome must include a diagnostic",
                    },
                })?;
                ToolCallResult::failed(call_id, artifact, diagnostic)
            }
        };
        let observation =
            observation.map(|observation| observation.into_update_for_artifact(result.artifact()));

        self.validate_tool_result_content(&result, &content)?;
        self.artifacts
            .ensure_recordable(result.artifact(), &content)?;

        let pending = self.pending_tool_calls.remove(pending_index);
        let pending_for_skill_event = pending.clone();
        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        let action_kind = proposal.action_kind();
        self.record_proposed_tool_action_audit(proposal);
        let content_bytes = content.as_bytes().len();
        if let Some(execution_evidence) = execution_evidence {
            self.record_executed_tool_action_audit(
                &pending,
                action_kind,
                policy,
                execution_evidence,
            );
        }
        let recorded = self
            .artifacts
            .record_preflighted(result.artifact().clone(), content);
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        debug_assert_eq!(&recorded, result.artifact());
        self.resolved_tool_calls.insert(result.call_id().clone());
        let history_id = self.next_history_id();
        self.uncheckpointed_tool_continuations
            .push(ResolvedToolContinuation::new(
                history_id,
                pending,
                result.clone(),
            ));

        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));
        if let Some(observation) = observation {
            self.record_tool_result_observation(observation);
        }
        events.push(self.record_event(
            RuntimeEventKind::ToolCallResolved {
                result: result.clone(),
            },
            LedgerFactKind::ToolCallResolved,
        ));
        if let Some(event) = self.skill_used_event_for_read(&pending_for_skill_event, &result) {
            events.push(event);
        }

        Ok(events)
    }

    pub(crate) fn submit_denied_tool_action(
        &mut self,
        pending: &PendingToolCall,
        decision: &ActionPolicyDecision,
        proposal: Option<ActionProposal>,
        content: ArtifactContent,
        diagnostic: ErrorInfo,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        debug_assert!(!decision.is_allowed());

        let Some(pending_index) = self
            .pending_tool_calls
            .iter()
            .position(|call| call.id() == pending.id())
        else {
            return if self.resolved_tool_calls.contains(pending.id()) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id: pending.id().clone(),
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id: pending.id().clone(),
                })
            };
        };

        let artifact_kind = match &content {
            ArtifactContent::Text(_) => ArtifactKind::Text,
            ArtifactContent::Json(_) => ArtifactKind::Json,
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                return Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: self.next_tool_result_artifact_id(),
                    content_kind: content.kind(),
                });
            }
        };
        let artifact = ArtifactRef::new(self.next_tool_result_artifact_id(), artifact_kind);
        let result = ToolCallResult::failed(pending.id().clone(), artifact, diagnostic);

        self.validate_tool_result_content(&result, &content)?;
        self.artifacts
            .ensure_recordable(result.artifact(), &content)?;

        let pending = self.pending_tool_calls.remove(pending_index);
        if let Some(started) = self.record_session_started_if_needed() {
            if let Some(proposal) = proposal {
                self.record_proposed_tool_action_audit(proposal);
            }
            self.record_denied_tool_action_audit(&pending, decision);
            let content_bytes = content.as_bytes().len();
            let artifact = self
                .artifacts
                .record_preflighted(result.artifact().clone(), content);
            Self::trace_artifact_record(self.session_id.as_str(), &artifact, content_bytes);
            debug_assert_eq!(artifact, *result.artifact());
            self.resolved_tool_calls.insert(result.call_id().clone());
            let history_id = self.next_history_id();
            self.uncheckpointed_tool_continuations
                .push(ResolvedToolContinuation::new(
                    history_id,
                    pending,
                    result.clone(),
                ));
            return Ok(vec![
                started,
                self.record_event(
                    RuntimeEventKind::ArtifactRecorded { artifact },
                    LedgerFactKind::ArtifactRecorded,
                ),
                self.record_event(
                    RuntimeEventKind::ToolCallResolved { result },
                    LedgerFactKind::ToolCallResolved,
                ),
            ]);
        }

        if let Some(proposal) = proposal {
            self.record_proposed_tool_action_audit(proposal);
        }
        self.record_denied_tool_action_audit(&pending, decision);
        let content_bytes = content.as_bytes().len();
        let artifact = self
            .artifacts
            .record_preflighted(result.artifact().clone(), content);
        Self::trace_artifact_record(self.session_id.as_str(), &artifact, content_bytes);
        debug_assert_eq!(artifact, *result.artifact());
        self.resolved_tool_calls.insert(result.call_id().clone());
        let history_id = self.next_history_id();
        self.uncheckpointed_tool_continuations
            .push(ResolvedToolContinuation::new(
                history_id,
                pending,
                result.clone(),
            ));
        Ok(vec![
            self.record_event(
                RuntimeEventKind::ArtifactRecorded { artifact },
                LedgerFactKind::ArtifactRecorded,
            ),
            self.record_event(
                RuntimeEventKind::ToolCallResolved { result },
                LedgerFactKind::ToolCallResolved,
            ),
        ])
    }

    pub(crate) fn record_guarded_tool_action(
        &mut self,
        pending: &PendingToolCall,
        action_kind: crate::ToolActionKind,
        policy: ActionAuditPolicy,
    ) -> Result<(), RuntimeError> {
        debug_assert!(action_kind.is_mutating());

        if !self
            .pending_tool_calls
            .iter()
            .any(|call| call.id() == pending.id())
        {
            return if self.resolved_tool_calls.contains(pending.id()) {
                Err(RuntimeError::ToolCallAlreadyResolved {
                    session_id: self.session_id.clone(),
                    call_id: pending.id().clone(),
                })
            } else {
                Err(RuntimeError::UnknownToolCall {
                    session_id: self.session_id.clone(),
                    call_id: pending.id().clone(),
                })
            };
        }

        if self.record_guarded_tool_action_audit(pending, action_kind, policy) {
            self.ledger
                .record_lifecycle(self.next_sequence, LedgerFactKind::ActionAuditRecorded);
        }
        Ok(())
    }

    pub(crate) fn record_cancelled(&mut self, diagnostic: ErrorInfo) -> RuntimeEvent {
        self.record_event(
            RuntimeEventKind::Cancelled { diagnostic },
            LedgerFactKind::Cancelled,
        )
    }

    pub(crate) fn record_failed(&mut self, diagnostic: ErrorInfo) -> RuntimeEvent {
        self.record_event(
            RuntimeEventKind::Failed { diagnostic },
            LedgerFactKind::Failed,
        )
    }

    fn record_event(&mut self, kind: RuntimeEventKind, fact_kind: LedgerFactKind) -> RuntimeEvent {
        let sequence = self.next_sequence;
        self.ledger.record(sequence, fact_kind);
        self.next_sequence += 1;
        RuntimeEvent::new(self.session_id.clone(), sequence, kind)
    }

    fn skill_used_event_for_read(
        &mut self,
        pending: &PendingToolCall,
        result: &ToolCallResult,
    ) -> Option<RuntimeEvent> {
        if result.status() != ToolCallResultStatus::Succeeded {
            return None;
        }
        if pending.name().as_str() != WORKSPACE_READ_FILE_TOOL_NAME {
            return None;
        }
        let path = pending
            .arguments()
            .as_object()
            .get("path")
            .and_then(serde_json::Value::as_str)?;
        let (skill_name, skill_md_path) = {
            let skill = self.skill_catalog.as_ref()?.find_by_skill_md_path(path)?;
            (
                skill.name().to_owned(),
                skill.skill_md_path().display().to_string(),
            )
        };

        Some(self.record_event(
            RuntimeEventKind::SkillUsed {
                skill_name,
                skill_md_path,
                tool_call_id: pending.id().clone(),
                artifact: result.artifact().clone(),
            },
            LedgerFactKind::SkillUsed,
        ))
    }

    fn record_tool_result_observation(&mut self, observation: LedgerUpdateKind) {
        self.ledger.append(observation);
    }

    fn trace_artifact_record(session_id: &str, artifact: &ArtifactRef, byte_count: usize) {
        tracing::info!(
            event = "runtime.artifact.record",
            session_id,
            artifact_id = artifact.id().as_str(),
            artifact_kind = ?artifact.kind(),
            byte_count,
            "runtime artifact recorded"
        );
    }

    fn record_denied_tool_action_audit(
        &mut self,
        pending: &PendingToolCall,
        decision: &ActionPolicyDecision,
    ) {
        self.action_audits
            .record_denied_tool_action(pending, decision);
        self.ledger
            .record_lifecycle(self.next_sequence, LedgerFactKind::ActionAuditRecorded);
    }

    fn record_proposed_tool_action_audit(&mut self, proposal: ActionProposal) {
        self.action_audits.record_proposed_tool_action(proposal);
        self.ledger
            .record_lifecycle(self.next_sequence, LedgerFactKind::ActionAuditRecorded);
    }

    fn record_executed_tool_action_audit(
        &mut self,
        pending: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
        evidence: ActionExecutionEvidence,
    ) {
        self.action_audits
            .record_executed_tool_action(pending, action_kind, policy, evidence);
        self.ledger
            .record_lifecycle(self.next_sequence, LedgerFactKind::ActionAuditRecorded);
    }

    fn record_guarded_tool_action_audit(
        &mut self,
        pending: &PendingToolCall,
        action_kind: ToolActionKind,
        policy: ActionAuditPolicy,
    ) -> bool {
        self.action_audits
            .record_guarded_tool_action(pending, action_kind, policy)
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    fn next_history_id(&mut self) -> u64 {
        let id = self.next_history_id;
        self.next_history_id = self.next_history_id.wrapping_add(1);
        id
    }

    #[cfg(test)]
    fn history_item_ids(&self) -> Vec<u64> {
        let mut ids = self
            .append_only_body
            .iter()
            .map(|message| match message {
                SessionMessage::User { history_id, .. }
                | SessionMessage::Assistant { history_id, .. } => *history_id,
            })
            .chain(
                self.uncheckpointed_tool_continuations
                    .iter()
                    .map(|continuation| continuation.history_id),
            )
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    #[cfg(test)]
    pub(crate) fn append_only_body_text_for_tests(&self) -> Vec<String> {
        self.append_only_body
            .iter()
            .map(|message| match message {
                SessionMessage::User { text, .. } => text.clone(),
                SessionMessage::Assistant { artifact_id, .. } => self
                    .read_artifact_content(artifact_id)
                    .expect("assistant artifact should be readable")
                    .as_text()
                    .expect("assistant artifact should be text")
                    .to_owned(),
            })
            .collect()
    }

    fn next_tool_result_artifact_id(&self) -> ArtifactId {
        tool_result_id(self.next_sequence())
    }

    fn validate_tool_result_content(
        &self,
        result: &ToolCallResult,
        content: &ArtifactContent,
    ) -> Result<(), RuntimeError> {
        let supported = matches!(content, ArtifactContent::Text(_) | ArtifactContent::Json(_));
        let compatible = matches!(
            (result.artifact().kind(), content),
            (ArtifactKind::Text, ArtifactContent::Text(_))
                | (ArtifactKind::Json, ArtifactContent::Json(_))
        );

        if !supported {
            return Err(RuntimeError::UnsupportedToolResultContent {
                artifact_id: result.artifact().id().clone(),
                content_kind: content.kind(),
            });
        }

        if !compatible {
            return Err(ArtifactError::IncompatibleContent {
                id: result.artifact().id().clone(),
                artifact_kind: result.artifact().kind().clone(),
                content_kind: content.kind(),
            }
            .into());
        }

        match content {
            ArtifactContent::Text(text) | ArtifactContent::Json(text) if text.trim().is_empty() => {
                Err(RuntimeError::UnsupportedToolResultContent {
                    artifact_id: result.artifact().id().clone(),
                    content_kind: content.kind(),
                })
            }
            ArtifactContent::Text(_) | ArtifactContent::Json(_) => Ok(()),
            ArtifactContent::Binary(_) | ArtifactContent::Image(_) | ArtifactContent::Other(_) => {
                unreachable!("unsupported tool result content is rejected before blank validation")
            }
        }
    }

    #[allow(dead_code)]
    fn validate_judgment_evidence(
        &self,
        request: &JudgmentRequest,
        outcome: &JudgmentOutcome,
    ) -> Result<(), JudgmentError> {
        self.validate_judgment_evidence_refs(request.evidence().iter().chain(outcome.evidence()))
    }

    #[allow(dead_code)]
    fn validate_judgment_evidence_refs<'a>(
        &self,
        evidence_refs: impl IntoIterator<Item = &'a JudgmentEvidence>,
    ) -> Result<(), JudgmentError> {
        for evidence in evidence_refs {
            self.artifacts
                .validate_evidence(evidence.reference())
                .map_err(|source| JudgmentError::UnreadableEvidence {
                    artifact_id: evidence.reference().artifact_id.clone(),
                    source,
                })?;
        }

        Ok(())
    }
}

fn duplicate_tool_call_diagnostic(call_id: &ToolCallId, state: &'static str) -> ErrorInfo {
    ErrorInfo::new(
        "tool_call_duplicate",
        &format!("tool call {call_id} is {state}; duplicate pending admission rejected"),
    )
    .expect("duplicate tool call diagnostic uses static code and validated call id")
}

#[cfg(test)]
mod tests {
    use super::{ProposedToolExecutionOutcome, SessionState, ToolResultLedgerObservation};
    use crate::{
        ActionExecutionEvidence, ActionProposal, ActionProposalEvidence, CitationCompactionPolicy,
        RuntimeError, TaskAnchor, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
        action_audit::{ActionAuditPolicy, ActionAuditStatus},
        action_policy::{ActionPolicyDisposition, ActionRiskTier, DefaultActionPolicy},
        artifact::{ArtifactContent, ArtifactError},
        checkpoint::{
            CheckpointId, CheckpointRef, CheckpointRefId, CheckpointRefManifest,
            CheckpointSequenceRange, CheckpointSourceKind, CheckpointValidationPolicy,
            CitationBackedCheckpoint, CompactedCheckpointCandidate,
        },
        context::{ContextCompiler, ContextEntry, ContextError, ContextEvidence, ContextSummary},
        judgment::{
            JudgmentConfidence, JudgmentError, JudgmentEvidence, JudgmentOutcome,
            JudgmentProvenance, JudgmentPurpose, JudgmentRecommendation, JudgmentRecordId,
            JudgmentRiskLevel, JudgmentSourceKind, SummaryDraftAcceptance,
            SummaryDraftAcceptanceAuthority, SummaryDraftPromotionError,
            SummaryDraftPromotionInput,
        },
        ledger::{LedgerFactKind, LedgerProjection, LedgerScope},
        memory::{
            ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason,
            MemoryActivationScore, MemoryActivationSourceKind, MemoryEvidence, MemoryId,
            MemoryItem, MemoryItemSelection, MemoryScope,
        },
        summary_draft_promotion::SummaryDraftPromotionState,
    };
    use merry_core::{
        ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, EvidenceLocator, EvidenceRef,
        PendingToolCall, RuntimeEventKind, SessionId, ToolCallArguments, ToolCallId,
        ToolCallResult, ToolName,
    };
    use serde_json::json;

    fn session_id() -> SessionId {
        SessionId::new("session-state-test").expect("valid session id")
    }

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("valid artifact id")
    }

    fn tool_call_id(value: &str) -> ToolCallId {
        ToolCallId::new(value).expect("valid tool call id")
    }

    fn pending_tool_call(id: &str) -> PendingToolCall {
        PendingToolCall::new(
            tool_call_id(id),
            ToolName::new("lookup").expect("valid tool name"),
            ToolCallArguments::try_from(json!({ "query": "value" }))
                .expect("object arguments are valid"),
        )
    }

    fn citation_plain_runtime_checkpoint_for_tests(
        checkpoint_id: &str,
        text: &str,
    ) -> crate::CompactedCheckpoint {
        let manifest = CheckpointRefManifest::new(
            CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
            vec![
                CheckpointRef::new(
                    CheckpointRefId::new("r1").expect("valid ref id"),
                    CheckpointSourceKind::UserMessage,
                    "history:1",
                    CheckpointSequenceRange::new(1, 1).expect("valid range"),
                    "body[0]",
                    text,
                )
                .expect("valid ref"),
            ],
        )
        .expect("valid manifest");
        let candidate = CompactedCheckpointCandidate::from_json(&format!(
            r#"{{
              "claims": [
                {{
                  "id": "c1",
                  "kind": "constraint",
                  "text": {text_json},
                  "refs": ["r1"]
                }}
              ],
              "working_intent": null
            }}"#,
            text_json = serde_json::to_string(text).expect("text serializes"),
        ))
        .expect("candidate parses");
        let checkpoint = CitationBackedCheckpoint::from_candidate(
            CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
            candidate,
            manifest,
            CheckpointValidationPolicy::default(),
        )
        .expect("citation checkpoint builds");
        crate::CompactedCheckpoint::from_citation_backed(checkpoint)
            .expect("compacted checkpoint builds")
    }

    fn judgment_evidence(label: &str, id: &str, locator: EvidenceLocator) -> JudgmentEvidence {
        JudgmentEvidence::new(label, EvidenceRef::new(artifact_id(id), locator))
            .expect("valid judgment evidence")
    }

    fn judgment_constraints() -> Vec<String> {
        vec!["advisory semantic signal only".to_owned()]
    }

    fn judgment_provenance() -> JudgmentProvenance {
        JudgmentProvenance::new(JudgmentSourceKind::Test, "session test source")
            .expect("valid judgment provenance")
    }

    fn judgment_confidence(value: f32) -> JudgmentConfidence {
        JudgmentConfidence::new(value).expect("valid judgment confidence")
    }

    fn memory_relevance_request(
        evidence: Vec<JudgmentEvidence>,
    ) -> crate::judgment::JudgmentRequest {
        crate::judgment::JudgmentRequest::new(
            JudgmentPurpose::MemoryRelevance,
            "candidate memory",
            "Is this memory relevant?",
            evidence,
            judgment_constraints(),
            "session test request",
        )
        .expect("valid memory relevance request")
    }

    fn summary_draft_request(evidence: Vec<JudgmentEvidence>) -> crate::judgment::JudgmentRequest {
        crate::judgment::JudgmentRequest::new(
            JudgmentPurpose::SummaryDraft,
            "session summary",
            "Draft from exact evidence.",
            evidence,
            judgment_constraints(),
            "session test request",
        )
        .expect("valid summary draft request")
    }

    fn summary_draft_outcome_with_draft(
        evidence: Vec<JudgmentEvidence>,
        draft: impl Into<String>,
    ) -> JudgmentOutcome {
        JudgmentOutcome::new(
            JudgmentPurpose::SummaryDraft,
            JudgmentRecommendation::SummaryDraft {
                draft: draft.into(),
            },
            judgment_confidence(0.8),
            evidence,
            "The draft is grounded in readable evidence.",
            "The source is partial.",
            judgment_provenance(),
        )
        .expect("valid summary draft outcome")
    }

    fn summary_draft_outcome(evidence: Vec<JudgmentEvidence>) -> JudgmentOutcome {
        summary_draft_outcome_with_draft(evidence, "Draft from readable evidence.")
    }

    fn high_tool_risk_request() -> crate::judgment::JudgmentRequest {
        crate::judgment::JudgmentRequest::new(
            JudgmentPurpose::ToolRiskReview,
            "pending lookup tool",
            "Review whether the lookup input has semantic risk.",
            Vec::new(),
            judgment_constraints(),
            "session test request",
        )
        .expect("valid tool risk request")
    }

    fn high_tool_risk_outcome() -> JudgmentOutcome {
        JudgmentOutcome::new(
            JudgmentPurpose::ToolRiskReview,
            JudgmentRecommendation::ToolRiskReview {
                risk: JudgmentRiskLevel::High,
                concerns: vec!["Input references credential-like material.".to_owned()],
            },
            judgment_confidence(0.95),
            Vec::new(),
            "Credential-like input is semantically risky.",
            "This is advisory and not a hard policy decision.",
            judgment_provenance(),
        )
        .expect("valid high risk outcome")
    }

    fn memory_id(value: &str) -> MemoryId {
        MemoryId::new(value).expect("valid memory id")
    }

    fn promotion_input(
        summary_id: &str,
        draft_text: &str,
        evidence: Vec<JudgmentEvidence>,
    ) -> SummaryDraftPromotionInput {
        promotion_input_with_source_record_id(summary_id, draft_text, evidence, None)
    }

    fn promotion_input_with_source_record_id(
        summary_id: &str,
        draft_text: &str,
        evidence: Vec<JudgmentEvidence>,
        source_record_id: Option<JudgmentRecordId>,
    ) -> SummaryDraftPromotionInput {
        SummaryDraftPromotionInput::new(
            summary_id,
            draft_text,
            evidence,
            SummaryDraftAcceptance::new(
                SummaryDraftAcceptanceAuthority::HardPolicy,
                "session hard policy",
                "Hard policy accepted the draft for context promotion.",
            )
            .expect("valid promotion acceptance"),
            source_record_id,
        )
        .expect("valid promotion input")
    }

    fn assert_single_promotion_record(
        session: &SessionState,
        summary_id: &str,
        state: SummaryDraftPromotionState,
        source_record_id: Option<&str>,
    ) {
        let snapshot = session.summary_draft_promotion_snapshot();
        assert_eq!(snapshot.records().len(), 1);
        let record = &snapshot.records()[0];
        assert_eq!(
            record.id().as_str(),
            "summary-draft-promotion-00000000000000000000"
        );
        assert_eq!(record.summary_id(), summary_id);
        assert_eq!(record.state(), state);
        assert_eq!(record.commit_order(), 0);
        assert_eq!(
            record.source_record_id().map(JudgmentRecordId::as_str),
            source_record_id
        );
    }

    fn activated_memory(id: &str) -> ActivatedMemory {
        activated_memory_with_details(id, format!("{id} text"), 1, 0, 0.5)
    }

    fn activated_memory_with_details(
        id: &str,
        text: impl Into<String>,
        matches: usize,
        priority: i32,
        confidence: f32,
    ) -> ActivatedMemory {
        let item = MemoryItem::new(
            memory_id(id),
            MemoryScope::Session,
            text,
            vec![memory_evidence("primary source", &format!("{id}-artifact"))],
            MemoryItemSelection::new(vec!["topic".to_owned()], confidence, priority, None)
                .expect("valid memory selection"),
        )
        .expect("valid memory item");
        let score =
            MemoryActivationScore::new(matches, priority, confidence).expect("valid memory score");
        ActivatedMemory::new(
            item,
            score,
            vec![
                MemoryActivationReason::ScopeAllowed,
                MemoryActivationReason::trigger_matched("topic").expect("valid trigger"),
                MemoryActivationReason::ranked(score),
            ],
            provenance(),
        )
        .expect("valid activated memory")
    }

    fn provenance() -> MemoryActivationProvenance {
        MemoryActivationProvenance::new(
            "topic",
            vec![MemoryScope::Session, MemoryScope::Task, MemoryScope::Step],
            MemoryActivationSourceKind::UserQuery,
            "user request",
        )
        .expect("valid memory provenance")
    }

    fn memory_evidence(label: &str, artifact: &str) -> MemoryEvidence {
        MemoryEvidence::new(
            label,
            EvidenceRef::new(artifact_id(artifact), EvidenceLocator::whole_artifact()),
        )
        .expect("valid memory evidence")
    }

    fn record_memory_artifacts(session: &mut SessionState, memories: &[&ActivatedMemory]) {
        let mut seen = std::collections::BTreeSet::new();

        for memory in memories {
            for evidence in memory.item().evidence() {
                if !seen.insert(evidence.reference().artifact_id.clone()) {
                    continue;
                }

                session
                    .record_artifact_state(
                        ArtifactRef::new(
                            evidence.reference().artifact_id.clone(),
                            ArtifactKind::Text,
                        ),
                        ArtifactContent::text(format!(
                            "evidence for {}\n{}",
                            memory.item().id(),
                            memory.item().text()
                        )),
                    )
                    .expect("memory artifact records");
            }
        }
    }

    #[test]
    fn session_start_is_recorded_once_before_step_lifecycle() {
        let mut session = SessionState::new(session_id());

        let first = session
            .record_session_started_if_needed()
            .expect("first start should emit");
        let second = session.record_session_started_if_needed();
        let started = session.record_step_started();
        let completed = session.record_step_completed();

        assert!(matches!(first.kind, RuntimeEventKind::SessionStarted));
        assert!(second.is_none());
        assert_eq!(started.sequence, 1);
        assert_eq!(completed.sequence, 2);
    }

    #[test]
    fn assistant_output_artifact_id_uses_artifact_event_sequence() {
        let mut session = SessionState::new(session_id());
        let _started = session
            .record_session_started_if_needed()
            .expect("start should emit");
        let _step_started = session.record_step_started();

        let artifact = session
            .record_assistant_text_output("hello".to_owned())
            .expect("assistant output should record");
        let completed = session.record_step_completed();

        match artifact.kind {
            RuntimeEventKind::ArtifactRecorded { artifact } => {
                assert_eq!(artifact.id().as_str(), "assistant-output-2");
                assert_eq!(artifact.kind(), &ArtifactKind::Text);
            }
            other => panic!("expected artifact event, got {other:?}"),
        }
        assert_eq!(artifact.sequence, 2);
        assert_eq!(completed.sequence, 3);
    }

    #[test]
    fn submit_tool_result_stores_exact_content_before_resolved_event() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("call-1");
        session
            .record_tool_call_pending(call.clone())
            .expect("pending call should record");
        let artifact = ArtifactRef::new(artifact_id("tool-result-exact"), ArtifactKind::Text);
        let result = ToolCallResult::succeeded(call.id().clone(), artifact.clone());

        let events = session
            .submit_tool_result(result.clone(), ArtifactContent::text("exact result\n"))
            .expect("tool result should submit");

        assert_eq!(
            session
                .read_artifact_content(artifact.id())
                .expect("recorded content should be readable"),
            ArtifactContent::text("exact result\n")
        );
        assert!(matches!(
            events.last().map(|event| &event.kind),
            Some(RuntimeEventKind::ToolCallResolved { result: resolved }) if resolved == &result
        ));
    }

    #[test]
    fn session_history_ids_increase_across_messages_and_tool_continuations() {
        let mut session =
            SessionState::new(SessionId::new("history-order").expect("valid session id"));
        session.record_user_message_body("first user");
        session
            .record_assistant_text_output("first assistant".to_owned())
            .expect("assistant output records");

        let call = pending_tool_call("call-history");
        session
            .record_tool_call_pending(call.clone())
            .expect("tool call pending records");
        let artifact = ArtifactRef::new(artifact_id("tool-result-history"), ArtifactKind::Text);
        let result = ToolCallResult::succeeded(call.id().clone(), artifact);
        session
            .submit_tool_result(result, ArtifactContent::text("tool result"))
            .expect("tool result records");

        let ids = session.history_item_ids();
        assert_eq!(ids, vec![0, 1, 2]);
    }

    #[test]
    fn compaction_input_excludes_retained_raw_tail() {
        let mut session =
            SessionState::new(SessionId::new("compaction-input-tail").expect("valid session id"));
        session.set_task_anchor(TaskAnchor::new("Keep the current task").expect("valid anchor"));
        session.record_user_message_body("old user message to compact");
        session
            .record_assistant_text_output("old assistant message to compact".to_owned())
            .expect("assistant records");
        session.record_user_message_body("retained raw tail user sentinel");
        session
            .record_assistant_text_output("retained raw tail assistant sentinel".to_owned())
            .expect("assistant records");

        let policy =
            CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy");
        let input = session
            .build_citation_compaction_input(policy)
            .expect("input builds")
            .expect("old prefix should be compressible");
        let payload = input.to_model_payload_json().expect("payload serializes");

        assert!(payload.contains("old user message to compact"));
        assert!(payload.contains("old assistant message to compact"));
        assert!(!payload.contains("retained raw tail user sentinel"));
        assert!(!payload.contains("retained raw tail assistant sentinel"));
        assert!(payload.contains("\"current_user_input_excluded\":true"));
    }

    #[test]
    fn compaction_retained_raw_tail_is_policy_driven() {
        let mut session =
            SessionState::new(SessionId::new("retained-tail-policy").expect("valid session id"));
        session.record_user_message_body("covered user sentinel");
        session
            .record_assistant_text_output("covered assistant sentinel".to_owned())
            .expect("assistant records");
        session.record_user_message_body("tail user one sentinel");
        session
            .record_assistant_text_output("tail assistant one sentinel".to_owned())
            .expect("assistant records");
        session.record_user_message_body("tail user two sentinel");
        session
            .record_assistant_text_output("tail assistant two sentinel".to_owned())
            .expect("assistant records");

        let input = session
            .build_citation_compaction_input(
                CitationCompactionPolicy::new(128, None, 4096, 4, 1200, 16).expect("valid policy"),
            )
            .expect("input builds")
            .expect("old prefix should be compressible");
        let payload = input.to_model_payload_json().expect("payload serializes");

        assert!(payload.contains("covered user sentinel"));
        assert!(payload.contains("covered assistant sentinel"));
        assert!(!payload.contains("tail user one sentinel"));
        assert!(!payload.contains("tail assistant one sentinel"));
        assert!(!payload.contains("tail user two sentinel"));
        assert!(!payload.contains("tail assistant two sentinel"));
    }

    #[test]
    fn compaction_input_includes_previous_checkpoint_without_old_raw_body() {
        let mut session =
            SessionState::new(SessionId::new("rolling-input").expect("valid session id"));
        let checkpoint = citation_plain_runtime_checkpoint_for_tests(
            "checkpoint-existing",
            "The prior direction rejected resource timelines.",
        );
        session.set_compacted_checkpoint(checkpoint);
        session.record_user_message_body("new user message to compact");
        session.record_user_message_body("retained tail");

        let input = session
            .build_citation_compaction_input(
                CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
            )
            .expect("input builds")
            .expect("input exists");
        let payload = input.to_model_payload_json().expect("payload serializes");

        assert!(payload.contains("previous_checkpoint"));
        assert!(payload.contains("The prior direction rejected resource timelines."));
        assert!(payload.contains("new user message to compact"));
        assert!(!payload.contains("retained tail"));
    }

    #[test]
    fn rolling_compaction_candidate_can_cite_prior_claim_and_new_window_ref() {
        let mut session =
            SessionState::new(SessionId::new("rolling-install").expect("valid session id"));
        let checkpoint = citation_plain_runtime_checkpoint_for_tests(
            "checkpoint-existing",
            "Runtime cannot validate open semantic truth.",
        );
        session.set_compacted_checkpoint(checkpoint);
        session.record_user_message_body("new compacted work");
        session.record_user_message_body("retained tail");

        let input = session
            .build_citation_compaction_input(
                CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy"),
            )
            .expect("input builds")
            .expect("input exists");
        let checkpoint_id = input.manifest().checkpoint_id().clone();

        session
            .install_citation_compaction_candidate(
                input,
                r#"{
                  "claims": [
                    {
                      "id": "c2",
                      "kind": "constraint",
                      "text": "Carry the prior semantic-validation constraint while adding new compacted work.",
                      "refs": ["prior-c1", "r1"]
                    }
                  ],
                  "working_intent": null
                }"#,
            )
            .expect("install succeeds with prior and new refs");

        let prior_excerpt = session
            .read_checkpoint_ref(
                &checkpoint_id,
                &CheckpointRefId::new("prior-c1").expect("valid ref id"),
            )
            .expect("prior claim ref remains inspectable");
        let new_excerpt = session
            .read_checkpoint_ref(
                &checkpoint_id,
                &CheckpointRefId::new("r1").expect("valid ref id"),
            )
            .expect("new window ref remains inspectable");

        assert_eq!(
            prior_excerpt.source_kind(),
            CheckpointSourceKind::PriorCheckpointClaim
        );
        assert!(prior_excerpt.excerpt().contains("open semantic truth"));
        assert_eq!(new_excerpt.source_kind(), CheckpointSourceKind::UserMessage);
        assert_eq!(new_excerpt.excerpt(), "new compacted work");
    }

    #[test]
    fn installing_valid_checkpoint_removes_only_covered_history() {
        let mut session =
            SessionState::new(SessionId::new("install-checkpoint").expect("valid session id"));
        session.record_user_message_body("old user");
        session
            .record_assistant_text_output("old assistant".to_owned())
            .expect("assistant records");
        session.record_user_message_body("tail user");

        let policy =
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy");
        let input = session
            .build_citation_compaction_input(policy)
            .expect("input builds")
            .expect("input exists");
        let candidate_json = r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "The older user and assistant messages were covered by compaction.",
              "refs": ["r1", "r2"]
            }
          ],
          "working_intent": null
        }"#;

        let outcome = session
            .install_citation_compaction_candidate(input, candidate_json)
            .expect("install succeeds");

        assert_eq!(outcome.covered_history_item_count(), 2);
        assert_eq!(session.append_only_body_text_for_tests(), vec!["tail user"]);
        assert!(
            session
                .context_snapshot()
                .compacted_checkpoint_for_tests()
                .is_some()
        );
    }

    #[test]
    fn failed_checkpoint_install_keeps_history_unchanged() {
        let mut session = SessionState::new(
            SessionId::new("install-checkpoint-rollback").expect("valid session id"),
        );
        session.record_user_message_body("old user");
        session.record_user_message_body("tail user");

        let policy =
            CitationCompactionPolicy::new(128, None, 4096, 1, 1200, 16).expect("valid policy");
        let input = session
            .build_citation_compaction_input(policy)
            .expect("input builds")
            .expect("input exists");
        let bad_candidate_json = r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "constraint",
              "text": "This cites a missing ref.",
              "refs": ["r-missing"]
            }
          ],
          "working_intent": null
        }"#;

        let error = session
            .install_citation_compaction_candidate(input, bad_candidate_json)
            .expect_err("bad candidate must fail");

        assert!(matches!(error, RuntimeError::Checkpoint { .. }));
        assert_eq!(
            session.append_only_body_text_for_tests(),
            vec!["old user", "tail user"]
        );
    }

    #[test]
    fn denied_tool_action_records_audit_lifecycle_before_artifact_and_resolution() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("denied-action-call");
        let decision = DefaultActionPolicy.decide(crate::ToolActionKind::WorkspaceWrite);
        let diagnostic = ErrorInfo::new("action_policy_denied", "blocked by test policy")
            .expect("valid diagnostic");
        session
            .record_session_started_if_needed()
            .expect("session should start");
        session
            .record_tool_call_pending(call.clone())
            .expect("pending call should record");

        let events = session
            .submit_denied_tool_action(
                &call,
                &decision,
                None,
                ArtifactContent::json(r#"{"ok":false}"#),
                diagnostic,
            )
            .expect("denial should resolve");

        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].kind,
            RuntimeEventKind::ArtifactRecorded { .. }
        ));
        assert!(matches!(
            events[1].kind,
            RuntimeEventKind::ToolCallResolved { .. }
        ));

        let audit_snapshot = session.action_audit_snapshot();
        assert_eq!(audit_snapshot.records().len(), 1);
        let audit = &audit_snapshot.records()[0];
        assert_eq!(audit.id().as_str(), "action-audit-00000000000000000000");
        assert_eq!(audit.order(), 0);
        assert_eq!(audit.tool_call_id(), call.id());
        assert_eq!(audit.tool_name(), call.name());
        assert_eq!(audit.action_kind(), crate::ToolActionKind::WorkspaceWrite);
        assert_eq!(audit.status(), ActionAuditStatus::Denied);
        let policy = audit.policy().expect("denied audit should include policy");
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
        assert_eq!(policy.risk_tier(), decision.risk_tier());
        assert_eq!(policy.reason(), decision.reason());

        let projection = session.ledger_projection();
        let lifecycle_kinds = projection
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                LedgerProjection::Lifecycle { kind, .. } => Some(*kind),
                LedgerProjection::Fact { .. } => None,
            })
            .collect::<Vec<_>>();
        let audit_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == LedgerFactKind::ActionAuditRecorded)
            .expect("audit lifecycle should be recorded");
        let artifact_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
            .expect("artifact lifecycle should be recorded");
        let resolved_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
            .expect("resolution lifecycle should be recorded");
        assert!(audit_index < artifact_index);
        assert!(artifact_index < resolved_index);
        assert!(session.pending_tool_calls().is_empty());
    }

    #[test]
    fn proposed_tool_execution_records_executed_audit_before_artifact_and_resolution() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("executed-action-call");
        session
            .record_session_started_if_needed()
            .expect("session should start");
        session
            .record_tool_call_pending(call.clone())
            .expect("pending call should record");

        let proposal_evidence = ActionProposalEvidence::WorkspacePatch(
            WorkspacePatchProposal::new(
                "note.txt",
                3,
                5,
                16,
                18,
                "fnv1a64:0000000000000100",
                "fnv1a64:0000000000000101",
            )
            .expect("valid workspace patch proposal"),
        );
        let proposal = ActionProposal::new(
            &call,
            crate::ToolActionKind::WorkspaceWrite,
            "workspace patch",
            "note.txt",
            "Replace one preimage in note.txt.",
            proposal_evidence,
        )
        .expect("valid action proposal");
        let execution_evidence = ActionExecutionEvidence::WorkspacePatch(
            WorkspacePatchExecutionEvidence::new(
                "note.txt",
                3,
                5,
                16,
                18,
                "fnv1a64:0000000000000100",
                "fnv1a64:0000000000000101",
            )
            .expect("valid execution evidence"),
        );
        let policy = ActionAuditPolicy::new(
            ActionRiskTier::EditLow,
            ActionPolicyDisposition::Allow,
            "test low-risk workspace patch allow",
        );

        let events = session
            .submit_proposed_tool_execution_outcome(
                proposal,
                merry_core::ToolCallResultStatus::Succeeded,
                ArtifactContent::json(r#"{"ok":true}"#),
                None,
                Some(execution_evidence.clone()),
                policy,
            )
            .expect("proposed execution should resolve");

        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].kind,
            RuntimeEventKind::ArtifactRecorded { .. }
        ));
        assert!(matches!(
            events[1].kind,
            RuntimeEventKind::ToolCallResolved { .. }
        ));

        let audit_snapshot = session.action_audit_snapshot();
        assert_eq!(audit_snapshot.records().len(), 2);
        assert_eq!(
            audit_snapshot.records()[0].status(),
            ActionAuditStatus::Proposed
        );
        assert_eq!(
            audit_snapshot.records()[1].status(),
            ActionAuditStatus::Executed
        );
        assert!(audit_snapshot.records()[0].proposal().is_some());
        assert!(audit_snapshot.records()[0].execution_evidence().is_none());
        assert!(audit_snapshot.records()[1].proposal().is_none());
        assert_eq!(
            audit_snapshot.records()[1]
                .execution_evidence()
                .expect("executed audit should include evidence"),
            &execution_evidence
        );

        let lifecycle_kinds = session
            .ledger_projection()
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                LedgerProjection::Lifecycle { kind, .. } => Some(*kind),
                LedgerProjection::Fact { .. } => None,
            })
            .collect::<Vec<_>>();
        let audit_indexes = lifecycle_kinds
            .iter()
            .enumerate()
            .filter_map(|(index, kind)| {
                (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(audit_indexes.len(), 2);
        let artifact_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
            .expect("artifact lifecycle should be recorded");
        let resolved_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
            .expect("resolution lifecycle should be recorded");
        assert!(audit_indexes[0] < audit_indexes[1]);
        assert!(audit_indexes[1] < artifact_index);
        assert!(artifact_index < resolved_index);
    }

    #[test]
    fn proposed_tool_execution_can_record_observation_after_artifact_before_resolution() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("observed-action-call");
        session
            .record_session_started_if_needed()
            .expect("session should start");
        session
            .record_tool_call_pending(call.clone())
            .expect("pending call should record");

        let proposal_evidence = ActionProposalEvidence::WorkspacePatch(
            WorkspacePatchProposal::new(
                "note.txt",
                3,
                5,
                16,
                18,
                "fnv1a64:0000000000000100",
                "fnv1a64:0000000000000101",
            )
            .expect("valid workspace patch proposal"),
        );
        let proposal = ActionProposal::new(
            &call,
            crate::ToolActionKind::WorkspaceWrite,
            "workspace patch",
            "note.txt",
            "Replace one preimage in note.txt.",
            proposal_evidence,
        )
        .expect("valid action proposal");
        let execution_evidence = ActionExecutionEvidence::WorkspacePatch(
            WorkspacePatchExecutionEvidence::new(
                "note.txt",
                3,
                5,
                16,
                18,
                "fnv1a64:0000000000000100",
                "fnv1a64:0000000000000101",
            )
            .expect("valid execution evidence"),
        );
        let policy = ActionAuditPolicy::new(
            ActionRiskTier::EditLow,
            ActionPolicyDisposition::Allow,
            "test low-risk workspace patch allow",
        );
        let observation = ToolResultLedgerObservation::new(
            LedgerScope::Tool,
            "process action `rustc --version` exit code 0; stdout_bytes=21; stderr_bytes=0",
        )
        .expect("valid compact observation");

        let events = session
            .submit_proposed_tool_execution_outcome_record(
                ProposedToolExecutionOutcome::new(
                    proposal,
                    merry_core::ToolCallResultStatus::Succeeded,
                    ArtifactContent::json(r#"{"ok":true}"#),
                    None,
                    Some(execution_evidence),
                    policy,
                )
                .with_observation(observation),
            )
            .expect("observed proposed execution should resolve");
        let result = events
            .iter()
            .find_map(|event| match &event.kind {
                RuntimeEventKind::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("tool result should resolve");

        let projection = session.ledger_projection();
        let artifact_order = projection
            .entries()
            .iter()
            .find_map(|entry| match entry {
                LedgerProjection::Lifecycle {
                    kind: LedgerFactKind::ArtifactRecorded,
                    order,
                    ..
                } => Some(*order),
                LedgerProjection::Lifecycle { .. } | LedgerProjection::Fact { .. } => None,
            })
            .expect("artifact lifecycle should be recorded");
        let resolved_order = projection
            .entries()
            .iter()
            .find_map(|entry| match entry {
                LedgerProjection::Lifecycle {
                    kind: LedgerFactKind::ToolCallResolved,
                    order,
                    ..
                } => Some(*order),
                LedgerProjection::Lifecycle { .. } | LedgerProjection::Fact { .. } => None,
            })
            .expect("resolution lifecycle should be recorded");
        let (observation_order, observation_text) = projection
            .entries()
            .iter()
            .find_map(|entry| match entry {
                LedgerProjection::Fact {
                    order,
                    scope: LedgerScope::Tool,
                    text,
                    ..
                } if text.starts_with("process action `rustc --version`") => {
                    Some((*order, text.as_str()))
                }
                LedgerProjection::Fact { .. } | LedgerProjection::Lifecycle { .. } => None,
            })
            .expect("tool observation should be projected");

        assert!(artifact_order < observation_order);
        assert!(observation_order < resolved_order);
        assert!(
            observation_text.contains(&format!("artifact={}", result.artifact().id().as_str()))
        );
    }

    #[test]
    fn guarded_tool_action_records_internal_audit_without_events_or_resolution() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("guarded-action-call");
        session
            .record_session_started_if_needed()
            .expect("session should start");
        session
            .record_tool_call_pending(call.clone())
            .expect("pending call should record");
        let projection_before = session.ledger_projection();
        let next_sequence_before = session.next_sequence();
        let policy = ActionAuditPolicy::new(
            ActionRiskTier::EditElevated,
            ActionPolicyDisposition::Allow,
            "test policy allowed workspace write before commit lifecycle",
        );

        session
            .record_guarded_tool_action(&call, crate::ToolActionKind::WorkspaceWrite, policy)
            .expect("guarded audit should record for pending call");

        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), vec![call.clone()]);
        let audit_snapshot = session.action_audit_snapshot();
        assert_eq!(audit_snapshot.records().len(), 1);
        let audit = &audit_snapshot.records()[0];
        assert_eq!(audit.tool_call_id(), call.id());
        assert_eq!(audit.tool_name(), call.name());
        assert_eq!(audit.action_kind(), crate::ToolActionKind::WorkspaceWrite);
        assert_eq!(audit.status(), ActionAuditStatus::Guarded);
        assert_eq!(
            audit.policy().expect("guarded audit should include policy"),
            policy
        );
        assert!(audit.proposal().is_none());

        let projection_after = session.ledger_projection();
        assert_eq!(
            projection_after.entries().len(),
            projection_before.entries().len() + 1
        );
        assert!(matches!(
            projection_after.entries().last(),
            Some(LedgerProjection::Lifecycle {
                kind: LedgerFactKind::ActionAuditRecorded,
                ..
            })
        ));
        let expected_result_artifact_id = artifact_id("tool-result-2");
        assert!(matches!(
            session.read_artifact_content(&expected_result_artifact_id),
            Err(ArtifactError::MissingArtifact { id }) if id == expected_result_artifact_id
        ));

        let audit_snapshot_after_first = session.action_audit_snapshot();
        let projection_after_first = session.ledger_projection();
        session
            .record_guarded_tool_action(&call, crate::ToolActionKind::WorkspaceWrite, policy)
            .expect("duplicate guarded audit should no-op for pending call");

        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), vec![call.clone()]);
        assert_eq!(session.action_audit_snapshot(), audit_snapshot_after_first);
        assert_eq!(session.ledger_projection(), projection_after_first);
        assert!(matches!(
            session.read_artifact_content(&expected_result_artifact_id),
            Err(ArtifactError::MissingArtifact { id }) if id == expected_result_artifact_id
        ));
    }

    #[test]
    fn submit_tool_result_starts_session_before_artifact_and_resolution_when_needed() {
        let mut session = SessionState::new(session_id());
        session
            .pending_tool_calls
            .push(pending_tool_call("pre-start-call"));
        let artifact = ArtifactRef::new(artifact_id("pre-start-result"), ArtifactKind::Json);
        let result = ToolCallResult::succeeded(tool_call_id("pre-start-call"), artifact.clone());

        let events = session
            .submit_tool_result(result.clone(), ArtifactContent::json(r#"{"ok":true}"#))
            .expect("tool result should submit");

        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(matches!(events[0].kind, RuntimeEventKind::SessionStarted));
        assert!(matches!(
            &events[1].kind,
            RuntimeEventKind::ArtifactRecorded { artifact: recorded } if recorded == &artifact
        ));
        assert!(matches!(
            &events[2].kind,
            RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == &result
        ));
    }

    #[test]
    fn duplicate_tool_call_pending_is_rejected_without_second_pending_or_sequence() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("duplicate-call");
        let first = session
            .record_tool_call_pending(call.clone())
            .expect("first pending call should record");

        let err = session
            .record_tool_call_pending(call.clone())
            .expect_err("duplicate pending call id should be rejected");

        assert_eq!(err.code(), "tool_call_duplicate");
        assert_eq!(session.pending_tool_calls(), vec![call]);
        assert_eq!(first.sequence, 0);
        assert_eq!(
            session
                .ledger_projection()
                .entries()
                .iter()
                .map(|entry| match entry {
                    crate::ledger::LedgerProjection::Lifecycle { sequence, .. } => *sequence,
                    crate::ledger::LedgerProjection::Fact { sequence, .. } => *sequence,
                })
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn summary_draft_judgment_rejects_missing_evidence_without_registry_record() {
        let mut session = SessionState::new(session_id());
        let request = summary_draft_request(vec![judgment_evidence(
            "missing source",
            "missing-judgment-artifact",
            EvidenceLocator::whole_artifact(),
        )]);
        let outcome = summary_draft_outcome(vec![judgment_evidence(
            "missing source",
            "missing-judgment-artifact",
            EvidenceLocator::whole_artifact(),
        )]);

        let error = session
            .record_summary_draft_judgment(request, outcome)
            .expect_err("missing summary draft evidence should reject before registry write");

        assert!(matches!(
            error,
            JudgmentError::UnreadableEvidence {
                artifact_id,
                source: ArtifactError::MissingArtifact { .. },
            } if artifact_id.as_str() == "missing-judgment-artifact"
        ));
        assert!(session.judgment_records().is_empty());
    }

    #[test]
    fn summary_draft_judgment_rejects_bad_evidence_locator_without_registry_record() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("short-judgment-artifact"), ArtifactKind::Text),
                ArtifactContent::text("one line\n"),
            )
            .expect("artifact records");
        let request = summary_draft_request(vec![judgment_evidence(
            "bad line",
            "short-judgment-artifact",
            EvidenceLocator::line_range(4, 4).expect("valid locator shape"),
        )]);
        let outcome = summary_draft_outcome(vec![judgment_evidence(
            "whole source",
            "short-judgment-artifact",
            EvidenceLocator::whole_artifact(),
        )]);

        let error = session
            .record_summary_draft_judgment(request, outcome)
            .expect_err("bad summary draft evidence locator should reject before registry write");

        assert!(matches!(
            error,
            JudgmentError::UnreadableEvidence {
                artifact_id,
                source: ArtifactError::InvalidEvidenceLocator { .. },
            } if artifact_id.as_str() == "short-judgment-artifact"
        ));
        assert!(session.judgment_records().is_empty());
    }

    #[test]
    fn summary_draft_judgment_success_is_readable_from_internal_registry() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("judgment-source"), ArtifactKind::Text),
                ArtifactContent::text("first line\nsecond line\n"),
            )
            .expect("artifact records");
        let request = summary_draft_request(vec![judgment_evidence(
            "selected line",
            "judgment-source",
            EvidenceLocator::line_range(1, 1).expect("valid line locator"),
        )]);
        let outcome = summary_draft_outcome(vec![judgment_evidence(
            "whole source",
            "judgment-source",
            EvidenceLocator::whole_artifact(),
        )]);

        let record = session
            .record_summary_draft_judgment(request, outcome)
            .expect("readable summary draft evidence should record");
        let records = session.judgment_records();

        assert_eq!(records, vec![record.clone()]);
        assert_eq!(record.id().as_str(), "judgment-record-00000000000000000000");
        assert_eq!(record.request().purpose(), JudgmentPurpose::SummaryDraft);
        assert_eq!(record.outcome().purpose(), JudgmentPurpose::SummaryDraft);
        assert!(
            record
                .artifacts()
                .request()
                .content()
                .contains("artifact=request\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("artifact=outcome\n")
        );
        assert!(
            record
                .artifacts()
                .request()
                .content()
                .contains("evidence.0.locator=line:1-1\n")
        );
    }

    #[test]
    fn summary_draft_judgment_does_not_enter_context_ledger_events_or_tools() {
        let mut session = SessionState::new(session_id());
        let started = session
            .record_session_started_if_needed()
            .expect("session should start");
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("summary-audit-source"), ArtifactKind::Text),
                ArtifactContent::text("source text for advisory draft\n"),
            )
            .expect("artifact records");
        let request = summary_draft_request(vec![judgment_evidence(
            "summary source",
            "summary-audit-source",
            EvidenceLocator::whole_artifact(),
        )]);
        let draft = "Internal advisory summary draft that must not enter context.";
        let outcome = summary_draft_outcome_with_draft(
            vec![judgment_evidence(
                "summary source",
                "summary-audit-source",
                EvidenceLocator::whole_artifact(),
            )],
            draft,
        );
        let projection_before = session.ledger_projection();
        let next_sequence_before = session.next_sequence();
        let pending_tools_before = session.pending_tool_calls();
        let compiled_before = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("empty context compiles before judgment");

        let record = session
            .record_summary_draft_judgment(request, outcome)
            .expect("summary draft judgment records internally");
        let compiled_after = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("empty context compiles after judgment");
        let completed = session.record_step_completed();

        assert_eq!(session.judgment_records(), vec![record]);
        assert!(compiled_after.sections().is_empty());
        assert_eq!(compiled_after.to_snapshot(), "");
        assert_eq!(compiled_after, compiled_before);
        assert!(!compiled_after.to_snapshot().contains(draft));
        assert_eq!(session.pending_tool_calls(), pending_tools_before);
        assert_eq!(session.ledger_projection().entries().len(), 2);
        assert_eq!(
            session.ledger_projection().entries()[0],
            projection_before.entries()[0]
        );
        assert_eq!(next_sequence_before, 1);
        assert_eq!(started.sequence, 0);
        assert_eq!(completed.sequence, 1);
    }

    #[test]
    fn summary_draft_judgment_rejects_non_summary_draft_request_without_registry_record() {
        let mut session = SessionState::new(session_id());
        let outcome = JudgmentOutcome::new(
            JudgmentPurpose::MemoryRelevance,
            JudgmentRecommendation::NoRecommendation,
            judgment_confidence(0.1),
            Vec::new(),
            "No summary draft was produced.",
            "Only the helper boundary was exercised.",
            judgment_provenance(),
        )
        .expect("valid no recommendation outcome");

        let error = session
            .record_summary_draft_judgment(memory_relevance_request(Vec::new()), outcome)
            .expect_err("non-summary request is rejected by the narrow helper");

        assert_eq!(
            error,
            JudgmentError::SummaryDraftPurposeRequired {
                field: "judgment request",
                actual_purpose: JudgmentPurpose::MemoryRelevance,
            }
        );
        assert!(session.judgment_records().is_empty());
    }

    #[test]
    fn summary_draft_judgment_rejects_non_summary_draft_outcome_without_registry_record() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("summary-outcome-source"), ArtifactKind::Text),
                ArtifactContent::text("source text for advisory draft\n"),
            )
            .expect("artifact records");
        let request = summary_draft_request(vec![judgment_evidence(
            "summary source",
            "summary-outcome-source",
            EvidenceLocator::whole_artifact(),
        )]);

        let error = session
            .record_summary_draft_judgment(request, high_tool_risk_outcome())
            .expect_err("non-summary outcome is rejected by the narrow helper");

        assert_eq!(
            error,
            JudgmentError::SummaryDraftPurposeRequired {
                field: "judgment outcome",
                actual_purpose: JudgmentPurpose::ToolRiskReview,
            }
        );
        assert!(session.judgment_records().is_empty());
    }

    #[test]
    fn accepted_summary_draft_promotion_writes_one_compiled_context_summary_only() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("promotion-source"), ArtifactKind::Text),
                ArtifactContent::text("first source line\nsecond source line\n"),
            )
            .expect("artifact records");
        let started = session
            .record_session_started_if_needed()
            .expect("session starts");
        let pending = pending_tool_call("promotion-pending-call");
        let pending_event = session
            .record_tool_call_pending(pending.clone())
            .expect("pending tool call records");
        let evidence = judgment_evidence(
            "selected promotion line",
            "promotion-source",
            EvidenceLocator::line_range(1, 1).expect("valid line locator"),
        );
        let request = summary_draft_request(vec![evidence.clone()]);
        let outcome = summary_draft_outcome_with_draft(
            vec![judgment_evidence(
                "whole promotion source",
                "promotion-source",
                EvidenceLocator::whole_artifact(),
            )],
            "Accepted summary draft.",
        );
        let projection_before = session.ledger_projection();
        let next_sequence_before = session.next_sequence();
        let pending_tools_before = session.pending_tool_calls();

        session
            .promote_summary_draft_to_context(
                &request,
                &outcome,
                promotion_input(
                    "accepted-summary",
                    "Accepted summary draft.",
                    vec![evidence],
                ),
            )
            .expect("accepted summary draft promotes");

        let compiled = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("promoted context compiles");
        assert_eq!(compiled.sections().len(), 1);
        assert_eq!(
            compiled.to_snapshot(),
            [
                "summary:accepted-summary",
                "text:Accepted summary draft.",
                "evidence:selected promotion line:promotion-source:line:1-1",
            ]
            .join("\n")
        );
        assert_eq!(session.ledger_projection(), projection_before);
        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), pending_tools_before);
        assert_eq!(session.pending_tool_calls(), vec![pending]);
        assert_eq!(started.sequence, 0);
        assert_eq!(pending_event.sequence, 1);
        assert!(session.judgment_records().is_empty());
        assert_single_promotion_record(
            &session,
            "accepted-summary",
            SummaryDraftPromotionState::Promoted,
            None,
        );
    }

    #[test]
    fn checked_context_entry_rejects_invalid_candidate_without_context_mutation() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("checked-existing-source"), ArtifactKind::Text),
                ArtifactContent::text("existing source text\n"),
            )
            .expect("existing artifact records");
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("checked-short-source"), ArtifactKind::Text),
                ArtifactContent::text("one line\n"),
            )
            .expect("short artifact records");
        session.record_context_entry(ContextEntry::summary(
            ContextSummary::new(
                "checked-existing-summary",
                "Existing checked summary.",
                vec![
                    ContextEvidence::new(
                        "existing source",
                        EvidenceRef::new(
                            artifact_id("checked-existing-source"),
                            EvidenceLocator::whole_artifact(),
                        ),
                    )
                    .expect("valid context evidence"),
                ],
            )
            .expect("valid context summary"),
        ));
        let context_before = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("existing context compiles")
            .to_snapshot();

        let error = session
            .record_checked_context_entry(ContextEntry::summary(
                ContextSummary::new(
                    "checked-invalid-summary",
                    "Invalid checked summary.",
                    vec![
                        ContextEvidence::new(
                            "invalid source",
                            EvidenceRef::new(
                                artifact_id("checked-short-source"),
                                EvidenceLocator::line_range(2, 2).expect("valid locator shape"),
                            ),
                        )
                        .expect("valid context evidence"),
                    ],
                )
                .expect("valid context summary"),
            ))
            .expect_err("checked append rejects unreadable evidence");

        assert!(matches!(
            error,
            ContextError::UnreadableEvidence {
                summary_id,
                artifact_id,
                source: ArtifactError::InvalidEvidenceLocator { .. },
            } if summary_id == "checked-invalid-summary"
                && artifact_id.as_str() == "checked-short-source"
        ));
        assert_eq!(
            ContextCompiler::new()
                .compile(&session.context_snapshot())
                .expect("existing context still compiles")
                .to_snapshot(),
            context_before
        );
    }

    #[test]
    fn summary_draft_promotion_exact_replay_after_promoted_is_idempotent_without_context_change() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(
                    artifact_id("duplicate-promotion-source"),
                    ArtifactKind::Text,
                ),
                ArtifactContent::text("duplicate source text\n"),
            )
            .expect("artifact records");
        let source_record_id =
            JudgmentRecordId::new("source-summary-draft-record").expect("valid judgment record id");
        let evidence = judgment_evidence(
            "duplicate source",
            "duplicate-promotion-source",
            EvidenceLocator::whole_artifact(),
        );
        let request = summary_draft_request(vec![evidence.clone()]);
        let outcome =
            summary_draft_outcome_with_draft(vec![evidence.clone()], "Duplicate summary draft.");
        session
            .promote_summary_draft_to_context(
                &request,
                &outcome,
                promotion_input(
                    "duplicate-summary",
                    "Duplicate summary draft.",
                    vec![evidence.clone()],
                ),
            )
            .expect("first promotion succeeds");
        assert_single_promotion_record(
            &session,
            "duplicate-summary",
            SummaryDraftPromotionState::Promoted,
            None,
        );
        let context_before = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context compiles after first promotion");
        let projection_before = session.ledger_projection();
        let next_sequence_before = session.next_sequence();
        let pending_before = session.pending_tool_calls();

        session
            .promote_summary_draft_to_context(
                &request,
                &outcome,
                promotion_input(
                    "duplicate-summary",
                    "Duplicate summary draft.",
                    vec![evidence],
                ),
            )
            .expect("exact promoted replay is idempotent");

        let context_after = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context still compiles after exact replay");
        assert_eq!(context_after, context_before);
        assert_eq!(context_after.sections().len(), 1);
        assert_eq!(
            context_after.to_snapshot(),
            [
                "summary:duplicate-summary",
                "text:Duplicate summary draft.",
                "evidence:duplicate source:duplicate-promotion-source:whole",
            ]
            .join("\n")
        );
        assert_eq!(session.ledger_projection(), projection_before);
        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), pending_before);
        assert_single_promotion_record(
            &session,
            "duplicate-summary",
            SummaryDraftPromotionState::Promoted,
            None,
        );

        let conflict = session
            .promote_summary_draft_to_context(
                &request,
                &outcome,
                promotion_input_with_source_record_id(
                    "duplicate-summary",
                    "Duplicate summary draft.",
                    vec![judgment_evidence(
                        "duplicate source",
                        "duplicate-promotion-source",
                        EvidenceLocator::whole_artifact(),
                    )],
                    Some(source_record_id),
                ),
            )
            .expect_err("same summary id with different source record conflicts");

        assert_eq!(
            conflict,
            SummaryDraftPromotionError::PromotionPayloadConflict {
                summary_id: "duplicate-summary".to_owned(),
            }
        );
        assert_eq!(
            ContextCompiler::new()
                .compile(&session.context_snapshot())
                .expect("context still compiles")
                .to_snapshot(),
            context_before.to_snapshot()
        );
        assert_eq!(session.ledger_projection(), projection_before);
        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), pending_before);
        assert_single_promotion_record(
            &session,
            "duplicate-summary",
            SummaryDraftPromotionState::Promoted,
            None,
        );
    }

    #[test]
    fn summary_draft_promotion_pre_existing_context_duplicate_does_not_write_registry() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(
                    artifact_id("pre-existing-summary-source"),
                    ArtifactKind::Text,
                ),
                ArtifactContent::text("pre-existing source text\n"),
            )
            .expect("artifact records");
        let evidence = judgment_evidence(
            "pre-existing source",
            "pre-existing-summary-source",
            EvidenceLocator::whole_artifact(),
        );
        session.record_context_entry(crate::context::ContextEntry::summary(
            crate::context::ContextSummary::new(
                "pre-existing-summary",
                "Already recorded summary.",
                vec![
                    crate::context::ContextEvidence::new(
                        "pre-existing source",
                        EvidenceRef::new(
                            artifact_id("pre-existing-summary-source"),
                            EvidenceLocator::whole_artifact(),
                        ),
                    )
                    .expect("valid context evidence"),
                ],
            )
            .expect("valid context summary"),
        ));
        let context_before = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("pre-existing context compiles");
        let projection_before = session.ledger_projection();
        let next_sequence_before = session.next_sequence();
        let pending_before = session.pending_tool_calls();
        let request = summary_draft_request(vec![evidence.clone()]);
        let outcome =
            summary_draft_outcome_with_draft(vec![evidence.clone()], "New duplicate draft.");

        let error = session
            .promote_summary_draft_to_context(
                &request,
                &outcome,
                promotion_input(
                    "pre-existing-summary",
                    "New duplicate draft.",
                    vec![evidence],
                ),
            )
            .expect_err("external context summary id duplicate is rejected");

        assert_eq!(
            error,
            SummaryDraftPromotionError::DuplicateSummaryId {
                summary_id: "pre-existing-summary".to_owned(),
            }
        );
        assert_eq!(
            ContextCompiler::new()
                .compile(&session.context_snapshot())
                .expect("context still compiles after duplicate rejection"),
            context_before
        );
        assert!(
            session
                .summary_draft_promotion_snapshot()
                .records()
                .is_empty()
        );
        assert_eq!(session.ledger_projection(), projection_before);
        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), pending_before);
    }

    #[test]
    fn summary_draft_promotion_same_summary_id_different_draft_conflicts_without_context_change() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("draft-conflict-source"), ArtifactKind::Text),
                ArtifactContent::text("draft conflict source text\n"),
            )
            .expect("artifact records");
        let first_evidence = judgment_evidence(
            "draft conflict source",
            "draft-conflict-source",
            EvidenceLocator::whole_artifact(),
        );
        let first_request = summary_draft_request(vec![first_evidence.clone()]);
        let first_outcome =
            summary_draft_outcome_with_draft(vec![first_evidence.clone()], "Original draft.");
        session
            .promote_summary_draft_to_context(
                &first_request,
                &first_outcome,
                promotion_input(
                    "conflicting-summary",
                    "Original draft.",
                    vec![first_evidence],
                ),
            )
            .expect("first promotion succeeds");
        let context_before = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context compiles after first promotion");
        let projection_before = session.ledger_projection();
        let next_sequence_before = session.next_sequence();
        let pending_before = session.pending_tool_calls();
        let conflict_evidence = judgment_evidence(
            "draft conflict source",
            "draft-conflict-source",
            EvidenceLocator::whole_artifact(),
        );
        let conflict_request = summary_draft_request(vec![conflict_evidence.clone()]);
        let conflict_outcome =
            summary_draft_outcome_with_draft(vec![conflict_evidence.clone()], "Changed draft.");

        let error = session
            .promote_summary_draft_to_context(
                &conflict_request,
                &conflict_outcome,
                promotion_input(
                    "conflicting-summary",
                    "Changed draft.",
                    vec![conflict_evidence],
                ),
            )
            .expect_err("same summary id different draft conflicts");

        assert_eq!(
            error,
            SummaryDraftPromotionError::PromotionPayloadConflict {
                summary_id: "conflicting-summary".to_owned(),
            }
        );
        assert_eq!(
            ContextCompiler::new()
                .compile(&session.context_snapshot())
                .expect("context still compiles after draft conflict"),
            context_before
        );
        assert_eq!(session.ledger_projection(), projection_before);
        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), pending_before);
        assert_single_promotion_record(
            &session,
            "conflicting-summary",
            SummaryDraftPromotionState::Promoted,
            None,
        );
    }

    #[test]
    fn summary_draft_promotion_same_summary_id_different_evidence_conflicts_without_context_change()
    {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("evidence-conflict-source"), ArtifactKind::Text),
                ArtifactContent::text("first line\nsecond line\n"),
            )
            .expect("artifact records");
        let first_evidence = judgment_evidence(
            "first evidence line",
            "evidence-conflict-source",
            EvidenceLocator::line_range(1, 1).expect("valid line locator"),
        );
        let first_request = summary_draft_request(vec![first_evidence.clone()]);
        let first_outcome = summary_draft_outcome_with_draft(
            vec![first_evidence.clone()],
            "Evidence conflict draft.",
        );
        session
            .promote_summary_draft_to_context(
                &first_request,
                &first_outcome,
                promotion_input(
                    "evidence-conflicting-summary",
                    "Evidence conflict draft.",
                    vec![first_evidence],
                ),
            )
            .expect("first promotion succeeds");
        let context_before = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context compiles after first promotion");
        let projection_before = session.ledger_projection();
        let next_sequence_before = session.next_sequence();
        let pending_before = session.pending_tool_calls();
        let conflict_evidence = judgment_evidence(
            "second evidence line",
            "evidence-conflict-source",
            EvidenceLocator::line_range(2, 2).expect("valid line locator"),
        );
        let conflict_request = summary_draft_request(vec![conflict_evidence.clone()]);
        let conflict_outcome = summary_draft_outcome_with_draft(
            vec![conflict_evidence.clone()],
            "Evidence conflict draft.",
        );

        let error = session
            .promote_summary_draft_to_context(
                &conflict_request,
                &conflict_outcome,
                promotion_input(
                    "evidence-conflicting-summary",
                    "Evidence conflict draft.",
                    vec![conflict_evidence],
                ),
            )
            .expect_err("same summary id different evidence conflicts");

        assert_eq!(
            error,
            SummaryDraftPromotionError::PromotionPayloadConflict {
                summary_id: "evidence-conflicting-summary".to_owned(),
            }
        );
        assert_eq!(
            ContextCompiler::new()
                .compile(&session.context_snapshot())
                .expect("context still compiles after evidence conflict"),
            context_before
        );
        assert_eq!(session.ledger_projection(), projection_before);
        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), pending_before);
        assert_single_promotion_record(
            &session,
            "evidence-conflicting-summary",
            SummaryDraftPromotionState::Promoted,
            None,
        );
    }

    #[test]
    fn summary_draft_promotion_compile_failure_rejects_record_and_exact_replay() {
        let mut session = SessionState::new(session_id());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("short-promotion-source"), ArtifactKind::Text),
                ArtifactContent::text("one line\n"),
            )
            .expect("artifact records");
        let bad_evidence = judgment_evidence(
            "bad promotion line",
            "short-promotion-source",
            EvidenceLocator::line_range(3, 3).expect("valid locator shape"),
        );
        let request = summary_draft_request(vec![bad_evidence.clone()]);
        let outcome =
            summary_draft_outcome_with_draft(vec![bad_evidence.clone()], "Bad summary draft.");
        let input = promotion_input("bad-summary", "Bad summary draft.", vec![bad_evidence]);
        let context_before = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("empty context compiles before failed promotion")
            .to_snapshot();
        let projection_before = session.ledger_projection();
        let next_sequence_before = session.next_sequence();
        let pending_before = session.pending_tool_calls();

        let error = session
            .promote_summary_draft_to_context(&request, &outcome, input.clone())
            .expect_err("compile validation rejects unreadable evidence");

        assert!(matches!(
            error,
            SummaryDraftPromotionError::Context {
                source: ContextError::UnreadableEvidence {
                    summary_id,
                    artifact_id,
                    source: ArtifactError::InvalidEvidenceLocator { .. },
                },
            } if summary_id == "bad-summary" && artifact_id.as_str() == "short-promotion-source"
        ));
        assert_eq!(
            ContextCompiler::new()
                .compile(&session.context_snapshot())
                .expect("context still compiles after failed promotion")
                .to_snapshot(),
            context_before
        );
        assert_eq!(session.ledger_projection(), projection_before);
        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), pending_before);
        assert_single_promotion_record(
            &session,
            "bad-summary",
            SummaryDraftPromotionState::Rejected,
            None,
        );

        let replay_error = session
            .promote_summary_draft_to_context(&request, &outcome, input)
            .expect_err("exact rejected replay stays rejected");

        assert_eq!(
            replay_error,
            SummaryDraftPromotionError::PromotionAlreadyRejected {
                summary_id: "bad-summary".to_owned(),
            }
        );
        assert_eq!(
            ContextCompiler::new()
                .compile(&session.context_snapshot())
                .expect("context still compiles after rejected replay")
                .to_snapshot(),
            context_before
        );
        assert_eq!(session.ledger_projection(), projection_before);
        assert_eq!(session.next_sequence(), next_sequence_before);
        assert_eq!(session.pending_tool_calls(), pending_before);
        assert_single_promotion_record(
            &session,
            "bad-summary",
            SummaryDraftPromotionState::Rejected,
            None,
        );
    }

    #[test]
    fn high_tool_risk_review_does_not_mutate_pending_tool_or_context_state() {
        let mut session = SessionState::new(session_id());
        let call = pending_tool_call("risky-call");
        session
            .record_tool_call_pending(call.clone())
            .expect("pending tool call records");
        let pending_before = session.pending_tool_calls();
        let projection_before = session.ledger_projection();
        let context_before = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("empty context compiles")
            .to_snapshot();

        session
            .record_judgment(high_tool_risk_request(), high_tool_risk_outcome())
            .expect("high tool risk review records internally");

        assert_eq!(session.pending_tool_calls(), pending_before);
        assert_eq!(session.ledger_projection(), projection_before);
        let context_after = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("empty context compiles after judgment")
            .to_snapshot();
        assert_eq!(context_after, context_before);
        assert_eq!(session.judgment_records().len(), 1);
    }

    #[test]
    fn context_snapshot_is_independent_from_later_recorded_memory() {
        let mut session = SessionState::new(session_id());

        let stale = session.context_snapshot();
        let memory = activated_memory("memory-later");
        session.record_activated_memory(memory.clone());
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("memory-later-artifact"), ArtifactKind::Text),
                ArtifactContent::text("first exact memory evidence\n"),
            )
            .expect("memory artifact records");
        let current = session.context_snapshot();
        let second_memory =
            activated_memory_with_details("memory-later-2", "later text", 1, 2, 0.5);
        session.record_activated_memory(second_memory);
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id("memory-later-2-artifact"), ArtifactKind::Text),
                ArtifactContent::text("second exact memory evidence\n"),
            )
            .expect("later memory artifact records");

        let stale = ContextCompiler::new()
            .compile(&stale)
            .expect("stale snapshot compiles");
        let current = ContextCompiler::new()
            .compile(&current)
            .expect("current snapshot compiles");

        assert_eq!(stale.to_snapshot(), "");
        assert!(current.to_snapshot().contains("memory:memory-later"));
        assert!(
            current
                .to_snapshot()
                .contains("memory-evidence:primary source:memory-later-artifact:whole")
        );
        assert!(
            current
                .to_snapshot()
                .contains("memory-activation-source-label:user request")
        );
        assert!(!current.to_snapshot().contains("memory-later-2"));
    }

    #[test]
    fn record_activated_memories_appends_to_memory_projection() {
        let mut session = SessionState::new(session_id());
        let memory_a = activated_memory("memory-a");
        let memory_b = activated_memory("memory-b");
        record_memory_artifacts(&mut session, &[&memory_a, &memory_b]);
        session.record_activated_memories(vec![memory_a, memory_b]);

        let compiled = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("snapshot compiles");

        assert!(compiled.to_snapshot().contains("memory:memory-a"));
        assert!(compiled.to_snapshot().contains("memory:memory-b"));
    }

    #[test]
    fn replace_activated_memories_updates_current_memory_projection() {
        let mut session = SessionState::new(session_id());
        let stale = activated_memory("memory-stale");
        let current = activated_memory("memory-current");
        record_memory_artifacts(&mut session, &[&stale, &current]);

        session.record_activated_memory(stale);
        session.replace_activated_memories(vec![current]);

        let compiled = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("snapshot compiles");

        assert!(!compiled.to_snapshot().contains("memory:memory-stale"));
        assert!(compiled.to_snapshot().contains("memory:memory-current"));
    }

    #[test]
    fn replace_activated_memories_with_empty_clears_current_memory_projection() {
        let mut session = SessionState::new(session_id());
        let memory = activated_memory("memory-cleared");
        record_memory_artifacts(&mut session, &[&memory]);

        session.record_activated_memory(memory);
        session.replace_activated_memories(Vec::new());

        let compiled = ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("snapshot compiles");

        assert_eq!(compiled.to_snapshot(), "");
    }

    #[test]
    fn duplicate_recorded_activated_memories_compile_once_deterministically() {
        let lower_duplicate =
            activated_memory_with_details("memory-duplicate", "Lower duplicate.", 1, 0, 0.5);
        let higher_duplicate =
            activated_memory_with_details("memory-duplicate", "Higher duplicate.", 2, 0, 0.5);

        let mut first = SessionState::new(session_id());
        record_memory_artifacts(&mut first, &[&lower_duplicate, &higher_duplicate]);
        first.record_activated_memories(vec![lower_duplicate.clone(), higher_duplicate.clone()]);
        let first = ContextCompiler::new()
            .compile(&first.context_snapshot())
            .expect("first snapshot compiles")
            .to_snapshot();

        let mut second = SessionState::new(session_id());
        record_memory_artifacts(&mut second, &[&higher_duplicate, &lower_duplicate]);
        second.record_activated_memories(vec![higher_duplicate, lower_duplicate]);
        let second = ContextCompiler::new()
            .compile(&second.context_snapshot())
            .expect("second snapshot compiles")
            .to_snapshot();

        assert_eq!(first, second);
        assert_eq!(first.matches("memory:memory-duplicate").count(), 1);
        assert!(first.contains("memory-text:Higher duplicate."));
        assert!(!first.contains("memory-text:Lower duplicate."));
    }
}
