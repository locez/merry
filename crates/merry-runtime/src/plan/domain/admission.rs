use crate::plan::{
    domain::{PlanState, error::PlanError},
    protocol::PlanExecutionIntent,
    validation,
};
use merry_core::{
    PlanApprovalRequirementId, PlanApprovalRequirementKind, PlanApprovalRequirementSnapshot,
    PlanApprovalRequirementStatus, PlanCapabilityEnvelopeSnapshot, PlanPhase,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RootContract {
    pub(super) objective: String,
    pub(super) acceptance: Vec<String>,
}

impl PlanState {
    pub(super) fn apply_execution_intent(
        &mut self,
        intent: PlanExecutionIntent,
    ) -> Result<(), PlanError> {
        match intent {
            PlanExecutionIntent::ContinuePlanning => {
                if self.snapshot.phase == PlanPhase::Executing {
                    return Err(PlanError::WrongPhase {
                        actual: self.snapshot.phase,
                        operation: "continue planning",
                    });
                }
                self.snapshot.phase = PlanPhase::Planning;
            }
            PlanExecutionIntent::ExecuteIfAuthorized => {
                if self.snapshot.authorized_capability_envelope.is_none() {
                    self.snapshot.authorized_capability_envelope =
                        Some(self.current_plan_capability_envelope()?);
                } else if !self.authorized_envelope_covers_plan()? {
                    self.add_review_requirement(
                        PlanApprovalRequirementKind::CapabilityOrPermissionExpansion,
                    );
                }
                if self.has_pending_approval_requirements() {
                    self.snapshot.phase = PlanPhase::AwaitingApproval;
                } else {
                    self.snapshot.phase = PlanPhase::Executing;
                    self.snapshot.execution_contract_fingerprint =
                        Some(self.contract_fingerprint());
                }
            }
            PlanExecutionIntent::RequestUserReview => {
                if self.snapshot.authorized_capability_envelope.is_some()
                    && !self.authorized_envelope_covers_plan()?
                {
                    self.add_review_requirement(
                        PlanApprovalRequirementKind::CapabilityOrPermissionExpansion,
                    );
                }
                self.add_review_requirement(PlanApprovalRequirementKind::UserReviewRequested);
                self.snapshot.phase = PlanPhase::AwaitingApproval;
            }
        }
        Ok(())
    }

    pub(super) fn current_plan_capability_envelope(
        &self,
    ) -> Result<PlanCapabilityEnvelopeSnapshot, PlanError> {
        let root_id = self
            .snapshot
            .root_node_id
            .as_ref()
            .ok_or(PlanError::RootMissing)?;
        let root = self.node(root_id).ok_or(PlanError::RootMissing)?;
        Ok(PlanCapabilityEnvelopeSnapshot {
            allowed_tools: root.harness.allowed_tools.clone(),
            read_scope: root.harness.read_scope.clone(),
            write_scope: root.harness.write_scope.clone(),
            forbidden_paths: root.harness.forbidden_paths.clone(),
            destructive_external_authority: false,
        })
    }

    pub(super) fn add_review_requirement(&mut self, kind: PlanApprovalRequirementKind) {
        if self
            .snapshot
            .approval_requirements
            .iter()
            .any(|requirement| {
                requirement.status == PlanApprovalRequirementStatus::Pending
                    && requirement.kind == kind
            })
        {
            return;
        }
        let id =
            PlanApprovalRequirementId::new(&format!("approval-{}", self.next_approval_sequence))
                .expect("runtime-generated approval id is valid");
        self.next_approval_sequence += 1;
        self.snapshot
            .approval_requirements
            .push(PlanApprovalRequirementSnapshot {
                requirement_id: id,
                kind,
                status: PlanApprovalRequirementStatus::Pending,
                created_revision: self.snapshot.revision,
                resolution_ref: None,
            });
    }

    pub(super) fn root_contract(&self) -> Option<RootContract> {
        let root_id = self.snapshot.root_node_id.as_ref()?;
        let root = self
            .snapshot
            .nodes
            .iter()
            .find(|node| &node.id == root_id)?;
        Some(RootContract {
            objective: root.objective.clone(),
            acceptance: root.acceptance.clone(),
        })
    }

    pub(super) fn record_root_contract_changes(&mut self, established: Option<&RootContract>) {
        let Some(established) = established else {
            return;
        };
        let Some(current) = self.root_contract() else {
            return;
        };
        if current.objective != established.objective {
            self.add_review_requirement(PlanApprovalRequirementKind::RootObjectiveChange);
        }
        if current.acceptance != established.acceptance {
            self.add_review_requirement(PlanApprovalRequirementKind::RootAcceptanceChange);
        }
    }

    pub(super) fn authorized_envelope_covers_plan(&self) -> Result<bool, PlanError> {
        let Some(root_id) = self.snapshot.root_node_id.as_ref() else {
            return Err(PlanError::RootMissing);
        };
        let Some(envelope) = self.snapshot.authorized_capability_envelope.as_ref() else {
            return Ok(false);
        };
        let nodes = self
            .snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<BTreeMap<_, _>>();
        match validation::validate_authorized_envelope(&nodes, root_id, Some(envelope)) {
            Ok(()) => Ok(true),
            Err(PlanError::CapabilityEnvelopeExceeded { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(super) fn has_pending_approval_requirements(&self) -> bool {
        self.snapshot
            .approval_requirements
            .iter()
            .any(|requirement| requirement.status == PlanApprovalRequirementStatus::Pending)
    }
}
