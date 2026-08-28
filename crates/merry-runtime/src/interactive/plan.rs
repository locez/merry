use super::producer::InteractiveProducer;
use crate::events::RuntimeEventProjector;
use merry_core::{PlanAttemptOutcome, PlanPhase, RuntimeJournalEvent, RuntimeJournalPayload};

impl InteractiveProducer {
    pub(super) async fn runtime_tool_continuation(&self) -> Option<bool> {
        if self.interrupted {
            return Some(false);
        }
        match self.runtime.plan_snapshot().await {
            Ok(Some(snapshot))
                if snapshot.phase == PlanPhase::AwaitingApproval
                    || (snapshot.phase == PlanPhase::Planning
                        && snapshot.root_node_id.is_some()) =>
            {
                Some(false)
            }
            Ok(_) => Some(true),
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    "interactive plan phase check after runtime tool failed"
                );
                None
            }
        }
    }

    pub(super) async fn forward_plan_event(
        &mut self,
        event: Result<RuntimeJournalEvent, tokio::sync::broadcast::error::RecvError>,
    ) -> bool {
        match event {
            Ok(event) => {
                if super::producer::is_plan_payload(&event.payload)
                    && self.seen_plan_sequences.contains(&event.sequence)
                {
                    return true;
                }
                self.observe_plan_wakeup(&event.payload);
                let mut projector = RuntimeEventProjector::new();
                self.project_and_send_runtime_event(&mut projector, event)
                    .await
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::debug!(skipped, "interactive plan event receiver lagged");
                true
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => true,
        }
    }

    fn observe_plan_wakeup(&mut self, payload: &RuntimeJournalPayload) {
        match payload {
            RuntimeJournalPayload::PlanUpdated { snapshot, .. }
                if matches!(snapshot.phase, PlanPhase::Completed | PlanPhase::Blocked) =>
            {
                self.coordinator_continuation_requested = true;
            }
            RuntimeJournalPayload::PlanProgressReviewRequested { .. } => {
                self.coordinator_continuation_requested = true;
            }
            RuntimeJournalPayload::PlanAttemptFinished { attempt }
                if matches!(
                    attempt.outcome,
                    Some(PlanAttemptOutcome::SemanticFailure | PlanAttemptOutcome::Blocked)
                ) =>
            {
                self.coordinator_continuation_requested = true;
            }
            _ => {}
        }
    }
}
