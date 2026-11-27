use super::{ModelTurnStatus, SessionState, TranscriptItem};
use crate::{
    CitationCompactionInput, CitationCompactionPolicy, RuntimeError, TaskAnchor,
    context::ContextCompiler,
    memory::{
        ActivatedMemory, MemoryActivationProvenance, MemoryActivationReason, MemoryActivationScore,
        MemoryActivationSourceKind, MemoryEvidence, MemoryId, MemoryItem, MemoryItemSelection,
        MemoryScope,
    },
};
use merry_core::{
    ErrorInfo, PendingToolCall, PendingToolCallBatch, RuntimeJournalPayload, SessionId,
    ToolCallArguments, ToolCallBatchId, ToolCallId, ToolCallResult, ToolName, ToolOutput,
};
use serde_json::json;

mod checkpoint_refs;
mod compaction;
mod context_memory;
mod lifecycle;
mod model_tool_content;
mod persistence;
mod rolling_compaction;
mod tool_calls;
mod transcript;
mod usage;

fn session_id() -> SessionId {
    SessionId::new("session-state-test").expect("valid session id")
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
        let event = self.record_assistant_text_output(turn_id, text, None)?;
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

fn memory_id(value: &str) -> MemoryId {
    MemoryId::new(value).expect("valid memory id")
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
        vec![memory_evidence("primary source", &format!("{id}-source"))],
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

fn memory_evidence(label: &str, source: &str) -> MemoryEvidence {
    MemoryEvidence::new(label, source).expect("valid memory evidence")
}
