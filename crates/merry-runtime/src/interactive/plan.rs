use super::producer::InteractiveProducer;
use crate::events::RuntimeEventProjector;
use merry_core::RuntimeJournalEvent;

impl InteractiveProducer {
    /// Pauses the interactive tool loop whenever `exit_plan_mode` parked a plan
    /// for user review.
    pub(super) async fn runtime_tool_continuation(&self) -> Option<bool> {
        if self.interrupted {
            return Some(false);
        }
        Some(!self.runtime.awaiting_plan_approval().await)
    }

    pub(super) async fn forward_subagent_event(
        &mut self,
        event: Result<RuntimeJournalEvent, tokio::sync::broadcast::error::RecvError>,
    ) -> bool {
        match event {
            Ok(event) => {
                let mut projector = RuntimeEventProjector::new();
                self.project_and_send_runtime_event(&mut projector, event)
                    .await
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::debug!(skipped, "interactive subagent event receiver lagged");
                true
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => true,
        }
    }
}
