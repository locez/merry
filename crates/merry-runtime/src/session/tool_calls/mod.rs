use super::{ModelTurnId, SessionState};
use crate::RuntimeError;
use merry_core::{
    ErrorInfo, PendingToolCall, PendingToolCallBatch, RuntimeJournalEvent, RuntimeJournalPayload,
    ToolCallBatchId, ToolCallId,
};

mod content;
mod result;
mod skill;

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

    pub(crate) fn replace_pending_tool_call_arguments(
        &mut self,
        call_id: &ToolCallId,
        arguments: merry_core::ToolCallArguments,
    ) -> Result<PendingToolCall, RuntimeError> {
        let Some(pending) = self
            .pending_tool_calls
            .iter_mut()
            .find(|call| call.id() == call_id)
        else {
            return Err(RuntimeError::UnknownToolCall {
                session_id: self.session_id.clone(),
                call_id: call_id.clone(),
            });
        };
        *pending = PendingToolCall::new(pending.id().clone(), pending.name().clone(), arguments);
        Ok(pending.clone())
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
        Ok(self.record_event(RuntimeJournalPayload::ToolCallPending { call }))
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
        Ok(self.record_event(RuntimeJournalPayload::ToolCallBatchPending { batch }))
    }

    pub(crate) fn record_model_tool_call_response(
        &mut self,
        turn_id: ModelTurnId,
        commentary: Option<String>,
        calls: Vec<PendingToolCall>,
    ) -> Result<(Option<RuntimeJournalEvent>, RuntimeJournalEvent), ErrorInfo> {
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
        if let Some(text) = commentary.as_deref() {
            transcript
                .push_assistant_text(turn_id, text.into())
                .map_err(|error| tool_response_diagnostic("assistant_output_record", error))?;
        }
        for call in &calls {
            transcript
                .push_tool_call(turn_id, call.clone())
                .map_err(transcript_record_diagnostic)?;
        }
        transcript
            .close_model_response(turn_id, true)
            .map_err(|error| tool_response_diagnostic("model_turn_close", error))?;

        self.transcript = transcript;
        self.pending_tool_calls.extend(calls);
        let commentary_event = commentary
            .map(|text| self.record_event(RuntimeJournalPayload::AssistantOutputRecorded { text }));
        let tool_event = self.record_event(tool_payload);
        Ok((commentary_event, tool_event))
    }

    pub(crate) fn record_bridge_tool_call_requested(
        &mut self,
        call: PendingToolCall,
    ) -> RuntimeJournalEvent {
        self.record_event(RuntimeJournalPayload::BridgeToolCallRequested { call })
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
