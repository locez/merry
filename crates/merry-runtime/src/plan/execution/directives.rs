use crate::plan::{
    PlanError, PlanState, execution::PlanAttemptActor, protocol::ControlPlanAttemptInput,
    validation,
};
use merry_core::{
    CoordinatorDirectiveSnapshot, PlanAttemptId, PlanDirectiveId, PlanDirectiveStatus, PlanLeaseId,
    PlanLeaseStatus, PlanSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanDirectiveOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) directive: CoordinatorDirectiveSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanDirectiveDeliveryOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) updated_directives: Vec<CoordinatorDirectiveSnapshot>,
}

impl PlanState {
    pub(crate) fn issue_directive(
        &mut self,
        input: ControlPlanAttemptInput,
        now_ms: u64,
    ) -> Result<PlanDirectiveOutput, PlanError> {
        validation::validate_reason(&input.reason)?;
        if let Some(instruction) = input.instruction.as_deref() {
            validation::validate_reason(instruction)?;
        }
        validation::validate_payload_items("requested_output", input.requested_output.len())?;
        for requested in &input.requested_output {
            validation::validate_payload_text("requested_output", requested)?;
        }

        let mut candidate = self.clone();
        let attempt = candidate
            .snapshot
            .attempts
            .iter()
            .find(|attempt| attempt.attempt_id == input.attempt_id)
            .ok_or_else(|| PlanError::UnknownAttempt {
                attempt_id: input.attempt_id.clone(),
            })?;
        if attempt.outcome.is_some() {
            return Err(PlanError::AttemptAlreadyResolved {
                attempt_id: input.attempt_id,
            });
        }
        let expected_lease_id = attempt
            .lease_id
            .as_ref()
            .ok_or(PlanError::StaleDirectiveTarget)?;
        let lease = candidate
            .snapshot
            .leases
            .iter()
            .find(|lease| &lease.lease_id == expected_lease_id)
            .ok_or(PlanError::StaleDirectiveTarget)?;
        if lease.status != PlanLeaseStatus::Live
            || lease.attempt_id != attempt.attempt_id
            || lease.node_revision != attempt.node_revision
        {
            return Err(PlanError::StaleDirectiveTarget);
        }
        let active_directive_count = candidate
            .snapshot
            .directives
            .iter()
            .filter(|directive| {
                directive.attempt_id == attempt.attempt_id
                    && !matches!(
                        directive.status,
                        PlanDirectiveStatus::Applied
                            | PlanDirectiveStatus::Superseded
                            | PlanDirectiveStatus::Expired
                    )
            })
            .count();
        if active_directive_count >= validation::MAX_DIRECTIVES_PER_ATTEMPT {
            return Err(PlanError::TooManyActiveDirectives {
                attempt_id: attempt.attempt_id.clone(),
                actual: active_directive_count + 1,
                maximum: validation::MAX_DIRECTIVES_PER_ATTEMPT,
            });
        }

        let directive_id = PlanDirectiveId::new(&format!(
            "plan-directive-{}",
            candidate.next_directive_sequence
        ))
        .expect("runtime-generated directive id is valid");
        let sequence = candidate.next_directive_sequence;
        candidate.next_directive_sequence += 1;
        let directive = CoordinatorDirectiveSnapshot {
            directive_id,
            sequence,
            plan_id: candidate.snapshot.plan_id.clone(),
            node_id: attempt.node_id.clone(),
            node_revision: attempt.node_revision,
            attempt_id: attempt.attempt_id.clone(),
            lease_id: lease.lease_id.clone(),
            kind: input.kind,
            reason: input.reason,
            instruction: input.instruction,
            constraints: input.constraints.unwrap_or_default(),
            requested_output: input.requested_output,
            issued_at_ms: now_ms,
            status: PlanDirectiveStatus::Queued,
            delivered_at_ms: None,
            acknowledged_at_ms: None,
            applied_at_ms: None,
        };
        candidate.snapshot.directives.push(directive.clone());
        candidate.advance_revision("coordinator directive issued")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanDirectiveOutput {
            snapshot,
            directive,
        })
    }

    pub(crate) fn deliver_queued_directives(
        &mut self,
        actor: &PlanAttemptActor,
        lease_id: &PlanLeaseId,
        now_ms: u64,
    ) -> Result<PlanDirectiveDeliveryOutput, PlanError> {
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
        let (attempt_index, _) =
            candidate.validate_live_lease(actor, lease_id, expected_node_revision)?;
        let attempt_id = candidate.snapshot.attempts[attempt_index]
            .attempt_id
            .clone();
        let mut updated_directives = Vec::new();
        for directive in candidate
            .snapshot
            .directives
            .iter_mut()
            .filter(|directive| {
                directive.attempt_id == attempt_id
                    && directive.status == PlanDirectiveStatus::Queued
            })
        {
            directive.status = PlanDirectiveStatus::Delivered;
            directive.delivered_at_ms = Some(now_ms);
            updated_directives.push(directive.clone());
        }
        if !updated_directives.is_empty() {
            candidate.advance_revision("coordinator directives delivered")?;
        }
        let snapshot = candidate.snapshot.clone();
        if !updated_directives.is_empty() {
            *self = candidate;
        }
        Ok(PlanDirectiveDeliveryOutput {
            snapshot,
            updated_directives,
        })
    }

    pub(in crate::plan) fn expire_attempt_directives(
        &mut self,
        attempt_id: &PlanAttemptId,
    ) -> Vec<CoordinatorDirectiveSnapshot> {
        let mut expired = Vec::new();
        for directive in self
            .snapshot
            .directives
            .iter_mut()
            .filter(|directive| &directive.attempt_id == attempt_id)
        {
            if !matches!(
                directive.status,
                PlanDirectiveStatus::Applied
                    | PlanDirectiveStatus::Superseded
                    | PlanDirectiveStatus::Expired
            ) {
                directive.status = PlanDirectiveStatus::Expired;
                expired.push(directive.clone());
            }
        }
        expired
    }
}
