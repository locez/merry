use super::{PlanError, PlanState, execution::PlanAttemptActor};
use merry_core::{
    CoordinatorDirectiveSnapshot, ErrorInfo, PlanAttemptOutcome, PlanAttemptProgressSnapshot,
    PlanAttemptSnapshot, PlanLeaseId, PlanLeaseStatus, PlanLinkStatus, PlanNodeId, PlanNodeStatus,
    PlanPhase, PlanSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanRecoveryOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) interrupted_attempts: Vec<PlanAttemptSnapshot>,
    pub(crate) updated_directives: Vec<CoordinatorDirectiveSnapshot>,
    pub(crate) ready_node_ids: Vec<PlanNodeId>,
    pub(crate) previous_phase: PlanPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanProgressReviewOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) updated_progress: Option<PlanAttemptProgressSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanAttemptCancellationOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) attempt: PlanAttemptSnapshot,
    pub(crate) updated_directives: Vec<CoordinatorDirectiveSnapshot>,
    pub(crate) previous_phase: PlanPhase,
}

pub(super) fn retry_backoff_elapsed(
    snapshot: &PlanSnapshot,
    node: &merry_core::PlanNodeSnapshot,
    now_ms: u64,
) -> bool {
    if node.recovery_policy.retry_backoff_ms == 0 {
        return true;
    }
    snapshot
        .attempts
        .iter()
        .rev()
        .find(|attempt| {
            attempt.node_id == node.id
                && attempt.outcome == Some(PlanAttemptOutcome::TransientFailure)
        })
        .and_then(|attempt| attempt.finished_at_ms)
        .is_none_or(|finished_at_ms| {
            finished_at_ms.saturating_add(node.recovery_policy.retry_backoff_ms) <= now_ms
        })
}

fn recompute_link_projection(node: &mut merry_core::PlanNodeSnapshot) {
    let mut summary = merry_core::PlanExecutionSummary::default();
    for link in node
        .links
        .iter()
        .filter(|link| link.status != PlanLinkStatus::Superseded && link.superseded_by.is_none())
    {
        match link.status {
            PlanLinkStatus::Active => summary.active += 1,
            PlanLinkStatus::Completed => summary.completed += 1,
            PlanLinkStatus::Failed => summary.failed += 1,
            PlanLinkStatus::Cancelled => summary.cancelled += 1,
            PlanLinkStatus::Blocked => summary.blocked += 1,
            PlanLinkStatus::Superseded => {}
        }
    }
    node.execution_summary = summary;
}

impl PlanState {
    /// Makes persisted execution state inert when a session is loaded.
    ///
    /// Child runtimes and their cancellation tokens are process-local, so a
    /// loaded session cannot honestly restore an old attempt or active link.
    /// Preserve the authored tree and historical records, but close any
    /// in-flight runtime state instead of replaying it or leaving it live.
    pub(crate) fn abandon_unresumed_execution(&mut self, now_ms: u64) -> bool {
        let attempt_ids = self
            .snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.outcome.is_none())
            .map(|attempt| attempt.attempt_id.clone())
            .collect::<Vec<_>>();
        let linked_node_ids = self
            .snapshot
            .nodes
            .iter()
            .filter(|node| {
                node.links
                    .iter()
                    .any(|link| link.status == PlanLinkStatus::Active)
            })
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        if attempt_ids.is_empty() && linked_node_ids.is_empty() {
            return false;
        }

        let revision = self
            .advance_revision("persisted execution was not resumed")
            .expect("static resume cleanup summary is valid");
        for node in &mut self.snapshot.nodes {
            let mut cancelled_link = false;
            for link in &mut node.links {
                if link.status == PlanLinkStatus::Active {
                    link.status = PlanLinkStatus::Cancelled;
                    link.terminal_at_ms = Some(now_ms);
                    cancelled_link = true;
                }
            }
            if cancelled_link {
                recompute_link_projection(node);
                node.status = PlanNodeStatus::Blocked;
                node.updated_revision = revision;
            }
        }

        let diagnostic = ErrorInfo::new(
            "plan_attempt_interrupted",
            "persisted execution had no live subagent after session load",
        )
        .expect("static resume diagnostic is valid");
        for attempt_id in attempt_ids {
            let Some(attempt_index) = self
                .snapshot
                .attempts
                .iter()
                .position(|attempt| attempt.attempt_id == attempt_id)
            else {
                continue;
            };
            let node_id = self.snapshot.attempts[attempt_index].node_id.clone();
            let started_at_ms = self.snapshot.attempts[attempt_index].started_at_ms;
            let lease_id = self.snapshot.attempts[attempt_index].lease_id.clone();
            {
                let attempt = &mut self.snapshot.attempts[attempt_index];
                attempt.finished_at_ms = Some(now_ms);
                attempt.outcome = Some(PlanAttemptOutcome::Interrupted);
                attempt.diagnostic = Some(diagnostic.clone());
            }
            if let Some(lease_id) = lease_id
                && let Some(lease) = self
                    .snapshot
                    .leases
                    .iter_mut()
                    .find(|lease| lease.lease_id == lease_id)
                && lease.status == PlanLeaseStatus::Live
            {
                lease.status = PlanLeaseStatus::Expired;
            }
            if let Some(progress) = self
                .snapshot
                .attempt_progress
                .iter_mut()
                .find(|progress| progress.attempt_id == attempt_id)
            {
                progress.elapsed_ms = now_ms.saturating_sub(started_at_ms);
                progress.last_runtime_activity_at_ms = now_ms;
                progress.provider_request_in_flight = false;
                progress.tool_call_in_flight = false;
                progress.request_coordinator_review = false;
            }
            self.expire_attempt_directives(&attempt_id);
            if let Some(node) = self
                .snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == node_id)
            {
                node.status = PlanNodeStatus::Blocked;
                node.updated_revision = revision;
            }
        }
        self.refresh_parent_states(revision);
        self.refresh_terminal_phase();
        true
    }

    pub(crate) fn cancel_attempt(
        &mut self,
        actor: &PlanAttemptActor,
        lease_id: &PlanLeaseId,
        reason: &str,
        now_ms: u64,
    ) -> Result<PlanAttemptCancellationOutput, PlanError> {
        validation_reason(reason)?;
        if self.snapshot.scheduler_status != merry_core::PlanSchedulerStatus::Draining {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "cancel plan attempt",
            });
        }
        let mut candidate = self.clone();
        let expected_node_revision = candidate
            .snapshot
            .leases
            .iter()
            .find(|lease| &lease.lease_id == lease_id)
            .ok_or_else(|| PlanError::UnknownLease {
                lease_id: lease_id.clone(),
            })?
            .node_revision;
        let (attempt_index, lease_index) =
            candidate.validate_live_lease(actor, lease_id, expected_node_revision)?;
        let previous_phase = candidate.snapshot.phase;
        let attempt_id = candidate.snapshot.attempts[attempt_index]
            .attempt_id
            .clone();
        let node_id = candidate.snapshot.attempts[attempt_index].node_id.clone();
        let revision = candidate.advance_revision("plan attempt cancelled")?;
        let diagnostic = ErrorInfo::new("plan_attempt_cancelled", reason)
            .expect("validated cancellation reason produces a valid diagnostic");
        {
            let attempt = &mut candidate.snapshot.attempts[attempt_index];
            attempt.finished_at_ms = Some(now_ms);
            attempt.outcome = Some(PlanAttemptOutcome::Cancelled);
            attempt.diagnostic = Some(diagnostic);
        }
        candidate.snapshot.leases[lease_index].status = PlanLeaseStatus::Cancelled;
        let node = candidate
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.id == node_id)
            .expect("cancelled attempt node remains present");
        node.status = PlanNodeStatus::Blocked;
        node.updated_revision = revision;
        let updated_directives = candidate.expire_attempt_directives(&attempt_id);
        candidate.settle_draining_phase();
        let attempt = candidate.snapshot.attempts[attempt_index].clone();
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanAttemptCancellationOutput {
            snapshot,
            attempt,
            updated_directives,
            previous_phase,
        })
    }

    pub(crate) fn review_progress_at_boundary(
        &mut self,
        actor: &PlanAttemptActor,
        lease_id: &PlanLeaseId,
        now_ms: u64,
    ) -> Result<PlanProgressReviewOutput, PlanError> {
        let mut candidate = self.clone();
        let expected_node_revision = candidate
            .snapshot
            .leases
            .iter()
            .find(|lease| &lease.lease_id == lease_id)
            .ok_or_else(|| PlanError::UnknownLease {
                lease_id: lease_id.clone(),
            })?
            .node_revision;
        let (attempt_index, lease_index) =
            candidate.validate_live_lease(actor, lease_id, expected_node_revision)?;
        let attempt_id = candidate.snapshot.attempts[attempt_index]
            .attempt_id
            .clone();
        let progress_index = candidate
            .snapshot
            .attempt_progress
            .iter()
            .position(|progress| progress.attempt_id == attempt_id)
            .expect("live attempt has progress state");
        let progress = &candidate.snapshot.attempt_progress[progress_index];
        let baseline = progress
            .last_durable_progress_at_ms
            .unwrap_or(candidate.snapshot.leases[lease_index].started_at_ms);
        let window = candidate
            .snapshot
            .resource_policy_snapshot
            .no_durable_progress_review_window_ms;
        if progress.request_coordinator_review || now_ms.saturating_sub(baseline) < window {
            return Ok(PlanProgressReviewOutput {
                snapshot: candidate.snapshot,
                updated_progress: None,
            });
        }

        let progress = &mut candidate.snapshot.attempt_progress[progress_index];
        progress.elapsed_ms =
            now_ms.saturating_sub(candidate.snapshot.leases[lease_index].started_at_ms);
        progress.last_runtime_activity_at_ms = now_ms;
        progress.request_coordinator_review = true;
        let updated_progress = progress.clone();
        candidate.advance_revision("plan progress review requested")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanProgressReviewOutput {
            snapshot,
            updated_progress: Some(updated_progress),
        })
    }

    pub(crate) fn interrupt_expired_leases(
        &mut self,
        now_ms: u64,
    ) -> Result<PlanRecoveryOutput, PlanError> {
        self.interrupt_attempts(now_ms)
    }

    fn interrupt_attempts(&mut self, now_ms: u64) -> Result<PlanRecoveryOutput, PlanError> {
        let mut candidate = self.clone();
        let previous_phase = candidate.snapshot.phase;
        let attempt_ids = candidate
            .snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.outcome.is_none())
            .filter(|attempt| {
                attempt.lease_id.as_ref().is_some_and(|lease_id| {
                    candidate.snapshot.leases.iter().any(|lease| {
                        &lease.lease_id == lease_id
                            && lease.status == PlanLeaseStatus::Live
                            && lease.lease_expires_at_ms <= now_ms
                    })
                })
            })
            .map(|attempt| attempt.attempt_id.clone())
            .collect::<Vec<_>>();
        if attempt_ids.is_empty() {
            return Ok(PlanRecoveryOutput {
                snapshot: candidate.snapshot,
                interrupted_attempts: Vec::new(),
                updated_directives: Vec::new(),
                ready_node_ids: Vec::new(),
                previous_phase,
            });
        }

        let revision = candidate.advance_revision("expired subagent plan leases interrupted")?;
        let mut interrupted_attempts = Vec::with_capacity(attempt_ids.len());
        let mut updated_directives = Vec::new();
        for attempt_id in attempt_ids {
            let attempt_index = candidate
                .snapshot
                .attempts
                .iter()
                .position(|attempt| attempt.attempt_id == attempt_id)
                .expect("selected attempt remains present");
            let lease_index = candidate.snapshot.attempts[attempt_index]
                .lease_id
                .as_ref()
                .map(|lease_id| {
                    candidate
                        .snapshot
                        .leases
                        .iter()
                        .position(|lease| &lease.lease_id == lease_id)
                        .expect("subagent attempt lease remains present")
                });
            debug_assert!(lease_index.is_some());
            let node_id = candidate.snapshot.attempts[attempt_index].node_id.clone();
            let node_status = if candidate.interrupted_attempt_can_retry(&node_id, &attempt_id) {
                PlanNodeStatus::Pending
            } else {
                PlanNodeStatus::Blocked
            };
            let diagnostic_message = if lease_index.is_some() {
                "subagent lease expired without a terminal attempt report"
            } else {
                unreachable!("local attempts never participate in lease expiry")
            };
            let diagnostic = ErrorInfo::new("plan_attempt_interrupted", diagnostic_message)
                .expect("static interruption diagnostic is valid");
            {
                let attempt = &mut candidate.snapshot.attempts[attempt_index];
                attempt.finished_at_ms = Some(now_ms);
                attempt.outcome = Some(PlanAttemptOutcome::Interrupted);
                attempt.diagnostic = Some(diagnostic);
                interrupted_attempts.push(attempt.clone());
            }
            if let Some(lease_index) = lease_index {
                candidate.snapshot.leases[lease_index].status = PlanLeaseStatus::Expired;
            }
            if let Some(progress) = candidate
                .snapshot
                .attempt_progress
                .iter_mut()
                .find(|progress| progress.attempt_id == attempt_id)
            {
                progress.elapsed_ms =
                    now_ms.saturating_sub(candidate.snapshot.attempts[attempt_index].started_at_ms);
                progress.last_runtime_activity_at_ms = now_ms;
                progress.provider_request_in_flight = false;
                progress.tool_call_in_flight = false;
            }
            let node = candidate
                .snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == node_id)
                .expect("selected lease node remains present");
            node.status = node_status;
            node.updated_revision = revision;
            updated_directives.extend(candidate.expire_attempt_directives(&attempt_id));
        }
        candidate.refresh_parent_states(revision);
        candidate.refresh_terminal_phase();
        candidate.settle_draining_phase();
        let ready_node_ids = candidate.ready_node_ids_at(now_ms);
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanRecoveryOutput {
            snapshot,
            interrupted_attempts,
            updated_directives,
            ready_node_ids,
            previous_phase,
        })
    }

    fn interrupted_attempt_can_retry(
        &self,
        node_id: &PlanNodeId,
        attempt_id: &merry_core::PlanAttemptId,
    ) -> bool {
        let node = self
            .snapshot
            .nodes
            .iter()
            .find(|node| &node.id == node_id)
            .expect("attempt node remains present");
        let prior_recoveries = self
            .snapshot
            .attempts
            .iter()
            .filter(|attempt| {
                &attempt.node_id == node_id
                    && matches!(
                        attempt.outcome,
                        Some(
                            PlanAttemptOutcome::TransientFailure | PlanAttemptOutcome::Interrupted
                        )
                    )
            })
            .count();
        if prior_recoveries.saturating_add(1)
            >= node.recovery_policy.max_transient_attempts as usize
        {
            return false;
        }
        if !node
            .recovery_policy
            .retry_only_before_observable_side_effects
        {
            return true;
        }
        self.snapshot
            .attempt_progress
            .iter()
            .find(|progress| &progress.attempt_id == attempt_id)
            .is_none_or(|progress| {
                progress.observable_side_effects == 0
                    && progress.artifacts_created == 0
                    && progress.changed_paths.is_empty()
            })
    }
}

fn validation_reason(reason: &str) -> Result<(), PlanError> {
    super::validation::validate_reason(reason)
}
