use super::{SessionState, history::ResolvedToolContinuationSnapshot};
use crate::{artifact::ArtifactError, ledger::LedgerFactKind};
use merry_core::{ErrorInfo, PendingToolCall, RuntimeEvent, RuntimeEventKind, ToolCallId};

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
}

fn duplicate_tool_call_diagnostic(call_id: &ToolCallId, state: &'static str) -> ErrorInfo {
    ErrorInfo::new(
        "tool_call_duplicate",
        &format!("tool call {call_id} is {state}; duplicate pending admission rejected"),
    )
    .expect("duplicate tool call diagnostic uses static code and validated call id")
}
