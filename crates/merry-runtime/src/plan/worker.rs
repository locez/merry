use super::{
    PlanController, PlanControllerError, PlanError,
    controller::PlanCommandResult,
    execution::{
        PlanAttemptActor, PlanAttemptReportOutput, PlanDirectiveDeliveryOutput, PlanProgressOutput,
    },
    protocol::{ReportPlanAttemptInput, ReportPlanProgressInput},
    recovery::PlanProgressReviewOutput,
};
use crate::{ArtifactContent, context::stable_content_hash};
use merry_core::{
    ArtifactId, ArtifactRef, PlanAttemptId, PlanId, PlanLeaseId, PlanNodeId, PlanSnapshot,
    SessionId,
};

#[derive(Debug, Clone)]
pub(crate) struct PlanArtifactPromotion {
    pub(crate) artifact: ArtifactRef,
    pub(crate) content: ArtifactContent,
}

/// Scoped root-plan control carried by one depth-one worker runtime.
#[derive(Clone)]
pub struct PlanWorkerControl {
    controller: PlanController,
    plan_id: PlanId,
    node_id: PlanNodeId,
    node_revision: u64,
    attempt_id: PlanAttemptId,
    lease_id: PlanLeaseId,
    executor_session_id: SessionId,
}

impl PlanWorkerControl {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        controller: PlanController,
        plan_id: PlanId,
        node_id: PlanNodeId,
        node_revision: u64,
        attempt_id: PlanAttemptId,
        lease_id: PlanLeaseId,
        executor_session_id: SessionId,
    ) -> Self {
        Self {
            controller,
            plan_id,
            node_id,
            node_revision,
            attempt_id,
            lease_id,
            executor_session_id,
        }
    }

    pub(crate) async fn report_progress(
        &self,
        input: ReportPlanProgressInput,
        artifact_promotions: Vec<PlanArtifactPromotion>,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError> {
        self.validate_lease(input.lease_id.clone(), input.expected_node_revision)?;
        self.controller
            .progress_with_artifact_promotions(self.actor(), input, artifact_promotions, now_ms)
            .await
    }

    pub(crate) async fn report_attempt(
        &self,
        input: ReportPlanAttemptInput,
        artifact_promotions: Vec<PlanArtifactPromotion>,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanAttemptReportOutput>, PlanControllerError> {
        self.validate_lease(input.lease_id.clone(), input.expected_node_revision)?;
        self.controller
            .attempt_report_with_artifact_promotions(
                self.actor(),
                input,
                artifact_promotions,
                now_ms,
            )
            .await
    }

    pub(crate) fn promoted_artifact_ref(&self, source: &ArtifactRef) -> ArtifactRef {
        let material = format!(
            "{}\0{}\0{}\0{}",
            self.plan_id,
            self.attempt_id,
            self.executor_session_id,
            source.id()
        );
        let fingerprint = stable_content_hash(material.as_bytes());
        let hash = fingerprint
            .rsplit_once(':')
            .map(|(_, hash)| hash)
            .expect("stable content hashes include an algorithm prefix");
        let id = ArtifactId::new(&format!("plan-worker-artifact-{hash}"))
            .expect("runtime-generated promoted artifact id is valid");
        let promoted = ArtifactRef::new(id, source.kind().clone());
        match source.label() {
            Some(label) => promoted
                .with_label(label)
                .expect("source artifact labels were already validated"),
            None => promoted,
        }
    }

    pub(crate) async fn heartbeat(
        &self,
        now_ms: u64,
        provider_request_in_flight: bool,
        tool_call_in_flight: bool,
    ) -> Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError> {
        self.controller
            .heartbeat(
                self.actor(),
                self.lease_id.clone(),
                now_ms,
                provider_request_in_flight,
                tool_call_in_flight,
            )
            .await
    }

    pub(crate) async fn deliver_directives(
        &self,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanDirectiveDeliveryOutput>, PlanControllerError> {
        self.controller
            .deliver_directives(self.actor(), self.lease_id.clone(), now_ms)
            .await
    }

    pub(crate) async fn review_progress_at_boundary(
        &self,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanProgressReviewOutput>, PlanControllerError> {
        self.controller
            .review_progress_at_boundary(self.actor(), self.lease_id.clone(), now_ms)
            .await
    }

    pub(crate) async fn cancel_attempt(
        &self,
        reason: &str,
        now_ms: u64,
    ) -> Result<
        PlanCommandResult<super::recovery::PlanAttemptCancellationOutput>,
        PlanControllerError,
    > {
        self.controller
            .cancel_attempt(
                self.actor(),
                self.lease_id.clone(),
                reason.to_owned(),
                now_ms,
            )
            .await
    }

    pub(crate) async fn snapshot(&self) -> Result<PlanSnapshot, PlanControllerError> {
        let snapshot = self
            .controller
            .snapshot()
            .await?
            .ok_or(PlanControllerError::NoActivePlan)?;
        if snapshot.plan_id != self.plan_id {
            return Err(PlanControllerError::Plan {
                source: PlanError::UnknownAttempt {
                    attempt_id: self.attempt_id.clone(),
                },
            });
        }
        Ok(snapshot)
    }

    pub(crate) fn node_id(&self) -> &PlanNodeId {
        &self.node_id
    }

    pub(crate) fn attempt_id(&self) -> &PlanAttemptId {
        &self.attempt_id
    }

    pub(crate) fn lease_id(&self) -> &PlanLeaseId {
        &self.lease_id
    }

    pub(crate) fn node_revision(&self) -> u64 {
        self.node_revision
    }

    fn actor(&self) -> PlanAttemptActor {
        PlanAttemptActor {
            executor_session_id: self.executor_session_id.clone(),
        }
    }

    fn validate_lease(
        &self,
        lease_id: PlanLeaseId,
        node_revision: u64,
    ) -> Result<(), PlanControllerError> {
        if lease_id != self.lease_id || node_revision != self.node_revision {
            return Err(PlanControllerError::Plan {
                source: PlanError::AttemptNodeRevisionMismatch {
                    expected: node_revision,
                    actual: self.node_revision,
                },
            });
        }
        Ok(())
    }
}

impl std::fmt::Debug for PlanWorkerControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlanWorkerControl")
            .field("plan_id", &self.plan_id)
            .field("node_id", &self.node_id)
            .field("node_revision", &self.node_revision)
            .field("attempt_id", &self.attempt_id)
            .field("lease_id", &self.lease_id)
            .field("executor_session_id", &self.executor_session_id)
            .finish_non_exhaustive()
    }
}
