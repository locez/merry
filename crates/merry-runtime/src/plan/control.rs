use super::{PlanError, PlanState, protocol::PlanApprovalInput, validation};
use merry_core::{
    ErrorInfo, PlanApprovalRequirementKind, PlanApprovalRequirementStatus, PlanAttemptOutcome,
    PlanAttemptSnapshot, PlanLeaseStatus, PlanNodeId, PlanNodeStatus, PlanPhase,
    PlanSchedulerStatus, PlanSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanControlOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) previous_phase: PlanPhase,
    pub(crate) finished_attempts: Vec<PlanAttemptSnapshot>,
}

impl PlanState {
    pub(crate) fn approve(
        &mut self,
        input: PlanApprovalInput,
    ) -> Result<PlanControlOutput, PlanError> {
        if !matches!(
            self.snapshot.phase,
            PlanPhase::Planning | PlanPhase::AwaitingApproval
        ) {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "approve plan",
            });
        }
        if self.snapshot.root_node_id.is_none() {
            return Err(PlanError::EmptyPlan);
        }
        if input.plan_id != self.snapshot.plan_id {
            return Err(PlanError::StalePlanIdentity {
                expected: input.plan_id,
                actual: self.snapshot.plan_id.clone(),
            });
        }
        if input.expected_plan_revision != self.snapshot.revision {
            return Err(PlanError::StalePlanRevision {
                expected: input.expected_plan_revision,
                actual: self.snapshot.revision,
            });
        }
        validation::validate_reason(&input.review_resolution_ref)?;
        for reference in &input.authorization_refs {
            validation::validate_reason(reference)?;
        }
        for reference in input.requirement_resolution_refs.values() {
            validation::validate_reason(reference)?;
        }

        let mut candidate = self.clone();
        let supplied_envelope = input.capability_envelope.is_some();
        let envelope = input
            .capability_envelope
            .or_else(|| candidate.snapshot.authorized_capability_envelope.clone())
            .ok_or_else(|| unresolved_requirement(&candidate))?;
        let root_id = candidate
            .snapshot
            .root_node_id
            .clone()
            .expect("checked root");
        let nodes = candidate
            .snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect();
        validation::validate_authorized_envelope(&nodes, &root_id, Some(&envelope))?;

        for requirement in &mut candidate.snapshot.approval_requirements {
            if requirement.status != PlanApprovalRequirementStatus::Pending {
                continue;
            }
            let resolution = match &requirement.kind {
                PlanApprovalRequirementKind::UserReviewRequested
                | PlanApprovalRequirementKind::SkillReviewRequested { .. }
                | PlanApprovalRequirementKind::RootObjectiveChange
                | PlanApprovalRequirementKind::RootAcceptanceChange => {
                    Some(input.review_resolution_ref.clone())
                }
                PlanApprovalRequirementKind::CapabilityOrPermissionExpansion => (supplied_envelope
                    && !input.authorization_refs.is_empty())
                .then(|| input.authorization_refs.join(",")),
                PlanApprovalRequirementKind::DestructiveExternalAuthority => (supplied_envelope
                    && envelope.destructive_external_authority
                    && !input.authorization_refs.is_empty())
                .then(|| input.authorization_refs.join(",")),
                PlanApprovalRequirementKind::RequiredExternalInput { .. } => input
                    .requirement_resolution_refs
                    .get(&requirement.requirement_id)
                    .cloned(),
            }
            .ok_or_else(|| PlanError::UnresolvedApprovalRequirement {
                requirement_id: requirement.requirement_id.clone(),
            })?;
            requirement.status = PlanApprovalRequirementStatus::Resolved;
            requirement.resolution_ref = Some(resolution);
        }
        candidate.snapshot.authorized_capability_envelope = Some(envelope);
        candidate.snapshot.execution_authorization_refs = input.authorization_refs;
        candidate.snapshot.phase = PlanPhase::Executing;
        candidate.snapshot.scheduler_status = PlanSchedulerStatus::Active;
        candidate.snapshot.execution_contract_fingerprint = Some(candidate.contract_fingerprint());
        let previous_phase = self.snapshot.phase;
        candidate.advance_revision("plan approval resolved")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanControlOutput {
            snapshot,
            previous_phase,
            finished_attempts: Vec::new(),
        })
    }

    pub(crate) fn pause_scheduling(
        &mut self,
        reason: &str,
    ) -> Result<PlanControlOutput, PlanError> {
        self.change_scheduler_status(
            reason,
            PlanSchedulerStatus::Active,
            PlanSchedulerStatus::Paused,
            "pause plan scheduling",
        )
    }

    pub(crate) fn resume_scheduling(
        &mut self,
        reason: &str,
    ) -> Result<PlanControlOutput, PlanError> {
        self.change_scheduler_status(
            reason,
            PlanSchedulerStatus::Paused,
            PlanSchedulerStatus::Active,
            "resume plan scheduling",
        )
    }

    pub(crate) fn revise(&mut self, reason: &str) -> Result<PlanControlOutput, PlanError> {
        validation::validate_reason(reason)?;
        if !matches!(
            self.snapshot.phase,
            PlanPhase::AwaitingApproval | PlanPhase::Executing
        ) {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "revise plan",
            });
        }
        if self
            .snapshot
            .attempts
            .iter()
            .any(|attempt| attempt.outcome.is_none())
        {
            return Err(PlanError::ActiveAttemptsPreventControl {
                operation: "plan revision",
            });
        }
        let mut candidate = self.clone();
        let previous_phase = candidate.snapshot.phase;
        for requirement in &mut candidate.snapshot.approval_requirements {
            if requirement.status == PlanApprovalRequirementStatus::Pending {
                requirement.status = PlanApprovalRequirementStatus::Rejected;
                requirement.resolution_ref = Some(reason.to_owned());
            }
        }
        candidate.snapshot.phase = PlanPhase::Planning;
        candidate.snapshot.scheduler_status = PlanSchedulerStatus::Paused;
        candidate.advance_revision("plan returned to revision")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanControlOutput {
            snapshot,
            previous_phase,
            finished_attempts: Vec::new(),
        })
    }

    pub(crate) fn request_cancellation(
        &mut self,
        reason: &str,
        now_ms: u64,
    ) -> Result<PlanControlOutput, PlanError> {
        validation::validate_reason(reason)?;
        if matches!(
            self.snapshot.phase,
            PlanPhase::Completed | PlanPhase::Blocked | PlanPhase::Cancelled
        ) {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "cancel plan",
            });
        }
        let mut candidate = self.clone();
        let previous_phase = candidate.snapshot.phase;
        let local_attempt_ids = candidate
            .snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.outcome.is_none() && attempt.lease_id.is_none())
            .map(|attempt| attempt.attempt_id.clone())
            .collect::<Vec<_>>();
        candidate.snapshot.scheduler_status = PlanSchedulerStatus::Draining;
        let revision = candidate.advance_revision("plan cancellation requested")?;
        let diagnostic = ErrorInfo::new("plan_attempt_cancelled", reason)
            .expect("validated cancellation reason produces a valid diagnostic");
        let mut finished_attempts = Vec::with_capacity(local_attempt_ids.len());
        for attempt_id in local_attempt_ids {
            let attempt_index = candidate
                .snapshot
                .attempts
                .iter()
                .position(|attempt| attempt.attempt_id == attempt_id)
                .expect("selected local attempt remains present");
            let node_id = candidate.snapshot.attempts[attempt_index].node_id.clone();
            let started_at_ms = candidate.snapshot.attempts[attempt_index].started_at_ms;
            {
                let attempt = &mut candidate.snapshot.attempts[attempt_index];
                attempt.finished_at_ms = Some(now_ms);
                attempt.outcome = Some(PlanAttemptOutcome::Cancelled);
                attempt.diagnostic = Some(diagnostic.clone());
            }
            if let Some(progress) = candidate
                .snapshot
                .attempt_progress
                .iter_mut()
                .find(|progress| progress.attempt_id == attempt_id)
            {
                progress.elapsed_ms = now_ms.saturating_sub(started_at_ms);
                progress.last_runtime_activity_at_ms = now_ms;
                progress.provider_request_in_flight = false;
                progress.tool_call_in_flight = false;
            }
            let node = candidate
                .snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == node_id)
                .expect("cancelled local attempt node remains present");
            node.status = PlanNodeStatus::Blocked;
            node.updated_revision = revision;
            finished_attempts.push(candidate.snapshot.attempts[attempt_index].clone());
        }
        candidate.refresh_parent_states(revision);
        candidate.settle_draining_phase();
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanControlOutput {
            snapshot,
            previous_phase,
            finished_attempts,
        })
    }

    pub(crate) fn retry_interrupted_node(
        &mut self,
        node_id: &PlanNodeId,
        reason: &str,
    ) -> Result<PlanControlOutput, PlanError> {
        super::validation::validate_reason(reason)?;
        if !matches!(
            self.snapshot.phase,
            PlanPhase::Executing | PlanPhase::Blocked
        ) || self.snapshot.scheduler_status == PlanSchedulerStatus::Draining
        {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "retry interrupted plan node",
            });
        }
        let node = self
            .snapshot
            .nodes
            .iter()
            .find(|node| &node.id == node_id)
            .ok_or_else(|| PlanError::UnknownNode {
                node_id: node_id.clone(),
            })?;
        let latest_attempt = self
            .snapshot
            .attempts
            .iter()
            .rev()
            .find(|attempt| &attempt.node_id == node_id);
        if node.status != PlanNodeStatus::Blocked
            || latest_attempt.and_then(|attempt| attempt.outcome)
                != Some(PlanAttemptOutcome::Interrupted)
        {
            return Err(PlanError::InterruptedRetryUnavailable {
                node_id: node_id.clone(),
            });
        }
        if self
            .snapshot
            .leases
            .iter()
            .any(|lease| &lease.node_id == node_id && lease.status == PlanLeaseStatus::Live)
        {
            return Err(PlanError::LiveLeaseExists {
                node_id: node_id.clone(),
            });
        }

        let mut candidate = self.clone();
        let previous_phase = candidate.snapshot.phase;
        let revision = candidate.advance_revision("interrupted plan node explicitly retried")?;
        let mut cursor = Some(node_id.clone());
        while let Some(current_id) = cursor {
            let current = candidate
                .snapshot
                .nodes
                .iter_mut()
                .find(|node| node.id == current_id)
                .expect("validated retry path remains present");
            cursor = current.parent_id.clone();
            current.status = if current.id == *node_id {
                PlanNodeStatus::Pending
            } else if current.status == PlanNodeStatus::Blocked {
                PlanNodeStatus::Expanded
            } else {
                current.status
            };
            current.updated_revision = revision;
        }
        candidate.refresh_parent_states(revision);
        candidate.snapshot.phase = PlanPhase::Executing;
        candidate.snapshot.scheduler_status = PlanSchedulerStatus::Active;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanControlOutput {
            snapshot,
            previous_phase,
            finished_attempts: Vec::new(),
        })
    }

    fn change_scheduler_status(
        &mut self,
        reason: &str,
        expected: PlanSchedulerStatus,
        target: PlanSchedulerStatus,
        operation: &'static str,
    ) -> Result<PlanControlOutput, PlanError> {
        validation::validate_reason(reason)?;
        if self.snapshot.phase != PlanPhase::Executing || self.snapshot.scheduler_status != expected
        {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation,
            });
        }
        let mut candidate = self.clone();
        let previous_phase = candidate.snapshot.phase;
        candidate.snapshot.scheduler_status = target;
        candidate.advance_revision(operation)?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanControlOutput {
            snapshot,
            previous_phase,
            finished_attempts: Vec::new(),
        })
    }
}

fn unresolved_requirement(plan: &PlanState) -> PlanError {
    let requirement_id = plan
        .snapshot
        .approval_requirements
        .iter()
        .find(|requirement| requirement.status == PlanApprovalRequirementStatus::Pending)
        .map(|requirement| requirement.requirement_id.clone())
        .unwrap_or_else(|| {
            merry_core::PlanApprovalRequirementId::new("approval-envelope")
                .expect("static approval id is valid")
        });
    PlanError::UnresolvedApprovalRequirement { requirement_id }
}
