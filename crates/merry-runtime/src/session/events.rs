use super::SessionState;
use crate::ledger::LedgerFactKind;
use merry_core::{ErrorInfo, RuntimeEvent, RuntimeEventKind};

impl SessionState {
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

    pub(super) fn record_event(
        &mut self,
        kind: RuntimeEventKind,
        fact_kind: LedgerFactKind,
    ) -> RuntimeEvent {
        let sequence = self.next_sequence;
        self.ledger.record(sequence, fact_kind);
        self.next_sequence += 1;
        RuntimeEvent::new(self.session_id.clone(), sequence, kind)
    }

    pub(super) fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
}
