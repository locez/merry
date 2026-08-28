//! Interactive runtime-event projection and output helpers.

use super::super::handles::InteractiveRunMessage;
use super::super::types::InteractiveError;
use super::InteractiveProducer;
use crate::events::RuntimeEventProjector;
use merry_core::{InteractiveRunState, RuntimeEvent, RuntimeJournalEvent, RuntimeJournalPayload};

impl InteractiveProducer {
    pub(super) async fn send_runtime_events(&mut self, events: Vec<RuntimeJournalEvent>) -> bool {
        let mut projector = RuntimeEventProjector::new();
        for event in events {
            if !self
                .project_and_send_runtime_event(&mut projector, event)
                .await
            {
                return false;
            }
        }
        true
    }

    pub(crate) async fn project_and_send_runtime_event(
        &mut self,
        projector: &mut RuntimeEventProjector,
        event: RuntimeJournalEvent,
    ) -> bool {
        if is_plan_payload(&event.payload) {
            if !self.seen_plan_sequences.insert(event.sequence) {
                return true;
            }
            if self.seen_plan_sequences.len() > 256
                && let Some(oldest) = self.seen_plan_sequences.first().copied()
            {
                self.seen_plan_sequences.remove(&oldest);
            }
        }
        let event = match projector.project(event, &self.runtime).await {
            Ok(Some(event)) => event,
            Ok(None) => return true,
            Err(error) => {
                self.remember_terminal_error(InteractiveError::Runtime { source: error });
                return false;
            }
        };
        self.send_event(event).await
    }

    pub(super) fn remember_terminal_error(&mut self, error: InteractiveError) {
        if self.terminal_error.is_none() {
            self.terminal_error = Some(error);
        }
    }

    pub(super) async fn send_queue_changed(&self) -> bool {
        self.send_event(RuntimeEvent::QueuedInputsChanged {
            inputs: self.queue.snapshot(),
        })
        .await
    }

    pub(super) async fn send_state(&self, state: InteractiveRunState) -> bool {
        self.send_event(RuntimeEvent::InteractiveRunStateChanged { state })
            .await
    }

    pub(super) async fn send_event(&self, event: RuntimeEvent) -> bool {
        self.message_sender
            .send(InteractiveRunMessage::Event(event))
            .await
            .is_ok()
    }
}

pub(crate) fn is_plan_payload(payload: &RuntimeJournalPayload) -> bool {
    matches!(
        payload,
        RuntimeJournalPayload::PlanUpdated { .. }
            | RuntimeJournalPayload::PlanPhaseChanged { .. }
            | RuntimeJournalPayload::PlanNodeReady { .. }
            | RuntimeJournalPayload::PlanLeaseStarted { .. }
            | RuntimeJournalPayload::PlanProgressUpdated { .. }
            | RuntimeJournalPayload::PlanProgressReviewRequested { .. }
            | RuntimeJournalPayload::PlanAttemptProgressReported { .. }
            | RuntimeJournalPayload::PlanDirectiveUpdated { .. }
            | RuntimeJournalPayload::PlanAttemptFinished { .. }
    )
}
