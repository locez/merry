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
    ArtifactId, ArtifactRef, PlanAttemptId, PlanHarnessSnapshot, PlanId, PlanLeaseId,
    PlanLeaseStatus, PlanNodeId, PlanSnapshot, SessionId,
};

#[derive(Debug, Clone)]
pub(crate) struct PlanArtifactPromotion {
    pub(crate) artifact: ArtifactRef,
    pub(crate) content: ArtifactContent,
}

/// Scoped root-plan control carried by one depth-one subagent runtime.
#[derive(Clone)]
pub struct PlanSubagentControl {
    controller: PlanController,
    plan_id: PlanId,
    node_id: PlanNodeId,
    attempt_id: PlanAttemptId,
    lease_id: PlanLeaseId,
    executor_session_id: SessionId,
}

impl PlanSubagentControl {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        controller: PlanController,
        plan_id: PlanId,
        node_id: PlanNodeId,
        attempt_id: PlanAttemptId,
        lease_id: PlanLeaseId,
        executor_session_id: SessionId,
    ) -> Self {
        Self {
            controller,
            plan_id,
            node_id,
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
        let id = ArtifactId::new(&format!("plan-subagent-artifact-{hash}"))
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

    pub(crate) async fn record_runtime_effect(
        &self,
        changed_paths: Vec<String>,
        now_ms: u64,
    ) -> Result<PlanCommandResult<PlanProgressOutput>, PlanControllerError> {
        self.controller
            .record_runtime_effect(self.actor(), changed_paths, now_ms)
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

    pub(crate) async fn active_harness(&self) -> Result<PlanHarnessSnapshot, PlanControllerError> {
        let snapshot = self.snapshot().await?;
        let attempt = snapshot
            .attempts
            .iter()
            .find(|attempt| attempt.attempt_id == self.attempt_id)
            .ok_or_else(|| PlanControllerError::Plan {
                source: PlanError::UnknownAttempt {
                    attempt_id: self.attempt_id.clone(),
                },
            })?;
        if attempt.outcome.is_some() {
            return Err(PlanControllerError::Plan {
                source: PlanError::AttemptAlreadyResolved {
                    attempt_id: self.attempt_id.clone(),
                },
            });
        }
        if attempt.node_id != self.node_id
            || attempt.lease_id.as_ref() != Some(&self.lease_id)
            || attempt.executor_session_id != self.executor_session_id
        {
            return Err(PlanControllerError::Plan {
                source: PlanError::AttemptOwnershipMismatch {
                    attempt_id: self.attempt_id.clone(),
                },
            });
        }
        let lease = snapshot
            .leases
            .iter()
            .find(|lease| lease.lease_id == self.lease_id)
            .ok_or_else(|| PlanControllerError::Plan {
                source: PlanError::UnknownLease {
                    lease_id: self.lease_id.clone(),
                },
            })?;
        if lease.status != PlanLeaseStatus::Live {
            return Err(PlanControllerError::Plan {
                source: PlanError::LeaseNotLive {
                    lease_id: self.lease_id.clone(),
                },
            });
        }
        if lease.attempt_id != self.attempt_id
            || lease.node_id != self.node_id
            || lease.executor_session_id != self.executor_session_id
        {
            return Err(PlanControllerError::Plan {
                source: PlanError::AttemptOwnershipMismatch {
                    attempt_id: self.attempt_id.clone(),
                },
            });
        }
        if lease.node_revision != attempt.node_revision {
            return Err(PlanControllerError::Plan {
                source: PlanError::AttemptNodeRevisionMismatch {
                    expected: attempt.node_revision,
                    actual: lease.node_revision,
                },
            });
        }
        snapshot
            .nodes
            .iter()
            .find(|node| node.id == self.node_id)
            .map(|node| node.harness.clone())
            .ok_or_else(|| PlanControllerError::Plan {
                source: PlanError::UnknownNode {
                    node_id: self.node_id.clone(),
                },
            })
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

    fn actor(&self) -> PlanAttemptActor {
        PlanAttemptActor {
            executor_session_id: self.executor_session_id.clone(),
        }
    }
}

impl std::fmt::Debug for PlanSubagentControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlanSubagentControl")
            .field("plan_id", &self.plan_id)
            .field("node_id", &self.node_id)
            .field("attempt_id", &self.attempt_id)
            .field("lease_id", &self.lease_id)
            .field("executor_session_id", &self.executor_session_id)
            .finish_non_exhaustive()
    }
}
