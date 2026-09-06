use crate::plan::{
    domain::{
        PlanState, ensure_mutable,
        error::PlanError,
        tree::{
            TreeBuilder, TreeBuilderOutput, depth_of, existing_client_keys, live_subtree_ids,
            ordered_nodes, resolve_all_dependencies, validate_authored_status,
        },
    },
    protocol::{
        PlanNodeInput, PlanNodeReferenceInput, PlanUpdateOutput, SubagentPlanChangeInput,
        SubagentPlanUpdateInput,
    },
    validation,
};
use merry_core::{
    PlanBindingId, PlanId, PlanLinkStatus, PlanNodeId, PlanNodeSnapshot, PlanNodeStatus, PlanPhase,
    PlanRevisionSummary,
};
use std::collections::{BTreeMap, BTreeSet};

impl PlanState {
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
        let client_key_to_runtime_node_id = match input.change {
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
        validation::validate_snapshot_limits(&candidate.snapshot)?;
        *self = candidate;
        Ok(PlanUpdateOutput {
            snapshot: self.snapshot.clone(),
            client_key_to_runtime_node_id,
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
        let mut dependency_client_keys = existing_client_keys(&combined);
        dependency_client_keys.extend(client_key_to_runtime_node_id.clone());
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
        Ok(client_key_to_runtime_node_id)
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
            client_key_to_runtime_node_id,
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
        dependency_client_keys.extend(client_key_to_runtime_node_id.clone());
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
        Ok(client_key_to_runtime_node_id)
    }

    pub(super) fn validate_subagent_scope(
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
                && link.status == PlanLinkStatus::Active
                && link.superseded_by.is_none()
        }) {
            return Err(PlanError::SubagentScopeViolation {
                reason: "scope root is not owned by the linked binding",
            });
        }
        Ok(())
    }
}

pub(super) fn validate_scoped_inputs(
    inputs: &[PlanNodeInput],
    existing: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    scope_ids: &BTreeSet<PlanNodeId>,
    replacement_target: Option<&PlanNodeId>,
    scope_root_id: &PlanNodeId,
) -> Result<(), PlanError> {
    if replacement_target.is_none() && inputs.iter().any(|input| !input.children.is_empty()) {
        return Err(PlanError::NestedPlanInput);
    }
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

pub(super) fn collect_scoped_client_keys(
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
pub(super) fn validate_scoped_input_node(
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

pub(super) fn validate_scoped_root_contract(
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

pub(super) fn validate_scoped_dependency_refs(
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

pub(super) fn preserve_scoped_root(
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

pub(super) fn preserve_scoped_runtime_state(
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
