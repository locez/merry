use super::{PlanError, PlanState, protocol::PlanApprovalInput, validation};
use merry_core::{
    PlanApprovalRequirementKind, PlanApprovalRequirementStatus, PlanLeaseStatus, PlanPhase,
    PlanSchedulerStatus, PlanSnapshot,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanControlOutput {
    pub(crate) snapshot: PlanSnapshot,
    pub(crate) previous_phase: PlanPhase,
}

impl PlanState {
    pub(crate) fn approve(
        &mut self,
        input: PlanApprovalInput,
    ) -> Result<PlanControlOutput, PlanError> {
        if self.snapshot.phase != PlanPhase::AwaitingApproval {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "approve plan",
            });
        }
        if self.snapshot.root_node_id.is_none() {
            return Err(PlanError::EmptyPlan);
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
            .leases
            .iter()
            .any(|lease| lease.status == PlanLeaseStatus::Live)
        {
            return Err(PlanError::LiveLeasesPreventControl {
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
        })
    }

    pub(crate) fn request_cancellation(
        &mut self,
        reason: &str,
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
        let has_live_leases = candidate
            .snapshot
            .leases
            .iter()
            .any(|lease| lease.status == PlanLeaseStatus::Live);
        candidate.snapshot.scheduler_status = PlanSchedulerStatus::Draining;
        if !has_live_leases {
            candidate.snapshot.phase = PlanPhase::Cancelled;
        }
        candidate.advance_revision("plan cancellation requested")?;
        let snapshot = candidate.snapshot.clone();
        *self = candidate;
        Ok(PlanControlOutput {
            snapshot,
            previous_phase,
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
