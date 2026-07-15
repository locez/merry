use super::{
    protocol::{
        PlanChangeInput, PlanExecutionIntent, PlanNodeInput, PlanNodeReferenceInput,
        PlanUpdateOutput, SubagentPlanChangeInput, SubagentPlanUpdateInput, UpdatePlanInput,
    },
    validation,
};
use merry_core::{
    PlanActivationSource, PlanApprovalRequirementId, PlanApprovalRequirementKind,
    PlanApprovalRequirementSnapshot, PlanApprovalRequirementStatus, PlanBindingId,
    PlanCapabilityEnvelopeSnapshot, PlanId, PlanLinkStatus, PlanNodeId, PlanNodeSnapshot,
    PlanNodeStatus, PlanPhase, PlanResourcePolicySnapshot, PlanRevisionSummary,
    PlanSchedulerStatus, PlanSnapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("authored node status {status:?} is runtime-owned")]
    InvalidAuthoredNodeStatus { status: PlanNodeStatus },
    #[error("subagent scope violation: {reason}")]
    SubagentScopeViolation { reason: &'static str },
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
    #[error("plan identity is stale: expected {expected}, actual {actual}")]
    StalePlanIdentity { expected: PlanId, actual: PlanId },
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
    #[error("active plan attempts prevent {operation}")]
    ActiveAttemptsPreventControl { operation: &'static str },
    #[error("node {node_id} is not ready for execution")]
    NodeNotReady { node_id: PlanNodeId },
    #[error("node {node_id} already has a live lease")]
    LiveLeaseExists { node_id: PlanNodeId },
    #[error("node {node_id} has no blocked interrupted attempt eligible for explicit retry")]
    InterruptedRetryUnavailable { node_id: PlanNodeId },
    #[error("plan lease {lease_id} was not found")]
    UnknownLease { lease_id: merry_core::PlanLeaseId },
    #[error("plan lease {lease_id} is not live")]
    LeaseNotLive { lease_id: merry_core::PlanLeaseId },
    #[error("plan attempt {attempt_id} was not found")]
    UnknownAttempt {
        attempt_id: merry_core::PlanAttemptId,
    },
    #[error("executor session {executor_session_id} has no active plan attempt")]
    NoActiveAttemptForExecutor {
        executor_session_id: merry_core::SessionId,
    },
    #[error("executor session {executor_session_id} has multiple active plan attempts")]
    MultipleActiveAttemptsForExecutor {
        executor_session_id: merry_core::SessionId,
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
    #[error("promoted plan artifact {artifact_id} conflicts with existing root-session content")]
    ArtifactPromotionConflict { artifact_id: merry_core::ArtifactId },
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootContract {
    objective: String,
    acceptance: Vec<String>,
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
        let TreeBuilderOutput {
            nodes: new_nodes,
            client_key_ids,
            next_node_sequence,
            unresolved,
        } = builder.finish();
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
                if matches!(
                    persisted.snapshot.phase,
                    PlanPhase::Executing
                        | PlanPhase::Completed
                        | PlanPhase::Blocked
                        | PlanPhase::Cancelled
                ) {
                    validation::validate_authorized_envelope(
                        &nodes,
                        root_id,
                        persisted.snapshot.authorized_capability_envelope.as_ref(),
                    )?;
                }
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
        let established_root_contract = self
            .snapshot
            .execution_contract_fingerprint
            .as_ref()
            .and_then(|_| self.root_contract());
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
            PlanChangeInput::UseCurrentPlan {
                expected_plan_revision,
            } => candidate.use_current_plan(expected_plan_revision)?,
        };
        candidate.snapshot.coordinator_node_id = input.coordinator_node_id;
        candidate.snapshot.max_concurrency_hint = input.max_concurrency_hint;
        candidate.record_root_contract_changes(established_root_contract.as_ref());
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

    pub(crate) fn update_subagent(
        &mut self,
        plan_id: PlanId,
        root_node_id: PlanNodeId,
        binding_id: PlanBindingId,
        input: SubagentPlanUpdateInput,
    ) -> Result<PlanUpdateOutput, PlanError> {
        validation::validate_reason(&input.reason)?;
        let mut candidate = self.clone();
        candidate.validate_subagent_scope(&plan_id, &root_node_id, &binding_id)?;
        let client_key_ids = match input.change {
            SubagentPlanChangeInput::DefineChildren {
                expected_plan_revision,
                children,
            } => {
                candidate.define_scoped_children(&root_node_id, expected_plan_revision, children)?
            }
            SubagentPlanChangeInput::ReplaceSubtree {
                target_node_id,
                expected_node_revision,
                subtree,
            } => candidate.replace_scoped_subtree(
                &root_node_id,
                &target_node_id,
                expected_node_revision,
                subtree,
            )?,
        };
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

    pub(crate) fn define_scoped_children(
        &mut self,
        root_node_id: &PlanNodeId,
        expected_plan_revision: u64,
        children: Vec<PlanNodeInput>,
    ) -> Result<BTreeMap<String, PlanNodeId>, PlanError> {
        if !matches!(
            self.snapshot.phase,
            PlanPhase::Planning | PlanPhase::Executing
        ) {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "define scoped children",
            });
        }
        if expected_plan_revision != self.snapshot.revision {
            return Err(PlanError::StalePlanRevision {
                expected: expected_plan_revision,
                actual: self.snapshot.revision,
            });
        }
        if children.is_empty() {
            return Err(PlanError::EmptyDecomposition);
        }
        if children.len() > validation::MAX_DIRECT_CHILDREN {
            return Err(PlanError::TooManyChildren {
                node_id: root_node_id.clone(),
                actual: children.len(),
                maximum: validation::MAX_DIRECT_CHILDREN,
            });
        }

        let existing = self.node_map();
        let scope_ids = live_subtree_ids(&existing, root_node_id);
        validate_scoped_inputs(&children, &existing, &scope_ids, None, root_node_id)?;
        let existing_child_count = existing
            .values()
            .filter(|node| {
                node.parent_id.as_ref() == Some(root_node_id)
                    && node.status != PlanNodeStatus::Superseded
            })
            .count();
        let revision = self.snapshot.revision + 1;
        let mut builder = TreeBuilder::new(self.next_node_sequence, revision, &existing);
        for (offset, child) in children.into_iter().enumerate() {
            builder.flatten(
                child,
                Some(root_node_id.clone()),
                (existing_child_count + offset) as u16,
                depth_of(&existing, root_node_id) + 1,
            )?;
        }
        let TreeBuilderOutput {
            nodes: new_nodes,
            client_key_ids,
            next_node_sequence,
            unresolved,
        } = builder.finish();
        let mut combined = existing;
        combined.extend(new_nodes);
        let live_ids = combined
            .values()
            .filter(|node| node.status != PlanNodeStatus::Superseded)
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        let mut dependency_client_keys = existing_client_keys(&combined);
        dependency_client_keys.extend(client_key_ids.clone());
        resolve_all_dependencies(
            &mut combined,
            unresolved,
            &dependency_client_keys,
            &live_ids,
        )?;
        let plan_root_id = self
            .snapshot
            .root_node_id
            .as_ref()
            .ok_or(PlanError::RootMissing)?;
        validation::validate_graph(&combined, plan_root_id)?;
        validation::validate_authorized_envelope(
            &combined,
            plan_root_id,
            self.snapshot.authorized_capability_envelope.as_ref(),
        )?;
        self.snapshot.revision = revision;
        self.snapshot.nodes = ordered_nodes(combined);
        self.next_node_sequence = next_node_sequence;
        Ok(client_key_ids)
    }

    pub(crate) fn replace_scoped_subtree(
        &mut self,
        scope_root_id: &PlanNodeId,
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
                operation: "replace scoped subtree",
            });
        }
        let existing = self.node_map();
        let scope_ids = live_subtree_ids(&existing, scope_root_id);
        if !scope_ids.contains(target_node_id) {
            return Err(PlanError::SubagentScopeViolation {
                reason: "replacement target is outside the linked subtree",
            });
        }
        let target =
            existing
                .get(target_node_id)
                .cloned()
                .ok_or(PlanError::SubagentScopeViolation {
                    reason: "replacement target is not a live scoped node",
                })?;
        if target_node_id != scope_root_id {
            ensure_mutable(&target)?;
        }
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
        validate_scoped_inputs(
            std::slice::from_ref(&subtree),
            &existing,
            &scope_ids,
            Some(target_node_id),
            scope_root_id,
        )?;

        let old_region = live_subtree_ids(&existing, target_node_id);
        for id in &old_region {
            let node = existing.get(id).expect("subtree id came from existing map");
            if id != scope_root_id {
                ensure_mutable(node)?;
            }
        }

        let revision = self.snapshot.revision + 1;
        let mut builder = TreeBuilder::new(self.next_node_sequence, revision, &existing);
        if target_node_id == scope_root_id {
            builder = builder.allow_existing_node(scope_root_id.clone());
        }
        let replacement_root = builder.flatten(
            subtree,
            target.parent_id.clone(),
            target.sibling_order,
            depth_of(&existing, target_node_id),
        )?;
        debug_assert_eq!(&replacement_root, target_node_id);
        let TreeBuilderOutput {
            nodes: mut replacement,
            client_key_ids,
            next_node_sequence,
            unresolved,
        } = builder.finish();

        for (id, replacement_node) in &mut replacement {
            if id != scope_root_id
                && let Some(current) = existing.get(id)
            {
                preserve_scoped_runtime_state(current, replacement_node, revision);
            }
        }
        if target_node_id == scope_root_id {
            let current_root = existing
                .get(scope_root_id)
                .expect("scope root exists after scope validation");
            let replacement_root = replacement
                .get_mut(scope_root_id)
                .expect("replacement root was built");
            preserve_scoped_root(current_root, replacement_root, revision);
        }

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
        let mut dependency_client_keys = existing_client_keys(&combined);
        dependency_client_keys.extend(client_key_ids.clone());
        resolve_all_dependencies(
            &mut combined,
            unresolved,
            &dependency_client_keys,
            &live_ids,
        )?;
        let plan_root_id = self
            .snapshot
            .root_node_id
            .clone()
            .ok_or(PlanError::RootMissing)?;
        validation::validate_graph(&combined, &plan_root_id)?;
        self.snapshot.revision = revision;
        self.snapshot.nodes = ordered_nodes(combined);
        self.next_node_sequence = next_node_sequence;
        Ok(client_key_ids)
    }

    fn node_map(&self) -> BTreeMap<PlanNodeId, PlanNodeSnapshot> {
        self.snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect()
    }

    fn validate_subagent_scope(
        &self,
        plan_id: &PlanId,
        root_node_id: &PlanNodeId,
        binding_id: &PlanBindingId,
    ) -> Result<(), PlanError> {
        if &self.snapshot.plan_id != plan_id {
            return Err(PlanError::SubagentScopeViolation {
                reason: "scope plan id does not match the active plan",
            });
        }
        let root = self
            .node(root_node_id)
            .ok_or(PlanError::SubagentScopeViolation {
                reason: "scope root node does not exist",
            })?;
        if !root.links.iter().any(|link| {
            link.plan_id == self.snapshot.plan_id
                && link.node_id == *root_node_id
                && link.binding_id == *binding_id
                && link.status != PlanLinkStatus::Superseded
                && link.superseded_by.is_none()
        }) {
            return Err(PlanError::SubagentScopeViolation {
                reason: "scope root is not owned by the linked binding",
            });
        }
        Ok(())
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
        let TreeBuilderOutput {
            mut nodes,
            client_key_ids,
            next_node_sequence,
            unresolved,
        } = builder.finish();
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
        let old_region = live_subtree_ids(&existing, target_node_id);
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
        let TreeBuilderOutput {
            nodes: replacement,
            client_key_ids,
            next_node_sequence,
            unresolved,
        } = builder.finish();
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
        self.snapshot.revision = revision;
        self.snapshot.nodes = ordered_nodes(combined);
        self.next_node_sequence = next_node_sequence;
        Ok(client_key_ids)
    }

    fn use_current_plan(
        &mut self,
        expected_revision: u64,
    ) -> Result<BTreeMap<String, PlanNodeId>, PlanError> {
        if self.snapshot.phase != PlanPhase::Planning {
            return Err(PlanError::WrongPhase {
                actual: self.snapshot.phase,
                operation: "use current plan",
            });
        }
        if self.snapshot.root_node_id.is_none() {
            return Err(PlanError::EmptyPlan);
        }
        if expected_revision != self.snapshot.revision {
            return Err(PlanError::StalePlanRevision {
                expected: expected_revision,
                actual: self.snapshot.revision,
            });
        }
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
        Ok(BTreeMap::new())
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

    fn current_plan_capability_envelope(
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

    fn add_review_requirement(&mut self, kind: PlanApprovalRequirementKind) {
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

    fn root_contract(&self) -> Option<RootContract> {
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

    fn record_root_contract_changes(&mut self, established: Option<&RootContract>) {
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

    fn authorized_envelope_covers_plan(&self) -> Result<bool, PlanError> {
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

    fn has_pending_approval_requirements(&self) -> bool {
        self.snapshot
            .approval_requirements
            .iter()
            .any(|requirement| requirement.status == PlanApprovalRequirementStatus::Pending)
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
    allow_existing_node: Option<PlanNodeId>,
}

struct TreeBuilderOutput {
    nodes: BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    client_key_ids: BTreeMap<String, PlanNodeId>,
    next_node_sequence: u64,
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
            allow_existing_node: None,
        }
    }

    fn allow_existing_node(mut self, node_id: PlanNodeId) -> Self {
        self.allow_existing_node = Some(node_id);
        self
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
        if let Some(status) = input.status {
            validate_authored_status(status)?;
        }
        let (
            id,
            created_revision,
            client_key,
            executor_policy,
            harness,
            recovery_policy,
            inherited_status,
        ) = match (input.id, input.client_key) {
            (None, Some(client_key)) => {
                validation::validate_client_key(&client_key)?;
                if self.client_key_ids.contains_key(&client_key) {
                    return Err(PlanError::DuplicateClientKey { client_key });
                }
                let id = PlanNodeId::new(&format!("plan-node-{}", self.next_node_sequence))
                    .expect("runtime-generated node id is valid");
                self.next_node_sequence += 1;
                self.client_key_ids.insert(client_key.clone(), id.clone());
                (
                    id,
                    self.revision,
                    Some(client_key),
                    input.executor_policy,
                    input.harness,
                    input.recovery_policy,
                    None,
                )
            }
            (Some(id), None) => {
                let existing = self
                    .existing
                    .get(&id)
                    .ok_or_else(|| PlanError::UnknownNode {
                        node_id: id.clone(),
                    })?;
                if self.allow_existing_node.as_ref() != Some(&id) {
                    ensure_mutable(existing)?;
                }
                (
                    id,
                    existing.created_revision,
                    existing.client_key.clone(),
                    existing.executor_policy,
                    existing.harness.clone(),
                    existing.recovery_policy.clone(),
                    Some(existing.declared_status),
                )
            }
            (None, None) => return Err(PlanError::InvalidNewNodeIdentity),
            (Some(_), Some(_)) => return Err(PlanError::InvalidExistingNodeIdentity),
        };
        if self.nodes.contains_key(&id) {
            return Err(PlanError::DuplicateNodeId { node_id: id });
        }
        let dependencies = input.depends_on;
        let children = input.children;
        let declared_status = input.status.or(inherited_status).unwrap_or_default();
        let node = PlanNodeSnapshot {
            id: id.clone(),
            client_key,
            parent_id,
            sibling_order,
            objective: input.objective,
            acceptance: input.acceptance,
            status: declared_status,
            executor_policy,
            harness,
            recovery_policy,
            depends_on: Vec::new(),
            result: None,
            created_revision,
            updated_revision: self.revision,
            declared_status,
            execution_summary: Default::default(),
            links: Vec::new(),
        };
        self.nodes.insert(id.clone(), node);
        self.unresolved.insert(id.clone(), dependencies);
        for (order, child) in children.into_iter().enumerate() {
            self.flatten(child, Some(id.clone()), order as u16, depth + 1)?;
        }
        Ok(id)
    }

    fn finish(self) -> TreeBuilderOutput {
        TreeBuilderOutput {
            nodes: self.nodes,
            client_key_ids: self.client_key_ids,
            next_node_sequence: self.next_node_sequence,
            unresolved: self.unresolved,
        }
    }
}

fn validate_authored_status(status: PlanNodeStatus) -> Result<(), PlanError> {
    match status {
        PlanNodeStatus::Pending
        | PlanNodeStatus::InProgress
        | PlanNodeStatus::Completed
        | PlanNodeStatus::Failed => Ok(()),
        runtime_status => Err(PlanError::InvalidAuthoredNodeStatus {
            status: runtime_status,
        }),
    }
}

fn validate_scoped_inputs(
    inputs: &[PlanNodeInput],
    existing: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    scope_ids: &BTreeSet<PlanNodeId>,
    replacement_target: Option<&PlanNodeId>,
    scope_root_id: &PlanNodeId,
) -> Result<(), PlanError> {
    let mut local_client_keys = BTreeSet::new();
    collect_scoped_client_keys(inputs, &mut local_client_keys)?;
    let existing_client_keys = existing_client_keys(existing);
    for client_key in &local_client_keys {
        if existing_client_keys.contains_key(client_key) {
            return Err(PlanError::DuplicateClientKey {
                client_key: client_key.clone(),
            });
        }
    }
    for input in inputs {
        validate_scoped_input_node(
            input,
            true,
            existing,
            scope_ids,
            replacement_target,
            scope_root_id,
            &local_client_keys,
            &existing_client_keys,
        )?;
    }
    Ok(())
}

fn collect_scoped_client_keys(
    inputs: &[PlanNodeInput],
    keys: &mut BTreeSet<String>,
) -> Result<(), PlanError> {
    for input in inputs {
        if let Some(client_key) = input.client_key.as_deref() {
            validation::validate_client_key(client_key)?;
            if !keys.insert(client_key.to_owned()) {
                return Err(PlanError::DuplicateClientKey {
                    client_key: client_key.to_owned(),
                });
            }
        }
        collect_scoped_client_keys(&input.children, keys)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_scoped_input_node(
    input: &PlanNodeInput,
    is_top_level: bool,
    existing: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    scope_ids: &BTreeSet<PlanNodeId>,
    replacement_target: Option<&PlanNodeId>,
    scope_root_id: &PlanNodeId,
    local_client_keys: &BTreeSet<String>,
    existing_client_keys: &BTreeMap<String, PlanNodeId>,
) -> Result<(), PlanError> {
    if let Some(status) = input.status {
        validate_authored_status(status)?;
    }
    if input.id.is_some() {
        let Some(target_id) = replacement_target else {
            return Err(PlanError::SubagentScopeViolation {
                reason: "child input cannot provide an existing node id",
            });
        };
        if !is_top_level || input.id.as_ref() != Some(target_id) {
            return Err(PlanError::SubagentScopeViolation {
                reason: "child input cannot replace an id outside the target root",
            });
        }
        let existing_target = existing
            .get(target_id)
            .ok_or(PlanError::SubagentScopeViolation {
                reason: "replacement target is not present in the active plan",
            })?;
        if input.executor_policy != existing_target.executor_policy
            || input.harness != existing_target.harness
            || input.recovery_policy != existing_target.recovery_policy
        {
            return Err(PlanError::SubagentScopeViolation {
                reason: "child input cannot modify runtime-owned execution policy",
            });
        }
        if target_id == scope_root_id {
            validate_scoped_root_contract(input, existing_target, existing, existing_client_keys)?;
        } else {
            validate_scoped_dependency_refs(
                &input.depends_on,
                scope_ids,
                local_client_keys,
                existing_client_keys,
            )?;
        }
    } else {
        if input.executor_policy != merry_core::PlanExecutorPolicy::default()
            || input.harness != merry_core::PlanHarnessSnapshot::default()
            || input.recovery_policy != merry_core::PlanRecoveryPolicySnapshot::default()
        {
            return Err(PlanError::SubagentScopeViolation {
                reason: "new child input cannot author runtime-owned fields",
            });
        }
        validate_scoped_dependency_refs(
            &input.depends_on,
            scope_ids,
            local_client_keys,
            existing_client_keys,
        )?;
    }
    for child in &input.children {
        validate_scoped_input_node(
            child,
            false,
            existing,
            scope_ids,
            replacement_target,
            scope_root_id,
            local_client_keys,
            existing_client_keys,
        )?;
    }
    Ok(())
}

fn validate_scoped_root_contract(
    input: &PlanNodeInput,
    current: &PlanNodeSnapshot,
    existing: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    existing_client_keys: &BTreeMap<String, PlanNodeId>,
) -> Result<(), PlanError> {
    if input.objective != current.objective
        || input.acceptance != current.acceptance
        || input
            .status
            .is_some_and(|status| status != current.declared_status)
    {
        return Err(PlanError::SubagentScopeViolation {
            reason: "child input cannot modify the linked root contract or status projection",
        });
    }
    let live_ids = existing
        .values()
        .filter(|node| node.status != PlanNodeStatus::Superseded)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let resolved =
        validation::resolve_dependencies(&input.depends_on, existing_client_keys, &live_ids)
            .map_err(|_| PlanError::SubagentScopeViolation {
                reason: "child input cannot modify the linked root dependencies",
            })?;
    if resolved != current.depends_on {
        return Err(PlanError::SubagentScopeViolation {
            reason: "child input cannot modify the linked root dependencies",
        });
    }
    Ok(())
}

fn validate_scoped_dependency_refs(
    references: &[PlanNodeReferenceInput],
    scope_ids: &BTreeSet<PlanNodeId>,
    local_client_keys: &BTreeSet<String>,
    existing_client_keys: &BTreeMap<String, PlanNodeId>,
) -> Result<(), PlanError> {
    for reference in references {
        match reference {
            PlanNodeReferenceInput::Id { id } if !scope_ids.contains(id) => {
                return Err(PlanError::SubagentScopeViolation {
                    reason: "child input dependency escapes the linked subtree",
                });
            }
            PlanNodeReferenceInput::ClientKey { client_key }
                if local_client_keys.contains(client_key) => {}
            PlanNodeReferenceInput::ClientKey { client_key } => {
                let Some(id) = existing_client_keys.get(client_key) else {
                    return Err(PlanError::UnknownClientKey {
                        client_key: client_key.clone(),
                    });
                };
                if !scope_ids.contains(id) {
                    return Err(PlanError::SubagentScopeViolation {
                        reason: "child input dependency escapes the linked subtree",
                    });
                }
            }
            PlanNodeReferenceInput::Id { .. } => {}
        }
    }
    Ok(())
}

fn existing_client_keys(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
) -> BTreeMap<String, PlanNodeId> {
    nodes
        .values()
        .filter_map(|node| node.client_key.clone().map(|key| (key, node.id.clone())))
        .collect()
}

fn preserve_scoped_root(
    current: &PlanNodeSnapshot,
    replacement: &mut PlanNodeSnapshot,
    revision: u64,
) {
    replacement.parent_id = current.parent_id.clone();
    replacement.sibling_order = current.sibling_order;
    replacement.objective = current.objective.clone();
    replacement.acceptance = current.acceptance.clone();
    replacement.executor_policy = current.executor_policy;
    replacement.harness = current.harness.clone();
    replacement.recovery_policy = current.recovery_policy.clone();
    replacement.depends_on = current.depends_on.clone();
    replacement.result = current.result.clone();
    replacement.created_revision = current.created_revision;
    replacement.updated_revision = revision;
    replacement.status = current.status;
    replacement.declared_status = current.declared_status;
    replacement.execution_summary = current.execution_summary.clone();
    replacement.links = current.links.clone();
}

fn preserve_scoped_runtime_state(
    current: &PlanNodeSnapshot,
    replacement: &mut PlanNodeSnapshot,
    revision: u64,
) {
    replacement.result = current.result.clone();
    replacement.updated_revision = revision;
    replacement.status = current.status;
    replacement.execution_summary = current.execution_summary.clone();
    replacement.links = current.links.clone();
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

fn live_subtree_ids(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    target: &PlanNodeId,
) -> BTreeSet<PlanNodeId> {
    let mut ids = BTreeSet::from([target.clone()]);
    loop {
        let before = ids.len();
        for node in nodes.values() {
            if node.status != PlanNodeStatus::Superseded
                && node
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
