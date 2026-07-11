use super::{ModelTurnId, SessionState, artifacts::assistant_output_id};
use crate::{RuntimeError, artifact::ArtifactContent, ledger::LedgerFactKind};
use merry_core::{
    ArtifactKind, ArtifactRef, ErrorInfo, PendingToolCall, PendingToolCallBatch,
    RuntimeJournalEvent, RuntimeJournalPayload, ToolCallBatchId, ToolCallId,
};

mod action;
mod result;
mod skill;

pub(crate) struct PreparedModelToolCallResponse {
    commentary: Option<PreparedModelCommentary>,
    transcript: super::Transcript,
    calls: Vec<PendingToolCall>,
    tool_payload: RuntimeJournalPayload,
    tool_sequence: u64,
}

struct PreparedModelCommentary {
    artifact: ArtifactRef,
    content: ArtifactContent,
    transcript: super::Transcript,
    sequence: u64,
}

impl PreparedModelToolCallResponse {
    pub(crate) const fn has_commentary(&self) -> bool {
        self.commentary.is_some()
    }
}

impl SessionState {
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

    #[cfg(test)]
    pub(crate) fn record_tool_call_pending(
        &mut self,
        turn_id: ModelTurnId,
        call: PendingToolCall,
    ) -> Result<RuntimeJournalEvent, ErrorInfo> {
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

        let mut transcript = self.transcript.clone();
        transcript
            .push_tool_call(turn_id, call.clone())
            .map_err(transcript_record_diagnostic)?;
        self.transcript = transcript;
        self.pending_tool_calls.push(call.clone());
        Ok(self.record_event(
            RuntimeJournalPayload::ToolCallPending { call },
            LedgerFactKind::ToolCallPending,
        ))
    }

    #[cfg(test)]
    pub(crate) fn record_tool_call_batch_pending(
        &mut self,
        turn_id: ModelTurnId,
        batch: PendingToolCallBatch,
    ) -> Result<RuntimeJournalEvent, ErrorInfo> {
        for call in batch.calls() {
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
        }

        let mut transcript = self.transcript.clone();
        for call in batch.calls() {
            transcript
                .push_tool_call(turn_id, call.clone())
                .map_err(transcript_record_diagnostic)?;
        }

        self.transcript = transcript;
        self.pending_tool_calls
            .extend(batch.calls().iter().cloned());
        Ok(self.record_event(
            RuntimeJournalPayload::ToolCallBatchPending { batch },
            LedgerFactKind::ToolCallPending,
        ))
    }

    pub(crate) fn prepare_model_tool_call_response(
        &self,
        turn_id: ModelTurnId,
        commentary: Option<String>,
        calls: Vec<PendingToolCall>,
    ) -> Result<PreparedModelToolCallResponse, ErrorInfo> {
        let commentary_sequence = self.next_sequence();
        let tool_sequence = commentary_sequence + u64::from(commentary.is_some());
        let tool_payload = if calls.len() == 1 {
            RuntimeJournalPayload::ToolCallPending {
                call: calls[0].clone(),
            }
        } else {
            let batch_id = ToolCallBatchId::new(&format!("tool-batch-{}", self.next_sequence()))
                .map_err(|error| tool_response_diagnostic("tool_call_batch_id", error))?;
            let batch = PendingToolCallBatch::new(batch_id, calls.clone())
                .map_err(|error| tool_response_diagnostic("tool_call_batch_invalid", error))?;
            RuntimeJournalPayload::ToolCallBatchPending { batch }
        };

        for call in &calls {
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
        }

        let mut transcript = self.transcript.clone();
        let prepared_commentary = match commentary {
            Some(text) => {
                let artifact =
                    ArtifactRef::new(assistant_output_id(commentary_sequence), ArtifactKind::Text);
                let content = ArtifactContent::text(text);
                self.artifacts
                    .ensure_recordable(&artifact, &content)
                    .map_err(|error| {
                        tool_response_diagnostic("assistant_output_artifact", error)
                    })?;
                transcript
                    .push_assistant_text(turn_id, artifact.id().clone())
                    .map_err(|error| {
                        tool_response_diagnostic("assistant_output_artifact", error)
                    })?;
                Some(PreparedModelCommentary {
                    artifact,
                    content,
                    transcript: transcript.clone(),
                    sequence: commentary_sequence,
                })
            }
            None => None,
        };
        for call in &calls {
            transcript
                .push_tool_call(turn_id, call.clone())
                .map_err(transcript_record_diagnostic)?;
        }
        transcript
            .close_model_response(turn_id, true)
            .map_err(|error| tool_response_diagnostic("model_turn_close", error))?;

        Ok(PreparedModelToolCallResponse {
            commentary: prepared_commentary,
            transcript,
            calls,
            tool_payload,
            tool_sequence,
        })
    }

    pub(crate) fn record_prepared_model_commentary(
        &mut self,
        prepared: &mut PreparedModelToolCallResponse,
    ) -> Option<RuntimeJournalEvent> {
        let commentary = prepared.commentary.take()?;
        debug_assert_eq!(self.next_sequence(), commentary.sequence);
        let content_bytes = commentary.content.as_bytes().len();
        let recorded = self
            .artifacts
            .record_preflighted(commentary.artifact, commentary.content);
        Self::trace_artifact_record(self.session_id.as_str(), &recorded, content_bytes);
        self.transcript = commentary.transcript;
        Some(self.record_event(
            RuntimeJournalPayload::AssistantOutputRecorded { artifact: recorded },
            LedgerFactKind::ArtifactRecorded,
        ))
    }

    pub(crate) fn record_prepared_model_tool_calls(
        &mut self,
        prepared: PreparedModelToolCallResponse,
    ) -> RuntimeJournalEvent {
        debug_assert!(prepared.commentary.is_none());
        debug_assert_eq!(self.next_sequence(), prepared.tool_sequence);
        self.transcript = prepared.transcript;
        self.pending_tool_calls.extend(prepared.calls);
        self.record_event(prepared.tool_payload, LedgerFactKind::ToolCallPending)
    }

    pub(crate) fn record_bridge_tool_call_requested(
        &mut self,
        call: PendingToolCall,
    ) -> RuntimeJournalEvent {
        self.record_event(
            RuntimeJournalPayload::BridgeToolCallRequested { call },
            LedgerFactKind::BridgeToolCallRequested,
        )
    }
}

fn duplicate_tool_call_diagnostic(call_id: &ToolCallId, state: &'static str) -> ErrorInfo {
    ErrorInfo::new(
        "tool_call_duplicate",
        &format!("tool call {call_id} is {state}; duplicate pending admission rejected"),
    )
    .expect("duplicate tool call diagnostic uses static code and validated call id")
}

fn transcript_record_diagnostic(error: RuntimeError) -> ErrorInfo {
    ErrorInfo::new("transcript_record", &error.to_string())
        .expect("transcript diagnostic uses static code")
}

fn tool_response_diagnostic(code: &'static str, error: impl std::fmt::Display) -> ErrorInfo {
    ErrorInfo::new(code, &error.to_string())
        .expect("tool response diagnostic uses a static code and non-empty error message")
}
