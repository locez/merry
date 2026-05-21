//! Runtime session state and state-before-event helpers.

#[cfg(test)]
use crate::summary_draft_promotion::SummaryDraftPromotionRegistrySnapshot;
use crate::{
    RuntimeError,
    action_audit::ActionAuditRegistry,
    action_policy::ActionPolicyDecision,
    artifact::{ArtifactContent, ArtifactError, ArtifactRegistry},
    context::{ContextCompiler, ContextEntry, ContextError, SessionContextSnapshot},
    judgment::{
        JudgmentError, JudgmentEvidence, JudgmentOutcome, JudgmentRecord, JudgmentRegistry,
        JudgmentRequest, SummaryDraftPromotionError, SummaryDraftPromotionInput,
        context_summary_from_accepted_summary_draft, validate_summary_draft_record_purpose,
    },
    ledger::{LedgerFactKind, TaskLedger},
    memory::{ActivatedMemory, MemoryError, MemoryItem, MemoryStore},
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

const ASSISTANT_OUTPUT_ARTIFACT_PREFIX: &str = "assistant-output-";
const TOOL_RESULT_ARTIFACT_PREFIX: &str = "tool-result-";

/// Resolved tool call state that has not yet been compiled into a provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolContinuation {
    call: PendingToolCall,
    result: ToolCallResult,
}

impl ResolvedToolContinuation {
    fn new(call: PendingToolCall, result: ToolCallResult) -> Self {
        Self { call, result }
    }
}

/// Tool continuation data read from session state for one request compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedToolContinuationSnapshot {
    call: PendingToolCall,
    result: ToolCallResult,
    content: ArtifactContent,
}

impl ResolvedToolContinuationSnapshot {
    fn new(call: PendingToolCall, result: ToolCallResult, content: ArtifactContent) -> Self {
        Self {
            call,
            result,
            content,
        }
    }

    pub(crate) fn call(&self) -> &PendingToolCall {
        &self.call
    }

    pub(crate) fn result(&self) -> &ToolCallResult {
        &self.result
    }

    pub(crate) fn content(&self) -> &ArtifactContent {
        &self.content
    }
}

/// Mutable runtime state for one session.
#[derive(Debug)]
pub(crate) struct SessionState {
    session_id: SessionId,
    next_sequence: u64,
    session_started: bool,
    ledger: TaskLedger,
    artifacts: ArtifactRegistry,
    memory_store: MemoryStore,
    context_entries: Vec<ContextEntry>,
    activated_memories: Vec<ActivatedMemory>,
    #[allow(dead_code)]
    judgments: JudgmentRegistry,
    summary_draft_promotions: SummaryDraftPromotionRegistry,
    action_audits: ActionAuditRegistry,
    pending_tool_calls: Vec<PendingToolCall>,
    resolved_tool_calls: BTreeSet<ToolCallId>,
    unconsumed_tool_continuations: Vec<ResolvedToolContinuation>,
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
            context_entries: Vec::new(),
            activated_memories: Vec::new(),
            judgments: JudgmentRegistry::default(),
            summary_draft_promotions: SummaryDraftPromotionRegistry::default(),
            action_audits: ActionAuditRegistry::default(),
            pending_tool_calls: Vec::new(),
            resolved_tool_calls: BTreeSet::new(),
            unconsumed_tool_continuations: Vec::new(),
        }
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
        let recorded = self.record_artifact_state(artifact, content)?;
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

    pub(crate) fn record_assistant_text_output(
        &mut self,
        text: String,
    ) -> Result<RuntimeEvent, ArtifactError> {
        let artifact_sequence = self.next_sequence();
        let artifact = ArtifactRef::new(assistant_output_id(artifact_sequence), ArtifactKind::Text);
        let recorded = self.record_artifact_state(artifact, ArtifactContent::text(text))?;
        Ok(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ))
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

    pub(crate) fn unconsumed_tool_continuation_snapshots(
        &self,
    ) -> Result<Vec<ResolvedToolContinuationSnapshot>, ArtifactError> {
        self.unconsumed_tool_continuations
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

    pub(crate) fn consume_tool_continuations(&mut self, count: usize) {
        let count = count.min(self.unconsumed_tool_continuations.len());
        self.unconsumed_tool_continuations.drain(..count);
    }

    #[cfg(test)]
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
        let recorded = self.record_artifact_state(result.artifact().clone(), content)?;
        debug_assert_eq!(&recorded, result.artifact());

        let pending = self.pending_tool_calls.remove(pending_index);
        self.resolved_tool_calls.insert(result.call_id().clone());
        self.unconsumed_tool_continuations
            .push(ResolvedToolContinuation::new(pending, result.clone()));

        let mut events = Vec::with_capacity(if self.session_started { 2 } else { 3 });
        if let Some(started) = self.record_session_started_if_needed() {
            events.push(started);
        }

        events.push(self.record_event(
            RuntimeEventKind::ArtifactRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ));
        events.push(self.record_event(
            RuntimeEventKind::ToolCallResolved { result },
            LedgerFactKind::ToolCallResolved,
        ));

        Ok(events)
    }

    pub(crate) fn submit_tool_execution_outcome(
        &mut self,
        call_id: &ToolCallId,
        status: ToolCallResultStatus,
        content: ArtifactContent,
        diagnostic: Option<ErrorInfo>,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
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

    pub(crate) fn submit_denied_tool_action(
        &mut self,
        pending: &PendingToolCall,
        decision: &ActionPolicyDecision,
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
            self.record_denied_tool_action_audit(&pending, decision);
            let artifact = self
                .artifacts
                .record_preflighted(result.artifact().clone(), content);
            debug_assert_eq!(artifact, *result.artifact());
            self.resolved_tool_calls.insert(result.call_id().clone());
            self.unconsumed_tool_continuations
                .push(ResolvedToolContinuation::new(pending, result.clone()));
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

        self.record_denied_tool_action_audit(&pending, decision);
        let artifact = self
            .artifacts
            .record_preflighted(result.artifact().clone(), content);
        debug_assert_eq!(artifact, *result.artifact());
        self.resolved_tool_calls.insert(result.call_id().clone());
        self.unconsumed_tool_continuations
            .push(ResolvedToolContinuation::new(pending, result.clone()));
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

    fn next_sequence(&self) -> u64 {
        self.next_sequence
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

pub(crate) fn is_runtime_reserved_artifact_id(artifact_id: &ArtifactId) -> bool {
    artifact_id
        .as_str()
        .starts_with(ASSISTANT_OUTPUT_ARTIFACT_PREFIX)
        || artifact_id
            .as_str()
            .starts_with(TOOL_RESULT_ARTIFACT_PREFIX)
}

fn assistant_output_id(sequence: u64) -> ArtifactId {
    ArtifactId::new(&format!("{ASSISTANT_OUTPUT_ARTIFACT_PREFIX}{sequence}"))
        .expect("assistant output artifact id uses a valid static prefix and sequence")
}

fn tool_result_id(sequence: u64) -> ArtifactId {
    ArtifactId::new(&format!("{TOOL_RESULT_ARTIFACT_PREFIX}{sequence}"))
        .expect("tool result artifact id uses a valid static prefix and sequence")
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
    use super::SessionState;
    use crate::{
        action_audit::ActionAuditStatus,
        action_policy::{ActionPolicyDisposition, DefaultActionPolicy},
        artifact::{ArtifactContent, ArtifactError},
        context::{ContextCompiler, ContextEntry, ContextError, ContextEvidence, ContextSummary},
        judgment::{
            JudgmentConfidence, JudgmentError, JudgmentEvidence, JudgmentOutcome,
            JudgmentProvenance, JudgmentPurpose, JudgmentRecommendation, JudgmentRecordId,
            JudgmentRiskLevel, JudgmentSourceKind, SummaryDraftAcceptance,
            SummaryDraftAcceptanceAuthority, SummaryDraftPromotionError,
            SummaryDraftPromotionInput,
        },
        ledger::{LedgerFactKind, LedgerProjection},
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
        assert_eq!(audit.policy().disposition(), ActionPolicyDisposition::Deny);
        assert_eq!(audit.policy().risk_tier(), decision.risk_tier());
        assert_eq!(audit.policy().reason(), decision.reason());

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
