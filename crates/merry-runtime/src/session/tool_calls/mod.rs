use super::SessionState;
use crate::{RuntimeError, ledger::LedgerFactKind};
use merry_core::{
    ErrorInfo, PendingToolCall, PendingToolCallBatch, RuntimeJournalEvent, RuntimeJournalPayload,
    ToolCallId,
};

mod action;
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

    pub(crate) fn record_tool_call_pending(
        &mut self,
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

        self.transcript
            .push_tool_call(call.clone())
            .map_err(transcript_record_diagnostic)?;
        self.pending_tool_calls.push(call.clone());
        Ok(self.record_event(
            RuntimeJournalPayload::ToolCallPending { call },
            LedgerFactKind::ToolCallPending,
        ))
    }

    pub(crate) fn record_tool_call_batch_pending(
        &mut self,
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
                .push_tool_call(call.clone())
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
