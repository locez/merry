use super::{PlanError, PlanState, execution::PlanAttemptActor};
use merry_core::{
    CoordinatorDirectiveSnapshot, PlanAttemptId, PlanDirectiveId, PlanDirectiveStatus, PlanLeaseId,
    PlanLeaseStatus,
};
use std::collections::BTreeSet;

impl PlanState {
    pub(super) fn validate_live_lease(
        &self,
        actor: &PlanAttemptActor,
        lease_id: &PlanLeaseId,
        expected_node_revision: u64,
    ) -> Result<(usize, usize), PlanError> {
        let lease_index = self
            .snapshot
            .leases
            .iter()
            .position(|lease| &lease.lease_id == lease_id)
            .ok_or_else(|| PlanError::UnknownLease {
                lease_id: lease_id.clone(),
            })?;
        let lease = &self.snapshot.leases[lease_index];
        if lease.status != PlanLeaseStatus::Live {
            return Err(PlanError::LeaseNotLive {
                lease_id: lease_id.clone(),
            });
        }
        let attempt_index = self
            .snapshot
            .attempts
            .iter()
            .position(|attempt| attempt.attempt_id == lease.attempt_id)
            .ok_or_else(|| PlanError::UnknownAttempt {
                attempt_id: lease.attempt_id.clone(),
            })?;
        let attempt = &self.snapshot.attempts[attempt_index];
        if attempt.outcome.is_some() {
            return Err(PlanError::AttemptAlreadyResolved {
                attempt_id: attempt.attempt_id.clone(),
            });
        }
        if attempt.executor_session_id != actor.executor_session_id
            || lease.executor_session_id != actor.executor_session_id
        {
            return Err(PlanError::AttemptOwnershipMismatch {
                attempt_id: attempt.attempt_id.clone(),
            });
        }
        if lease.node_revision != expected_node_revision {
            return Err(PlanError::AttemptNodeRevisionMismatch {
                expected: expected_node_revision,
                actual: lease.node_revision,
            });
        }
        Ok((attempt_index, lease_index))
    }

    pub(super) fn validate_current_attempt(
        &self,
        actor: &PlanAttemptActor,
    ) -> Result<(usize, Option<usize>), PlanError> {
        let mut active = self
            .snapshot
            .attempts
            .iter()
            .enumerate()
            .filter(|(_, attempt)| {
                attempt.outcome.is_none()
                    && attempt.executor_session_id == actor.executor_session_id
            })
            .map(|(index, _)| index);
        let attempt_index = active
            .next()
            .ok_or_else(|| PlanError::NoActiveAttemptForExecutor {
                executor_session_id: actor.executor_session_id.clone(),
            })?;
        if active.next().is_some() {
            return Err(PlanError::MultipleActiveAttemptsForExecutor {
                executor_session_id: actor.executor_session_id.clone(),
            });
        }
        let attempt = &self.snapshot.attempts[attempt_index];
        let Some(lease_id) = attempt.lease_id.clone() else {
            return Ok((attempt_index, None));
        };
        let (validated_attempt_index, lease_index) =
            self.validate_live_lease(actor, &lease_id, attempt.node_revision)?;
        debug_assert_eq!(validated_attempt_index, attempt_index);
        Ok((attempt_index, Some(lease_index)))
    }

    pub(super) fn apply_directive_reports(
        &mut self,
        attempt_id: &PlanAttemptId,
        acknowledged: &[PlanDirectiveId],
        applied: &[PlanDirectiveId],
        now_ms: u64,
    ) -> Result<Vec<CoordinatorDirectiveSnapshot>, PlanError> {
        let acknowledged = acknowledged.iter().cloned().collect::<BTreeSet<_>>();
        let applied = applied.iter().cloned().collect::<BTreeSet<_>>();
        let mut updated = Vec::new();
        for directive_id in acknowledged.union(&applied) {
            let directive = self
                .snapshot
                .directives
                .iter_mut()
                .find(|directive| {
                    &directive.directive_id == directive_id && &directive.attempt_id == attempt_id
                })
                .ok_or_else(|| PlanError::UnknownDirective {
                    directive_id: directive_id.clone(),
                })?;
            if acknowledged.contains(directive_id)
                && matches!(
                    directive.status,
                    PlanDirectiveStatus::Queued | PlanDirectiveStatus::Delivered
                )
            {
                directive.status = PlanDirectiveStatus::Acknowledged;
                directive.acknowledged_at_ms = Some(now_ms);
            }
            if applied.contains(directive_id) {
                if directive.status == PlanDirectiveStatus::Applied {
                    continue;
                }
                if directive.status != PlanDirectiveStatus::Acknowledged {
                    return Err(PlanError::InvalidDirectiveTransition {
                        directive_id: directive_id.clone(),
                        status: directive.status,
                        target: "applied",
                    });
                }
                directive.status = PlanDirectiveStatus::Applied;
                directive.applied_at_ms = Some(now_ms);
            }
            updated.push(directive.clone());
        }
        if let Some(max_sequence) = self
            .snapshot
            .directives
            .iter()
            .filter(|directive| {
                &directive.attempt_id == attempt_id
                    && directive.status == PlanDirectiveStatus::Applied
            })
            .map(|directive| directive.sequence)
            .max()
            && let Some(attempt) = self
                .snapshot
                .attempts
                .iter_mut()
                .find(|attempt| &attempt.attempt_id == attempt_id)
        {
            attempt.last_applied_directive_sequence = max_sequence;
        }
        Ok(updated)
    }
}
