use super::SessionState;
use merry_core::{ErrorInfo, RuntimeJournalEvent, RuntimeJournalPayload};

impl SessionState {
    pub(crate) fn record_session_started_if_needed(&mut self) -> Option<RuntimeJournalEvent> {
        if self.session_started {
            return None;
        }

        self.session_started = true;
        Some(self.record_event(RuntimeJournalPayload::SessionStarted))
    }

    pub(crate) fn record_step_started(&mut self) -> RuntimeJournalEvent {
        self.record_event(RuntimeJournalPayload::StepStarted)
    }

    pub(crate) fn record_model_retry_event(
        &mut self,
        payload: RuntimeJournalPayload,
    ) -> RuntimeJournalEvent {
        self.record_event(payload)
    }

    pub(crate) fn record_transient_event(
        &mut self,
        payload: RuntimeJournalPayload,
    ) -> RuntimeJournalEvent {
        self.record_event(payload)
    }

    pub(crate) fn record_step_completed(&mut self) -> RuntimeJournalEvent {
        self.record_event(RuntimeJournalPayload::StepCompleted)
    }

    pub(crate) fn record_compaction_started(&mut self) -> RuntimeJournalEvent {
        self.record_event(RuntimeJournalPayload::CompactionStarted)
    }

    pub(crate) fn record_compaction_completed(
        &mut self,
        checkpoint_id: String,
        covered_history_item_count: usize,
    ) -> RuntimeJournalEvent {
        self.record_event(RuntimeJournalPayload::CompactionCompleted {
            checkpoint_id,
            covered_history_item_count,
        })
    }

    pub(crate) fn record_cancelled(&mut self, diagnostic: ErrorInfo) -> RuntimeJournalEvent {
        self.record_event(RuntimeJournalPayload::Cancelled { diagnostic })
    }

    pub(crate) fn record_failed(&mut self, diagnostic: ErrorInfo) -> RuntimeJournalEvent {
        self.record_event(RuntimeJournalPayload::Failed { diagnostic })
    }

    pub(super) fn record_event(&mut self, payload: RuntimeJournalPayload) -> RuntimeJournalEvent {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        RuntimeJournalEvent::new(self.session_id.clone(), sequence, payload)
    }

    pub(crate) fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
}
