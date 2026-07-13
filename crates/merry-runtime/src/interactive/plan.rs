use super::InteractiveProducer;
use crate::{events::RuntimeEventProjector, plan::ReportPlanAttemptInput};
use merry_core::{
    ErrorInfo, PlanAttemptOutcome, PlanPhase, RuntimeJournalEvent, RuntimeJournalPayload,
};

impl InteractiveProducer {
    pub(super) async fn refresh_coordinator_continuation_request(&mut self) {
        let Ok(Some(snapshot)) = self.runtime.plan_snapshot().await else {
            return;
        };
        self.coordinator_continuation_requested = snapshot.attempts.iter().any(|attempt| {
            attempt.outcome.is_none()
                && attempt.lease_id.is_none()
                && attempt.executor_session_id == *self.runtime.session_id()
        });
    }

    pub(super) async fn fail_unreported_local_attempt(&self) {
        let diagnostic = ErrorInfo::new(
            "missing_attempt_report",
            "local coordinator turn completed without report_plan_attempt",
        )
        .expect("static missing-report diagnostic is valid");
        self.finish_local_attempt(PlanAttemptOutcome::TransientFailure, diagnostic)
            .await;
    }

    pub(super) async fn finish_local_attempt(
        &self,
        outcome: PlanAttemptOutcome,
        diagnostic: ErrorInfo,
    ) {
        let Ok(Some(snapshot)) = self.runtime.plan_snapshot().await else {
            return;
        };
        if !snapshot.attempts.iter().any(|attempt| {
            attempt.outcome.is_none()
                && attempt.lease_id.is_none()
                && attempt.executor_session_id == *self.runtime.session_id()
        }) {
            return;
        }
        let input = ReportPlanAttemptInput {
            outcome,
            result: None,
            diagnostic: Some(diagnostic),
            decomposition: None,
            acknowledged_directive_ids: Vec::new(),
            applied_directive_ids: Vec::new(),
        };
        if let Err(error) = self.runtime.report_current_local_plan_attempt(input).await {
            tracing::debug!(error = %error, "failed to close local plan attempt at runtime boundary");
        }
    }

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
                if super::is_plan_payload(&event.payload)
                    && self.seen_plan_sequences.contains(&event.sequence)
                {
                    return true;
                }
                self.observe_plan_wakeup(&event.payload);
                if !self.coordinator_continuation_requested {
                    self.refresh_coordinator_continuation_request().await;
                }
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
