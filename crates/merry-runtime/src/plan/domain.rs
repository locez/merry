use crate::plan::{
    domain::tree::{
        TreeBuilder, TreeBuilderOutput, depth_of, live_subtree_ids, ordered_nodes,
        resolve_all_dependencies,
    },
    protocol::{PlanChangeInput, PlanNodeInput, PlanUpdateOutput, UpdatePlanInput},
    validation,
};
pub use error::PlanError;
use merry_core::{
    PlanActivationSource, PlanId, PlanLinkStatus, PlanNodeId, PlanNodeSnapshot, PlanNodeStatus,
    PlanPhase, PlanResourcePolicySnapshot, PlanRevisionSummary, PlanSchedulerStatus, PlanSnapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod admission;

mod error;

mod scoped;

mod tree;

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
        let TreeBuilderOutput {
            nodes: new_nodes,
            client_key_to_runtime_node_id,
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
        resolve_all_dependencies(
            &mut combined,
            unresolved,
            &client_key_to_runtime_node_id,
            &live_ids,
        )?;
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
        Ok(client_key_to_runtime_node_id)
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
        validation::validate_snapshot_limits(&persisted.snapshot)?;
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
        let mut state = Self {
            snapshot: persisted.snapshot,
            next_node_sequence: persisted.next_node_sequence,
            next_approval_sequence: persisted.next_approval_sequence,
            next_attempt_sequence: persisted.next_attempt_sequence,
            next_lease_sequence: persisted.next_lease_sequence,
            next_directive_sequence: persisted.next_directive_sequence,
        };
        // Older persisted plans could remain in `Verifying` after every direct
        // child had completed. Recompute this derived root state on resume.
        let revision = state.snapshot.revision;
        state.refresh_parent_states(revision);
        Ok(state)
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
        let client_key_to_runtime_node_id = match input.change {
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
        validation::validate_snapshot_limits(&candidate.snapshot)?;
        *self = candidate;
        Ok(PlanUpdateOutput {
            snapshot: self.snapshot.clone(),
            client_key_to_runtime_node_id,
        })
    }

    fn node_map(&self) -> BTreeMap<PlanNodeId, PlanNodeSnapshot> {
        self.snapshot
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect()
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
            client_key_to_runtime_node_id,
            next_node_sequence,
            unresolved,
        } = builder.finish();
        let live_ids = nodes.keys().cloned().collect::<BTreeSet<_>>();
        resolve_all_dependencies(
            &mut nodes,
            unresolved,
            &client_key_to_runtime_node_id,
            &live_ids,
        )?;
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
        Ok(client_key_to_runtime_node_id)
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
            client_key_to_runtime_node_id,
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
        resolve_all_dependencies(
            &mut combined,
            unresolved,
            &client_key_to_runtime_node_id,
            &live_ids,
        )?;
        let root_id = self
            .snapshot
            .root_node_id
            .clone()
            .ok_or(PlanError::RootMissing)?;
        validation::validate_graph(&combined, &root_id)?;
        self.snapshot.revision = revision;
        self.snapshot.nodes = ordered_nodes(combined);
        self.next_node_sequence = next_node_sequence;
        Ok(client_key_to_runtime_node_id)
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

    #[cfg(test)]
    pub(crate) fn advance_unrelated_revision_for_test(&mut self) {
        self.snapshot.revision += 1;
    }
}

fn ensure_mutable(node: &PlanNodeSnapshot) -> Result<(), PlanError> {
    if node
        .links
        .iter()
        .any(|link| link.status == PlanLinkStatus::Active && link.superseded_by.is_none())
    {
        return Err(PlanError::ActiveSubagentOwnsSubtree {
            node_id: node.id.clone(),
        });
    }
    if node.status != PlanNodeStatus::Pending {
        return Err(PlanError::NodeNotMutable {
            node_id: node.id.clone(),
            status: node.status,
        });
    }
    Ok(())
}
