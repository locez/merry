use super::{
    ModelTurnStatus, ProposedToolExecutionOutcome, SessionState, ToolResultLedgerObservation,
    TranscriptItem,
};
use crate::{
    ActionExecutionEvidence, ActionProposal, ActionProposalEvidence, CitationCompactionInput,
    CitationCompactionPolicy, RuntimeError, TaskAnchor, WorkspacePatchExecutionEvidence,
    WorkspacePatchProposal,
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
        JudgmentConfidence, JudgmentError, JudgmentEvidence, JudgmentOutcome, JudgmentProvenance,
        JudgmentPurpose, JudgmentRecommendation, JudgmentRecordId, JudgmentRiskLevel,
        JudgmentSourceKind, SummaryDraftAcceptance, SummaryDraftAcceptanceAuthority,
        SummaryDraftPromotionError, SummaryDraftPromotionInput,
    },
    ledger::{LedgerFactKind, LedgerProjection, LedgerScope},
    memory::{
        ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason, MemoryActivationScore,
        MemoryActivationSourceKind, MemoryEvidence, MemoryId, MemoryItem, MemoryItemSelection,
        MemoryScope,
    },
    summary_draft_promotion::SummaryDraftPromotionState,
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, EvidenceLocator, EvidenceRef,
    PendingToolCall, PendingToolCallBatch, RuntimeJournalPayload, SessionId, ToolCallArguments,
    ToolCallBatchId, ToolCallId, ToolCallResult, ToolName,
};
use serde_json::json;

mod checkpoint_ref_persistence;
mod checkpoint_refs;
mod compaction;
mod context_memory;
mod judgments;
mod lifecycle;
mod persistence;
mod tool_calls;
mod transcript;
mod usage;

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

trait SessionStateTestExt {
    fn build_test_citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError>;
    fn record_test_user_message_body(&mut self, text: &str) -> Result<(), RuntimeError>;
    fn record_test_assistant_text_output(
        &mut self,
        text: String,
    ) -> Result<merry_core::RuntimeJournalEvent, RuntimeError>;
    fn record_test_tool_call_pending(
        &mut self,
        call: PendingToolCall,
    ) -> Result<merry_core::RuntimeJournalEvent, ErrorInfo>;
    fn record_test_tool_call_batch_pending(
        &mut self,
        batch: PendingToolCallBatch,
    ) -> Result<merry_core::RuntimeJournalEvent, ErrorInfo>;
}

impl SessionStateTestExt for SessionState {
    fn build_test_citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        self.build_citation_compaction_input(
            policy,
            policy.resolve(64_000).map_err(RuntimeError::from)?,
        )
    }

    fn record_test_user_message_body(&mut self, text: &str) -> Result<(), RuntimeError> {
        let turn_id = self.begin_model_turn()?;
        self.record_user_message_body(turn_id, text)?;
        self.close_model_response(turn_id, false)
    }

    fn record_test_assistant_text_output(
        &mut self,
        text: String,
    ) -> Result<merry_core::RuntimeJournalEvent, RuntimeError> {
        let turn_id = self.begin_model_turn()?;
        let event = self.record_assistant_text_output(turn_id, text)?;
        self.close_model_response(turn_id, false)?;
        Ok(event)
    }

    fn record_test_tool_call_pending(
        &mut self,
        call: PendingToolCall,
    ) -> Result<merry_core::RuntimeJournalEvent, ErrorInfo> {
        let turn_id = self.begin_model_turn().map_err(test_turn_diagnostic)?;
        let event = self.record_tool_call_pending(turn_id, call)?;
        self.close_model_response(turn_id, true)
            .map_err(test_turn_diagnostic)?;
        Ok(event)
    }

    fn record_test_tool_call_batch_pending(
        &mut self,
        batch: PendingToolCallBatch,
    ) -> Result<merry_core::RuntimeJournalEvent, ErrorInfo> {
        let turn_id = self.begin_model_turn().map_err(test_turn_diagnostic)?;
        let event = self.record_tool_call_batch_pending(turn_id, batch)?;
        self.close_model_response(turn_id, true)
            .map_err(test_turn_diagnostic)?;
        Ok(event)
    }
}

fn test_turn_diagnostic(error: RuntimeError) -> ErrorInfo {
    ErrorInfo::new("test_model_turn", &error.to_string())
        .expect("test model turn diagnostic should be valid")
}

fn citation_plain_runtime_checkpoint_for_tests(
    checkpoint_id: &str,
    text: &str,
) -> crate::CompactedCheckpoint {
    let manifest = CheckpointRefManifest::new(
        CheckpointId::new(checkpoint_id).expect("valid checkpoint id"),
        vec![CheckpointRef::new(
            CheckpointRefId::new("r1").expect("valid ref id"),
            CheckpointSourceKind::UserMessage,
            CheckpointSequenceRange::new(1, 1).expect("valid range"),
            EvidenceRef::new(
                artifact_id("checkpoint-test-source"),
                EvidenceLocator::whole_artifact(),
            ),
        )],
    )
    .expect("valid manifest");
    let candidate = CompactedCheckpointCandidate::from_json(&format!(
        r#"{{
              "confirmed_decisions": [],
              "rejected_approaches": [],
              "constraints_preferences_boundaries": [
                {{
                  "id": "c1",
                  "text": {text_json},
                  "refs": ["r1"]
                }}
              ],
              "corrected_misunderstandings": [],
              "durable_conclusions": [],
              "open_questions": [],
              "current_progress_and_next_steps": [],
              "exact_details": [],
              "handoffs": []
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

fn memory_relevance_request(evidence: Vec<JudgmentEvidence>) -> crate::judgment::JudgmentRequest {
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
                    ArtifactRef::new(evidence.reference().artifact_id.clone(), ArtifactKind::Text),
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
