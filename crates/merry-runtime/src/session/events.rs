use super::SessionState;
use crate::ledger::LedgerFactKind;
use merry_core::{ErrorInfo, RuntimeJournalEvent, RuntimeJournalPayload};

impl SessionState {
    pub(crate) fn record_session_started_if_needed(&mut self) -> Option<RuntimeJournalEvent> {
        if self.session_started {
            return None;
        }

        self.session_started = true;
        Some(self.record_event(
            RuntimeJournalPayload::SessionStarted,
            LedgerFactKind::SessionStarted,
        ))
    }

    pub(crate) fn record_step_started(&mut self) -> RuntimeJournalEvent {
        self.record_event(
            RuntimeJournalPayload::StepStarted,
            LedgerFactKind::StepStarted,
        )
    }

    pub(crate) fn record_model_retry_event(
        &mut self,
        payload: RuntimeJournalPayload,
    ) -> RuntimeJournalEvent {
        self.record_event(payload, LedgerFactKind::ModelRetry)
    }

    pub(crate) fn record_transient_event(
        &mut self,
        payload: RuntimeJournalPayload,
    ) -> RuntimeJournalEvent {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        RuntimeJournalEvent::new(self.session_id.clone(), sequence, payload)
    }

    pub(crate) fn record_step_completed(&mut self) -> RuntimeJournalEvent {
        self.record_event(
            RuntimeJournalPayload::StepCompleted,
            LedgerFactKind::StepCompleted,
        )
    }

    pub(crate) fn record_compaction_started(&mut self) -> RuntimeJournalEvent {
        self.record_event(
            RuntimeJournalPayload::CompactionStarted,
            LedgerFactKind::CompactionStarted,
        )
    }

    pub(crate) fn record_compaction_completed(
        &mut self,
        checkpoint_id: String,
        covered_history_item_count: usize,
    ) -> RuntimeJournalEvent {
        self.record_event(
            RuntimeJournalPayload::CompactionCompleted {
                checkpoint_id,
                covered_history_item_count,
            },
            LedgerFactKind::CompactionCompleted,
        )
    }

    pub(crate) fn record_cancelled(&mut self, diagnostic: ErrorInfo) -> RuntimeJournalEvent {
        self.record_event(
            RuntimeJournalPayload::Cancelled { diagnostic },
            LedgerFactKind::Cancelled,
        )
    }

    pub(crate) fn record_failed(&mut self, diagnostic: ErrorInfo) -> RuntimeJournalEvent {
        self.record_event(
            RuntimeJournalPayload::Failed { diagnostic },
            LedgerFactKind::Failed,
        )
    }

    pub(super) fn record_event(
        &mut self,
        payload: RuntimeJournalPayload,
        fact_kind: LedgerFactKind,
    ) -> RuntimeJournalEvent {
        let sequence = self.next_sequence;
        self.ledger.record(sequence, fact_kind);
        self.next_sequence += 1;
        RuntimeJournalEvent::new(self.session_id.clone(), sequence, payload)
    }

    pub(crate) fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
}
