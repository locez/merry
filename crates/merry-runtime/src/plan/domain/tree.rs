use crate::plan::{
    domain::{ensure_mutable, error::PlanError},
    protocol::PlanNodeInput,
    validation,
};
use merry_core::{PlanNodeId, PlanNodeSnapshot, PlanNodeStatus};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct TreeBuilder<'a> {
    pub(super) next_node_sequence: u64,
    pub(super) revision: u64,
    pub(super) existing: &'a BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    pub(super) nodes: BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    pub(super) client_key_to_runtime_node_id: BTreeMap<String, PlanNodeId>,
    pub(super) unresolved: BTreeMap<PlanNodeId, Vec<crate::plan::protocol::PlanNodeReferenceInput>>,
    pub(super) allow_existing_node: Option<PlanNodeId>,
}

pub(super) struct TreeBuilderOutput {
    pub(super) nodes: BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    pub(super) client_key_to_runtime_node_id: BTreeMap<String, PlanNodeId>,
    pub(super) next_node_sequence: u64,
    pub(super) unresolved: BTreeMap<PlanNodeId, Vec<crate::plan::protocol::PlanNodeReferenceInput>>,
}

impl<'a> TreeBuilder<'a> {
    pub(super) fn new(
        next_node_sequence: u64,
        revision: u64,
        existing: &'a BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    ) -> Self {
        Self {
            next_node_sequence,
            revision,
            existing,
            nodes: BTreeMap::new(),
            client_key_to_runtime_node_id: BTreeMap::new(),
            unresolved: BTreeMap::new(),
            allow_existing_node: None,
        }
    }

    pub(super) fn allow_existing_node(mut self, node_id: PlanNodeId) -> Self {
        self.allow_existing_node = Some(node_id);
        self
    }

    pub(super) fn flatten(
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
        validation::validate_recovery_policy(&input.recovery_policy)?;
        if input
            .children
            .iter()
            .any(|child| !child.children.is_empty())
        {
            return Err(PlanError::NestedPlanInput);
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
                if self.client_key_to_runtime_node_id.contains_key(&client_key) {
                    return Err(PlanError::DuplicateClientKey { client_key });
                }
                let id = PlanNodeId::new(&format!("plan-node-{}", self.next_node_sequence))
                    .expect("runtime-generated node id is valid");
                self.next_node_sequence += 1;
                self.client_key_to_runtime_node_id
                    .insert(client_key.clone(), id.clone());
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

    pub(super) fn finish(self) -> TreeBuilderOutput {
        TreeBuilderOutput {
            nodes: self.nodes,
            client_key_to_runtime_node_id: self.client_key_to_runtime_node_id,
            next_node_sequence: self.next_node_sequence,
            unresolved: self.unresolved,
        }
    }
}

pub(super) fn validate_authored_status(status: PlanNodeStatus) -> Result<(), PlanError> {
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

pub(super) fn existing_client_keys(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
) -> BTreeMap<String, PlanNodeId> {
    nodes
        .values()
        .filter_map(|node| node.client_key.clone().map(|key| (key, node.id.clone())))
        .collect()
}

pub(super) fn resolve_all_dependencies(
    nodes: &mut BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    unresolved: BTreeMap<PlanNodeId, Vec<crate::plan::protocol::PlanNodeReferenceInput>>,
    client_key_to_runtime_node_id: &BTreeMap<String, PlanNodeId>,
    live_ids: &BTreeSet<PlanNodeId>,
) -> Result<(), PlanError> {
    for (id, references) in unresolved {
        let resolved =
            validation::resolve_dependencies(&references, client_key_to_runtime_node_id, live_ids)?;
        nodes
            .get_mut(&id)
            .expect("unresolved entries belong to candidate nodes")
            .depends_on = resolved;
    }
    Ok(())
}

pub(super) fn ordered_nodes(
    nodes: BTreeMap<PlanNodeId, PlanNodeSnapshot>,
) -> Vec<PlanNodeSnapshot> {
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

pub(super) fn live_subtree_ids(
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

pub(super) fn depth_of(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    node_id: &PlanNodeId,
) -> usize {
    let mut depth = 1;
    let mut cursor = nodes.get(node_id).and_then(|node| node.parent_id.as_ref());
    while let Some(parent) = cursor {
        depth += 1;
        cursor = nodes.get(parent).and_then(|node| node.parent_id.as_ref());
    }
    depth
}
