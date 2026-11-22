//! Interactive runtime-event projection and output helpers.

use super::super::handles::InteractiveRunMessage;
use super::super::types::InteractiveError;
use super::InteractiveProducer;
use crate::events::RuntimeEventProjector;
use merry_core::{InteractiveRunState, RuntimeEvent, RuntimeJournalEvent};

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
        if matches!(&event, RuntimeEvent::ModelOutputRateUpdated { .. }) {
            if self.rate_sender.is_closed() {
                return false;
            }
            self.rate_sender.send_replace(Some(event));
            return true;
        }
        self.message_sender
            .send(InteractiveRunMessage::Event(event))
            .await
            .is_ok()
    }
}
