use super::{
    PlanController, PlanControllerError, PlanError, PlanUpdateOutput,
    protocol::SubagentPlanUpdateInput,
};
use merry_core::{PlanBindingId, PlanId, PlanLinkStatus, PlanNodeId, PlanSnapshot};
use std::collections::{BTreeMap, BTreeSet};

/// Child-owned view and mutation capability for one linked Plan node.
#[derive(Clone)]
pub(crate) struct PlanSubagentScope {
    pub(crate) plan_id: PlanId,
    pub(crate) root_node_id: PlanNodeId,
    pub(crate) binding_id: PlanBindingId,
    pub(crate) controller: PlanController,
}

impl PlanSubagentScope {
    pub(crate) fn new(
        plan_id: PlanId,
        root_node_id: PlanNodeId,
        binding_id: PlanBindingId,
        controller: PlanController,
    ) -> Self {
        Self {
            plan_id,
            root_node_id,
            binding_id,
            controller,
        }
    }

    pub(crate) async fn read(&self) -> Result<PlanSnapshot, PlanControllerError> {
        let snapshot = self
            .controller
            .snapshot()
            .await?
            .ok_or(PlanControllerError::NoActivePlan)?;
        project_snapshot(
            snapshot,
            &self.plan_id,
            &self.root_node_id,
            &self.binding_id,
        )
    }

    pub(crate) async fn snapshot(&self) -> Result<PlanSnapshot, PlanControllerError> {
        self.read().await
    }

    pub(crate) async fn update(
        &self,
        input: SubagentPlanUpdateInput,
    ) -> Result<PlanUpdateOutput, PlanControllerError> {
        let output = self
            .controller
            .update_subagent(
                self.plan_id.clone(),
                self.root_node_id.clone(),
                self.binding_id.clone(),
                input,
            )
            .await?;
        Ok(PlanUpdateOutput {
            snapshot: project_snapshot(
                output.snapshot,
                &self.plan_id,
                &self.root_node_id,
                &self.binding_id,
            )?,
            client_key_to_runtime_node_id: output.client_key_to_runtime_node_id,
        })
    }

    pub(crate) async fn update_plan(
        &self,
        input: SubagentPlanUpdateInput,
    ) -> Result<PlanUpdateOutput, PlanControllerError> {
        self.update(input).await
    }
}

fn project_snapshot(
    mut snapshot: PlanSnapshot,
    plan_id: &PlanId,
    root_node_id: &PlanNodeId,
    binding_id: &PlanBindingId,
) -> Result<PlanSnapshot, PlanControllerError> {
    if snapshot.plan_id != *plan_id {
        return Err(scope_error("scope plan id does not match the active plan"));
    }
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.id == *root_node_id)
        .ok_or_else(|| scope_error("scope root node does not exist"))?;
    if !root.links.iter().any(|link| {
        link.plan_id == snapshot.plan_id
            && link.node_id == *root_node_id
            && link.binding_id == *binding_id
            && link.status == PlanLinkStatus::Active
            && link.superseded_by.is_none()
    }) {
        return Err(scope_error("scope root is not owned by the linked binding"));
    }

    let nodes = snapshot
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect::<BTreeMap<_, _>>();
    let scoped_ids = nodes
        .keys()
        .filter(|node_id| is_in_subtree(&nodes, node_id, root_node_id))
        .cloned()
        .collect::<BTreeSet<_>>();
    snapshot.nodes.retain(|node| scoped_ids.contains(&node.id));
    for node in &mut snapshot.nodes {
        if node.id == *root_node_id {
            node.parent_id = None;
            node.sibling_order = 0;
        } else {
            node.depends_on
                .retain(|dependency| scoped_ids.contains(dependency));
        }
        node.links.retain(|link| link.binding_id == *binding_id);
    }
    snapshot.root_node_id = Some(root_node_id.clone());
    snapshot.coordinator_node_id = None;
    snapshot
        .attempts
        .retain(|attempt| scoped_ids.contains(&attempt.node_id));
    snapshot
        .leases
        .retain(|lease| scoped_ids.contains(&lease.node_id));
    snapshot
        .attempt_progress
        .retain(|progress| scoped_ids.contains(&progress.node_id));
    snapshot
        .directives
        .retain(|directive| scoped_ids.contains(&directive.node_id));
    Ok(snapshot)
}

fn is_in_subtree(
    nodes: &BTreeMap<PlanNodeId, merry_core::PlanNodeSnapshot>,
    candidate: &PlanNodeId,
    root: &PlanNodeId,
) -> bool {
    let mut cursor = Some(candidate);
    let mut visited = BTreeSet::new();
    while let Some(node_id) = cursor {
        if !visited.insert(node_id.clone()) {
            return false;
        }
        if node_id == root {
            return true;
        }
        cursor = nodes.get(node_id).and_then(|node| node.parent_id.as_ref());
    }
    false
}

fn scope_error(reason: &'static str) -> PlanControllerError {
    PlanControllerError::Plan {
        source: PlanError::SubagentScopeViolation { reason },
    }
}

#[cfg(test)]
mod tests;
