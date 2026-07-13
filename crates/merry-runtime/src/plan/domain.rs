use super::{
    protocol::{
        PlanChangeInput, PlanExecutionIntent, PlanNodeInput, PlanUpdateOutput, UpdatePlanInput,
    },
    validation,
};
use merry_core::{
    PlanActivationSource, PlanApprovalRequirementId, PlanApprovalRequirementKind,
    PlanApprovalRequirementSnapshot, PlanApprovalRequirementStatus, PlanId, PlanNodeId,
    PlanNodeSnapshot, PlanNodeStatus, PlanPhase, PlanResourcePolicySnapshot, PlanRevisionSummary,
    PlanSchedulerStatus, PlanSnapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("plan update reason {reason}")]
    InvalidText {
        field: &'static str,
        reason: &'static str,
    },
    #[error("plan phase {actual:?} does not allow {operation}")]
    WrongPhase {
        actual: PlanPhase,
        operation: &'static str,
    },
    #[error("plan revision is stale: expected {expected}, actual {actual}")]
    StalePlanRevision { expected: u64, actual: u64 },
    #[error("node {node_id} revision is stale: expected {expected}, actual {actual}")]
    StaleNodeRevision {
        node_id: PlanNodeId,
        expected: u64,
        actual: u64,
    },
    #[error("plan must contain exactly one root")]
    RootMissing,
    #[error("plan root must not have a parent")]
    RootHasParent,
    #[error("plan node {node_id} is missing a parent")]
    NodeMissingParent { node_id: PlanNodeId },
    #[error("plan node {node_id} references unknown parent {parent_id}")]
    UnknownParent {
        node_id: PlanNodeId,
        parent_id: PlanNodeId,
    },
    #[error("plan node {node_id} is not reachable from the root")]
    UnreachableNode { node_id: PlanNodeId },
    #[error("plan parent graph contains a cycle")]
    ParentCycle,
    #[error("plan dependency graph contains a cycle")]
    DependencyCycle,
    #[error("plan node {node_id} depends on itself")]
    SelfDependency { node_id: PlanNodeId },
    #[error("plan node {node_id} depends on descendant {dependency_id}")]
    DependsOnDescendant {
        node_id: PlanNodeId,
        dependency_id: PlanNodeId,
    },
    #[error("unknown plan node {node_id}")]
    UnknownNode { node_id: PlanNodeId },
    #[error("unknown dependency node {node_id}")]
    UnknownDependency { node_id: PlanNodeId },
    #[error("unknown request-local client key {client_key}")]
    UnknownClientKey { client_key: String },
    #[error("duplicate request-local client key {client_key}")]
    DuplicateClientKey { client_key: String },
    #[error("duplicate plan node id {node_id}")]
    DuplicateNodeId { node_id: PlanNodeId },
    #[error("new plan node must provide exactly one client_key and no id")]
    InvalidNewNodeIdentity,
    #[error("existing plan node must provide an id and no client_key")]
    InvalidExistingNodeIdentity,
    #[error("plan has {actual} live nodes, maximum is {maximum}")]
    TooManyNodes { actual: usize, maximum: usize },
    #[error("plan node {node_id} has {actual} children, maximum is {maximum}")]
    TooManyChildren {
        node_id: PlanNodeId,
        actual: usize,
        maximum: usize,
    },
    #[error("plan depth {actual} exceeds maximum {maximum}")]
    PlanTooDeep { actual: usize, maximum: usize },
    #[error("node has {actual} dependencies, maximum is {maximum}")]
    TooManyDependencies { actual: usize, maximum: usize },
    #[error("node has {actual} acceptance items, maximum is {maximum}")]
    TooManyAcceptanceItems { actual: usize, maximum: usize },
    #[error("duplicate sibling order {sibling_order} below parent {parent_id:?}")]
    DuplicateSiblingOrder {
        parent_id: Option<PlanNodeId>,
        sibling_order: u16,
    },
    #[error("node {node_id} scope path is invalid: {path}")]
    InvalidScopePath { node_id: PlanNodeId, path: String },
    #[error("node {node_id} exceeds its parent or authorized capability envelope")]
    CapabilityEnvelopeExceeded { node_id: PlanNodeId },
    #[error("node {node_id} is not mutable while in status {status:?}")]
    NodeNotMutable {
        node_id: PlanNodeId,
        status: PlanNodeStatus,
    },
    #[error("replacement root must retain target node id {target_node_id}")]
    ReplacementRootIdentity { target_node_id: PlanNodeId },
    #[error("subtree replacement would leave incoming dependency on superseded node {node_id}")]
    IncomingDependencyWouldDangle { node_id: PlanNodeId },
    #[error("max_concurrency_hint must be between one and runtime maximum {maximum}")]
    InvalidConcurrencyHint { maximum: usize },
    #[error("persisted plan id counters must be non-zero")]
    InvalidPersistedCounters,
    #[error("plan has no root node")]
    EmptyPlan,
    #[error("plan approval requirement {requirement_id} has no valid runtime resolution")]
    UnresolvedApprovalRequirement {
        requirement_id: merry_core::PlanApprovalRequirementId,
    },
    #[error("live plan leases prevent {operation}")]
    LiveLeasesPreventControl { operation: &'static str },
    #[error("node {node_id} is not ready for execution")]
    NodeNotReady { node_id: PlanNodeId },
    #[error("node {node_id} already has a live lease")]
    LiveLeaseExists { node_id: PlanNodeId },
    #[error("plan lease {lease_id} was not found")]
    UnknownLease { lease_id: merry_core::PlanLeaseId },
    #[error("plan lease {lease_id} is not live")]
    LeaseNotLive { lease_id: merry_core::PlanLeaseId },
    #[error("plan attempt {attempt_id} was not found")]
    UnknownAttempt {
        attempt_id: merry_core::PlanAttemptId,
    },
    #[error("plan attempt {attempt_id} belongs to another executor session")]
    AttemptOwnershipMismatch {
        attempt_id: merry_core::PlanAttemptId,
    },
    #[error("plan attempt {attempt_id} is already resolved")]
    AttemptAlreadyResolved {
        attempt_id: merry_core::PlanAttemptId,
    },
    #[error("attempt lease node revision is stale: expected {expected}, actual {actual}")]
    AttemptNodeRevisionMismatch { expected: u64, actual: u64 },
    #[error("attempt outcome {outcome:?} has an invalid result/decomposition contract")]
    InvalidAttemptOutcome {
        outcome: merry_core::PlanAttemptOutcome,
    },
    #[error("attempt decomposition must contain at least one direct child")]
    EmptyDecomposition,
    #[error("attempt decomposition children must be direct leaves")]
    NestedDecomposition,
    #[error("directive {directive_id} was not found for the current attempt")]
    UnknownDirective {
        directive_id: merry_core::PlanDirectiveId,
    },
    #[error("directive {directive_id} cannot transition from {status:?} to {target}")]
    InvalidDirectiveTransition {
        directive_id: merry_core::PlanDirectiveId,
        status: merry_core::PlanDirectiveStatus,
        target: &'static str,
    },
    #[error("directive target attempt or lease is stale")]
    StaleDirectiveTarget,
    #[error("plan result references missing artifact {artifact_id}")]
    MissingArtifactRef { artifact_id: merry_core::ArtifactId },
    #[error("plan result references invalid evidence in artifact {artifact_id}")]
    InvalidEvidenceRef { artifact_id: merry_core::ArtifactId },
}

#[derive(Debug, Clone)]
pub(crate) struct PlanState {
    pub(super) snapshot: PlanSnapshot,
    pub(super) next_node_sequence: u64,
    pub(super) next_approval_sequence: u64,
    pub(super) next_attempt_sequence: u64,
    pub(super) next_lease_sequence: u64,
    pub(super) next_directive_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedPlanState {
    snapshot: PlanSnapshot,
    next_node_sequence: u64,
    next_approval_sequence: u64,
    #[serde(default = "one")]
    next_attempt_sequence: u64,
    #[serde(default = "one")]
    next_lease_sequence: u64,
    #[serde(default = "one")]
    next_directive_sequence: u64,
}

const fn one() -> u64 {
    1
}

impl PlanState {
    pub(crate) fn empty(
        plan_id: PlanId,
        activation_source: PlanActivationSource,
        resource_policy_snapshot: PlanResourcePolicySnapshot,
    ) -> Self {
        Self {
            snapshot: PlanSnapshot {
                plan_id,
                revision: 0,
                phase: PlanPhase::Planning,
                activation_source,
                root_node_id: None,
                coordinator_node_id: None,
                execution_contract_fingerprint: None,
                execution_authorization_refs: Vec::new(),
                authorized_capability_envelope: None,
                approval_requirements: Vec::new(),
                nodes: Vec::new(),
                attempts: Vec::new(),
                leases: Vec::new(),
                attempt_progress: Vec::new(),
                directives: Vec::new(),
                resource_policy_snapshot,
                max_concurrency_hint: None,
                scheduler_status: PlanSchedulerStatus::Active,
                revision_summaries: Vec::new(),
            },
            next_node_sequence: 1,
            next_approval_sequence: 1,
            next_attempt_sequence: 1,
            next_lease_sequence: 1,
            next_directive_sequence: 1,
        }
    }

    pub(crate) fn snapshot(&self) -> &PlanSnapshot {
        &self.snapshot
    }

    pub(crate) fn node(&self, node_id: &PlanNodeId) -> Option<&PlanNodeSnapshot> {
        self.snapshot.nodes.iter().find(|node| &node.id == node_id)
    }

    pub(super) fn add_decomposition_children(
        &mut self,
        parent_id: &PlanNodeId,
        children: Vec<PlanNodeInput>,
        revision: u64,
    ) -> Result<BTreeMap<String, PlanNodeId>, PlanError> {
        if children.is_empty() {
            return Err(PlanError::EmptyDecomposition);
        }
        if children.len() > validation::MAX_DIRECT_CHILDREN {
            return Err(PlanError::TooManyChildren {
                node_id: parent_id.clone(),
                actual: children.len(),
                maximum: validation::MAX_DIRECT_CHILDREN,
            });
        }
        let existing = self
            .snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<BTreeMap<_, _>>();
        let parent = existing
            .get(parent_id)
            .ok_or_else(|| PlanError::UnknownNode {
                node_id: parent_id.clone(),
            })?;
        if parent.status != PlanNodeStatus::InProgress {
            return Err(PlanError::NodeNotMutable {
                node_id: parent_id.clone(),
                status: parent.status,
            });
        }
        let existing_child_count = existing
            .values()
            .filter(|node| {
                node.parent_id.as_ref() == Some(parent_id)
                    && node.status != PlanNodeStatus::Superseded
            })
            .count();
        let mut builder = TreeBuilder::new(self.next_node_sequence, revision, &existing);
        for (offset, child) in children.into_iter().enumerate() {
            if !child.children.is_empty() {
                return Err(PlanError::NestedDecomposition);
            }
            builder.flatten(
                child,
                Some(parent_id.clone()),
                (existing_child_count + offset) as u16,
                depth_of(&existing, parent_id) + 1,
            )?;
        }
        let (new_nodes, client_key_ids, next_node_sequence, unresolved) = builder.finish();
        let mut combined = existing;
        combined.extend(new_nodes);
        let live_ids = combined
            .values()
            .filter(|node| node.status != PlanNodeStatus::Superseded)
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        resolve_all_dependencies(&mut combined, unresolved, &client_key_ids, &live_ids)?;
        let root_id = self
            .snapshot
            .root_node_id
            .as_ref()
            .ok_or(PlanError::RootMissing)?;
        validation::validate_graph(&combined, root_id)?;
        validation::validate_authorized_envelope(
            &combined,
            root_id,
            self.snapshot.authorized_capability_envelope.as_ref(),
        )?;
        self.snapshot.nodes = ordered_nodes(combined);
        self.next_node_sequence = next_node_sequence;
        Ok(client_key_ids)
    }

    pub(crate) fn persisted(&self) -> PersistedPlanState {
        PersistedPlanState {
            snapshot: self.snapshot.clone(),
            next_node_sequence: self.next_node_sequence,
            next_approval_sequence: self.next_approval_sequence,
            next_attempt_sequence: self.next_attempt_sequence,
            next_lease_sequence: self.next_lease_sequence,
            next_directive_sequence: self.next_directive_sequence,
        }
    }

    pub(crate) fn from_persisted(persisted: PersistedPlanState) -> Result<Self, PlanError> {
        if persisted.next_node_sequence == 0
            || persisted.next_approval_sequence == 0
            || persisted.next_attempt_sequence == 0
            || persisted.next_lease_sequence == 0
            || persisted.next_directive_sequence == 0
        {
            return Err(PlanError::InvalidPersistedCounters);
        }
        match persisted.snapshot.root_node_id.as_ref() {
            Some(root_id) => {
                let nodes = persisted
                    .snapshot
                    .nodes
                    .iter()
                    .map(|node| (node.id.clone(), node.clone()))
                    .collect::<BTreeMap<_, _>>();
                validation::validate_graph(&nodes, root_id)?;
                validation::validate_authorized_envelope(
                    &nodes,
                    root_id,
                    persisted.snapshot.authorized_capability_envelope.as_ref(),
                )?;
            }
            None if persisted.snapshot.phase == PlanPhase::Planning
                && persisted.snapshot.nodes.is_empty() => {}
            None => return Err(PlanError::RootMissing),
        }
        Ok(Self {
            snapshot: persisted.snapshot,
            next_node_sequence: persisted.next_node_sequence,
            next_approval_sequence: persisted.next_approval_sequence,
            next_attempt_sequence: persisted.next_attempt_sequence,
            next_lease_sequence: persisted.next_lease_sequence,
            next_directive_sequence: persisted.next_directive_sequence,
        })
    }

    pub(crate) fn update(&mut self, input: UpdatePlanInput) -> Result<PlanUpdateOutput, PlanError> {
        validation::validate_reason(&input.reason)?;
        let maximum = self.snapshot.resource_policy_snapshot.max_concurrency;
        if input
            .max_concurrency_hint
            .is_some_and(|hint| hint == 0 || hint > maximum)
        {
            return Err(PlanError::InvalidConcurrencyHint { maximum });
        }
        let mut candidate = self.clone();
        let client_key_ids = match input.change {
            PlanChangeInput::DefinePlan {
                expected_plan_revision,
                root,
            } => candidate.define_plan(expected_plan_revision, root)?,
            PlanChangeInput::ReplaceSubtree {
                target_node_id,
                expected_node_revision,
                subtree,
            } => candidate.replace_subtree(&target_node_id, expected_node_revision, subtree)?,
        };
        candidate.snapshot.coordinator_node_id = input.coordinator_node_id;
        candidate.snapshot.max_concurrency_hint = input.max_concurrency_hint;
        candidate.apply_execution_intent(input.execution_intent)?;
        candidate.snapshot.revision_summaries.push(
            PlanRevisionSummary::new(candidate.snapshot.revision, &input.reason).map_err(|_| {
                PlanError::InvalidText {
                    field: "reason",
                    reason: "is invalid",
                }
            })?,
        );
        if candidate.snapshot.revision_summaries.len() > 32 {
            candidate.snapshot.revision_summaries.remove(0);
        }
        *self = candidate;
        Ok(PlanUpdateOutput {
            snapshot: self.snapshot.clone(),
            client_key_ids,
        })
    }

    fn define_plan(
        &mut self,
        expected_revision: u64,
        root: PlanNodeInput,
    ) -> Result<BTreeMap<String, PlanNodeId>, PlanError> {
        if self.snapshot.phase != PlanPhase::Planning {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "define plan",
            });
        }
        if expected_revision != self.snapshot.revision {
            return Err(PlanError::StalePlanRevision {
                expected: expected_revision,
                actual: self.snapshot.revision,
            });
        }
        let revision = self.snapshot.revision + 1;
        let existing = self
            .snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut builder = TreeBuilder::new(self.next_node_sequence, revision, &existing);
        let root_id = builder.flatten(root, None, 0, 1)?;
        let (mut nodes, client_key_ids, next_node_sequence, unresolved) = builder.finish();
        let live_ids = nodes.keys().cloned().collect::<BTreeSet<_>>();
        resolve_all_dependencies(&mut nodes, unresolved, &client_key_ids, &live_ids)?;
        for old in existing.values() {
            if !nodes.contains_key(&old.id) && old.status != PlanNodeStatus::Superseded {
                ensure_mutable(old)?;
                let mut superseded = old.clone();
                superseded.status = PlanNodeStatus::Superseded;
                superseded.updated_revision = revision;
                nodes.insert(superseded.id.clone(), superseded);
            }
        }
        validation::validate_graph(&nodes, &root_id)?;
        validation::validate_authorized_envelope(
            &nodes,
            &root_id,
            self.snapshot.authorized_capability_envelope.as_ref(),
        )?;
        self.snapshot.revision = revision;
        self.snapshot.root_node_id = Some(root_id);
        self.snapshot.nodes = ordered_nodes(nodes);
        self.next_node_sequence = next_node_sequence;
        Ok(client_key_ids)
    }

    fn replace_subtree(
        &mut self,
        target_node_id: &PlanNodeId,
        expected_node_revision: u64,
        subtree: PlanNodeInput,
    ) -> Result<BTreeMap<String, PlanNodeId>, PlanError> {
        if !matches!(
            self.snapshot.phase,
            PlanPhase::Planning | PlanPhase::Executing
        ) {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "replace subtree",
            });
        }
        let target = self
            .node(target_node_id)
            .cloned()
            .ok_or_else(|| PlanError::UnknownNode {
                node_id: target_node_id.clone(),
            })?;
        ensure_mutable(&target)?;
        if target.updated_revision != expected_node_revision {
            return Err(PlanError::StaleNodeRevision {
                node_id: target_node_id.clone(),
                expected: expected_node_revision,
                actual: target.updated_revision,
            });
        }
        if subtree.id.as_ref() != Some(target_node_id) || subtree.client_key.is_some() {
            return Err(PlanError::ReplacementRootIdentity {
                target_node_id: target_node_id.clone(),
            });
        }

        let revision = self.snapshot.revision + 1;
        let existing = self
            .snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<BTreeMap<_, _>>();
        let old_region = subtree_ids(&existing, target_node_id);
        for id in &old_region {
            let node = existing.get(id).expect("subtree id came from existing map");
            ensure_mutable(node)?;
            if node.result.is_some() {
                return Err(PlanError::NodeNotMutable {
                    node_id: node.id.clone(),
                    status: node.status,
                });
            }
        }

        let mut builder = TreeBuilder::new(self.next_node_sequence, revision, &existing);
        let replacement_root = builder.flatten(
            subtree,
            target.parent_id.clone(),
            target.sibling_order,
            depth_of(&existing, target_node_id),
        )?;
        debug_assert_eq!(&replacement_root, target_node_id);
        let (replacement, client_key_ids, next_node_sequence, unresolved) = builder.finish();
        let replacement_ids = replacement.keys().cloned().collect::<BTreeSet<_>>();
        let omitted = old_region
            .difference(&replacement_ids)
            .cloned()
            .collect::<BTreeSet<_>>();
        for node in existing
            .values()
            .filter(|node| !old_region.contains(&node.id))
        {
            if let Some(dependency) = node.depends_on.iter().find(|id| omitted.contains(*id)) {
                return Err(PlanError::IncomingDependencyWouldDangle {
                    node_id: dependency.clone(),
                });
            }
        }

        let mut combined = existing.clone();
        for id in &old_region {
            combined.remove(id);
        }
        combined.extend(replacement);
        for id in omitted {
            let mut superseded = existing
                .get(&id)
                .expect("omitted id came from existing map")
                .clone();
            superseded.status = PlanNodeStatus::Superseded;
            superseded.updated_revision = revision;
            combined.insert(id, superseded);
        }
        let live_ids = combined
            .values()
            .filter(|node| node.status != PlanNodeStatus::Superseded)
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        resolve_all_dependencies(&mut combined, unresolved, &client_key_ids, &live_ids)?;
        let root_id = self
            .snapshot
            .root_node_id
            .clone()
            .ok_or(PlanError::RootMissing)?;
        validation::validate_graph(&combined, &root_id)?;
        validation::validate_authorized_envelope(
            &combined,
            &root_id,
            self.snapshot.authorized_capability_envelope.as_ref(),
        )?;
        self.snapshot.revision = revision;
        self.snapshot.nodes = ordered_nodes(combined);
        self.next_node_sequence = next_node_sequence;
        Ok(client_key_ids)
    }

    fn apply_execution_intent(&mut self, intent: PlanExecutionIntent) -> Result<(), PlanError> {
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
                if self.snapshot.authorized_capability_envelope.is_some() {
                    self.snapshot.phase = PlanPhase::Executing;
                    self.snapshot.execution_contract_fingerprint =
                        Some(format!("plan-contract-{}", self.snapshot.revision));
                } else {
                    self.add_review_requirement(
                        PlanApprovalRequirementKind::CapabilityOrPermissionExpansion,
                    );
                    self.snapshot.phase = PlanPhase::AwaitingApproval;
                }
            }
            PlanExecutionIntent::RequestUserReview => {
                self.add_review_requirement(PlanApprovalRequirementKind::UserReviewRequested);
                self.snapshot.phase = PlanPhase::AwaitingApproval;
            }
        }
        Ok(())
    }

    fn add_review_requirement(&mut self, kind: PlanApprovalRequirementKind) {
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

    #[cfg(test)]
    pub(crate) fn advance_unrelated_revision_for_test(&mut self) {
        self.snapshot.revision += 1;
    }
}

struct TreeBuilder<'a> {
    next_node_sequence: u64,
    revision: u64,
    existing: &'a BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    nodes: BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    client_key_ids: BTreeMap<String, PlanNodeId>,
    unresolved: BTreeMap<PlanNodeId, Vec<super::protocol::PlanNodeReferenceInput>>,
}

impl<'a> TreeBuilder<'a> {
    fn new(
        next_node_sequence: u64,
        revision: u64,
        existing: &'a BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    ) -> Self {
        Self {
            next_node_sequence,
            revision,
            existing,
            nodes: BTreeMap::new(),
            client_key_ids: BTreeMap::new(),
            unresolved: BTreeMap::new(),
        }
    }

    fn flatten(
        &mut self,
        input: PlanNodeInput,
        parent_id: Option<PlanNodeId>,
        sibling_order: u16,
        depth: usize,
    ) -> Result<PlanNodeId, PlanError> {
        if depth > validation::MAX_PLAN_DEPTH {
            return Err(PlanError::PlanTooDeep {
                actual: depth,
                maximum: validation::MAX_PLAN_DEPTH,
            });
        }
        validation::validate_node_text(&input.objective, &input.acceptance)?;
        if input.children.len() > validation::MAX_DIRECT_CHILDREN {
            return Err(PlanError::TooManyChildren {
                node_id: input
                    .id
                    .clone()
                    .unwrap_or_else(|| PlanNodeId::new("unassigned").expect("valid sentinel")),
                actual: input.children.len(),
                maximum: validation::MAX_DIRECT_CHILDREN,
            });
        }
        let (id, created_revision) = match (input.id, input.client_key) {
            (None, Some(client_key)) => {
                validation::validate_client_key(&client_key)?;
                if self.client_key_ids.contains_key(&client_key) {
                    return Err(PlanError::DuplicateClientKey { client_key });
                }
                let id = PlanNodeId::new(&format!("plan-node-{}", self.next_node_sequence))
                    .expect("runtime-generated node id is valid");
                self.next_node_sequence += 1;
                self.client_key_ids.insert(client_key, id.clone());
                (id, self.revision)
            }
            (Some(id), None) => {
                let existing = self
                    .existing
                    .get(&id)
                    .ok_or_else(|| PlanError::UnknownNode {
                        node_id: id.clone(),
                    })?;
                ensure_mutable(existing)?;
                (id, existing.created_revision)
            }
            (None, None) => return Err(PlanError::InvalidNewNodeIdentity),
            (Some(_), Some(_)) => return Err(PlanError::InvalidExistingNodeIdentity),
        };
        if self.nodes.contains_key(&id) {
            return Err(PlanError::DuplicateNodeId { node_id: id });
        }
        let dependencies = input.depends_on;
        let children = input.children;
        let node = PlanNodeSnapshot {
            id: id.clone(),
            parent_id,
            sibling_order,
            objective: input.objective,
            acceptance: input.acceptance,
            status: PlanNodeStatus::Pending,
            executor_policy: input.executor_policy,
            harness: input.harness,
            recovery_policy: input.recovery_policy,
            depends_on: Vec::new(),
            result: None,
            created_revision,
            updated_revision: self.revision,
        };
        self.nodes.insert(id.clone(), node);
        self.unresolved.insert(id.clone(), dependencies);
        for (order, child) in children.into_iter().enumerate() {
            self.flatten(child, Some(id.clone()), order as u16, depth + 1)?;
        }
        Ok(id)
    }

    fn finish(
        self,
    ) -> (
        BTreeMap<PlanNodeId, PlanNodeSnapshot>,
        BTreeMap<String, PlanNodeId>,
        u64,
        BTreeMap<PlanNodeId, Vec<super::protocol::PlanNodeReferenceInput>>,
    ) {
        (
            self.nodes,
            self.client_key_ids,
            self.next_node_sequence,
            self.unresolved,
        )
    }
}

fn resolve_all_dependencies(
    nodes: &mut BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    unresolved: BTreeMap<PlanNodeId, Vec<super::protocol::PlanNodeReferenceInput>>,
    client_key_ids: &BTreeMap<String, PlanNodeId>,
    live_ids: &BTreeSet<PlanNodeId>,
) -> Result<(), PlanError> {
    for (id, references) in unresolved {
        let resolved = validation::resolve_dependencies(&references, client_key_ids, live_ids)?;
        nodes
            .get_mut(&id)
            .expect("unresolved entries belong to candidate nodes")
            .depends_on = resolved;
    }
    Ok(())
}

fn ensure_mutable(node: &PlanNodeSnapshot) -> Result<(), PlanError> {
    if node.status != PlanNodeStatus::Pending {
        return Err(PlanError::NodeNotMutable {
            node_id: node.id.clone(),
            status: node.status,
        });
    }
    Ok(())
}

fn ordered_nodes(nodes: BTreeMap<PlanNodeId, PlanNodeSnapshot>) -> Vec<PlanNodeSnapshot> {
    let mut nodes = nodes.into_values().collect::<Vec<_>>();
    nodes.sort_by_key(|node| {
        (
            node.status == PlanNodeStatus::Superseded,
            node.parent_id.clone(),
            node.sibling_order,
            node.id.clone(),
        )
    });
    nodes
}

fn subtree_ids(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    target: &PlanNodeId,
) -> BTreeSet<PlanNodeId> {
    let mut ids = BTreeSet::from([target.clone()]);
    loop {
        let before = ids.len();
        for node in nodes.values() {
            if node
                .parent_id
                .as_ref()
                .is_some_and(|parent| ids.contains(parent))
            {
                ids.insert(node.id.clone());
            }
        }
        if ids.len() == before {
            return ids;
        }
    }
}

fn depth_of(nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>, node_id: &PlanNodeId) -> usize {
    let mut depth = 1;
    let mut cursor = nodes.get(node_id).and_then(|node| node.parent_id.as_ref());
    while let Some(parent) = cursor {
        depth += 1;
        cursor = nodes.get(parent).and_then(|node| node.parent_id.as_ref());
    }
    depth
}
