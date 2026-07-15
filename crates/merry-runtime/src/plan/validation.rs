use super::{PlanError, protocol::PlanNodeReferenceInput};
use merry_core::{
    PlanCapabilityEnvelopeSnapshot, PlanHarnessSnapshot, PlanNodeId, PlanNodeSnapshot,
    PlanNodeStatus,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(crate) const MAX_PLAN_NODES: usize = 128;
pub(crate) const MAX_DIRECT_CHILDREN: usize = 16;
pub(crate) const MAX_DEPENDENCIES: usize = 16;
pub(crate) const MAX_ACCEPTANCE_ITEMS: usize = 16;
pub(crate) const MAX_PLAN_DEPTH: usize = 16;
pub(crate) const MAX_OBJECTIVE_BYTES: usize = 2 * 1024;
pub(crate) const MAX_ACCEPTANCE_BYTES: usize = 1024;
pub(crate) const MAX_REASON_BYTES: usize = 2 * 1024;
pub(crate) const MAX_CLIENT_KEY_BYTES: usize = 128;

pub(super) fn validate_reason(reason: &str) -> Result<(), PlanError> {
    validate_text("reason", reason, MAX_REASON_BYTES)
}

pub(super) fn validate_node_text(objective: &str, acceptance: &[String]) -> Result<(), PlanError> {
    validate_text("objective", objective, MAX_OBJECTIVE_BYTES)?;
    if acceptance.len() > MAX_ACCEPTANCE_ITEMS {
        return Err(PlanError::TooManyAcceptanceItems {
            actual: acceptance.len(),
            maximum: MAX_ACCEPTANCE_ITEMS,
        });
    }
    for item in acceptance {
        validate_text("acceptance", item, MAX_ACCEPTANCE_BYTES)?;
    }
    Ok(())
}

pub(super) fn validate_client_key(client_key: &str) -> Result<(), PlanError> {
    validate_text("client_key", client_key, MAX_CLIENT_KEY_BYTES)
}

pub(super) fn resolve_dependencies(
    refs: &[PlanNodeReferenceInput],
    client_key_ids: &BTreeMap<String, PlanNodeId>,
    live_ids: &BTreeSet<PlanNodeId>,
) -> Result<Vec<PlanNodeId>, PlanError> {
    if refs.len() > MAX_DEPENDENCIES {
        return Err(PlanError::TooManyDependencies {
            actual: refs.len(),
            maximum: MAX_DEPENDENCIES,
        });
    }
    let mut resolved = BTreeSet::new();
    for reference in refs {
        let id = match reference {
            PlanNodeReferenceInput::Id { id } => id.clone(),
            PlanNodeReferenceInput::ClientKey { client_key } => client_key_ids
                .get(client_key)
                .cloned()
                .ok_or_else(|| PlanError::UnknownClientKey {
                    client_key: client_key.clone(),
                })?,
        };
        if !live_ids.contains(&id) {
            return Err(PlanError::UnknownDependency { node_id: id });
        }
        resolved.insert(id);
    }
    Ok(resolved.into_iter().collect())
}

pub(super) fn validate_graph(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    root_id: &PlanNodeId,
) -> Result<(), PlanError> {
    let live = nodes
        .values()
        .filter(|node| node.status != PlanNodeStatus::Superseded)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    if live.len() > MAX_PLAN_NODES {
        return Err(PlanError::TooManyNodes {
            actual: live.len(),
            maximum: MAX_PLAN_NODES,
        });
    }
    if !live.contains(root_id) {
        return Err(PlanError::RootMissing);
    }

    for node in nodes.values().filter(|node| live.contains(&node.id)) {
        if node.id == *root_id {
            if node.parent_id.is_some() {
                return Err(PlanError::RootHasParent);
            }
        } else {
            let parent = node
                .parent_id
                .as_ref()
                .ok_or_else(|| PlanError::NodeMissingParent {
                    node_id: node.id.clone(),
                })?;
            if !live.contains(parent) {
                return Err(PlanError::UnknownParent {
                    node_id: node.id.clone(),
                    parent_id: parent.clone(),
                });
            }
        }
        for dependency in &node.depends_on {
            if dependency == &node.id {
                return Err(PlanError::SelfDependency {
                    node_id: node.id.clone(),
                });
            }
            if !live.contains(dependency) {
                return Err(PlanError::UnknownDependency {
                    node_id: dependency.clone(),
                });
            }
            if is_descendant(nodes, dependency, &node.id) {
                return Err(PlanError::DependsOnDescendant {
                    node_id: node.id.clone(),
                    dependency_id: dependency.clone(),
                });
            }
        }
    }

    validate_parent_reachability(nodes, root_id, &live)?;
    validate_dependency_cycles(nodes, &live)?;
    validate_sibling_order(nodes, &live)?;
    validate_harness_tree(nodes, root_id, None)?;
    Ok(())
}

pub(super) fn validate_authorized_envelope(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    root_id: &PlanNodeId,
    envelope: Option<&PlanCapabilityEnvelopeSnapshot>,
) -> Result<(), PlanError> {
    let Some(envelope) = envelope else {
        return Ok(());
    };
    let root = nodes.get(root_id).ok_or(PlanError::RootMissing)?;
    validate_harness_against_envelope(&root.id, &root.harness, envelope)
}

fn validate_parent_reachability(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    root_id: &PlanNodeId,
    live: &BTreeSet<PlanNodeId>,
) -> Result<(), PlanError> {
    for id in live {
        let mut cursor = id;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(cursor.clone()) {
                return Err(PlanError::ParentCycle);
            }
            if cursor == root_id {
                break;
            }
            cursor = nodes
                .get(cursor)
                .and_then(|node| node.parent_id.as_ref())
                .ok_or_else(|| PlanError::UnreachableNode {
                    node_id: id.clone(),
                })?;
        }
    }
    Ok(())
}

fn validate_dependency_cycles(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    live: &BTreeSet<PlanNodeId>,
) -> Result<(), PlanError> {
    fn visit(
        id: &PlanNodeId,
        nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
        live: &BTreeSet<PlanNodeId>,
        visiting: &mut BTreeSet<PlanNodeId>,
        visited: &mut BTreeSet<PlanNodeId>,
    ) -> Result<(), PlanError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.clone()) {
            return Err(PlanError::DependencyCycle);
        }
        let node = nodes.get(id).ok_or_else(|| PlanError::UnknownDependency {
            node_id: id.clone(),
        })?;
        for dependency in node.depends_on.iter().filter(|id| live.contains(*id)) {
            visit(dependency, nodes, live, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id.clone());
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in live {
        visit(id, nodes, live, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn validate_sibling_order(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    live: &BTreeSet<PlanNodeId>,
) -> Result<(), PlanError> {
    let mut seen = BTreeSet::new();
    for node in nodes.values().filter(|node| live.contains(&node.id)) {
        if !seen.insert((node.parent_id.clone(), node.sibling_order)) {
            return Err(PlanError::DuplicateSiblingOrder {
                parent_id: node.parent_id.clone(),
                sibling_order: node.sibling_order,
            });
        }
    }
    Ok(())
}

fn validate_harness_tree(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    node_id: &PlanNodeId,
    parent_harness: Option<&PlanHarnessSnapshot>,
) -> Result<(), PlanError> {
    let node = nodes.get(node_id).ok_or_else(|| PlanError::UnknownNode {
        node_id: node_id.clone(),
    })?;
    validate_harness_paths(&node.id, &node.harness)?;
    if let Some(parent) = parent_harness {
        validate_harness_subset(&node.id, parent, &node.harness)?;
    }
    let mut children = nodes
        .values()
        .filter(|candidate| {
            candidate.status != PlanNodeStatus::Superseded
                && candidate.parent_id.as_ref() == Some(node_id)
        })
        .collect::<Vec<_>>();
    children.sort_by_key(|child| (child.sibling_order, child.id.clone()));
    if children.len() > MAX_DIRECT_CHILDREN {
        return Err(PlanError::TooManyChildren {
            node_id: node_id.clone(),
            actual: children.len(),
            maximum: MAX_DIRECT_CHILDREN,
        });
    }
    for child in children {
        validate_harness_tree(nodes, &child.id, Some(&node.harness))?;
    }
    Ok(())
}

fn validate_harness_against_envelope(
    node_id: &PlanNodeId,
    harness: &PlanHarnessSnapshot,
    envelope: &PlanCapabilityEnvelopeSnapshot,
) -> Result<(), PlanError> {
    if !set_is_subset(&harness.allowed_tools, &envelope.allowed_tools)
        || !scopes_are_within(&harness.read_scope, &envelope.read_scope)
        || !scopes_are_within(&harness.write_scope, &envelope.write_scope)
        || !set_is_subset(&envelope.forbidden_paths, &harness.forbidden_paths)
    {
        return Err(PlanError::CapabilityEnvelopeExceeded {
            node_id: node_id.clone(),
        });
    }
    Ok(())
}

fn validate_harness_subset(
    node_id: &PlanNodeId,
    parent: &PlanHarnessSnapshot,
    child: &PlanHarnessSnapshot,
) -> Result<(), PlanError> {
    if !set_is_subset(&child.allowed_tools, &parent.allowed_tools)
        || !scopes_are_within(&child.read_scope, &parent.read_scope)
        || !scopes_are_within(&child.write_scope, &parent.write_scope)
        || !set_is_subset(&parent.forbidden_paths, &child.forbidden_paths)
    {
        return Err(PlanError::CapabilityEnvelopeExceeded {
            node_id: node_id.clone(),
        });
    }
    Ok(())
}

fn validate_harness_paths(
    node_id: &PlanNodeId,
    harness: &PlanHarnessSnapshot,
) -> Result<(), PlanError> {
    for path in harness
        .read_scope
        .iter()
        .chain(&harness.write_scope)
        .chain(&harness.forbidden_paths)
    {
        validate_scope_path(path).map_err(|_| PlanError::InvalidScopePath {
            node_id: node_id.clone(),
            path: path.clone(),
        })?;
    }
    Ok(())
}

fn validate_scope_path(value: &str) -> Result<(), ()> {
    crate::workspace_scope::is_valid_workspace_scope(Path::new(value))
        .then_some(())
        .ok_or(())
}

fn scopes_are_within(child: &[String], parent: &[String]) -> bool {
    child.iter().all(|child_path| {
        parent.iter().any(|parent_path| {
            let child = Path::new(child_path);
            let parent = Path::new(parent_path);
            crate::workspace_scope::workspace_scope_contains(parent, child)
        })
    })
}

fn set_is_subset<T: Ord>(child: &[T], parent: &[T]) -> bool {
    let parent = parent.iter().collect::<BTreeSet<_>>();
    child.iter().all(|value| parent.contains(value))
}

fn is_descendant(
    nodes: &BTreeMap<PlanNodeId, PlanNodeSnapshot>,
    candidate: &PlanNodeId,
    ancestor: &PlanNodeId,
) -> bool {
    let mut cursor = nodes
        .get(candidate)
        .and_then(|node| node.parent_id.as_ref());
    while let Some(parent) = cursor {
        if parent == ancestor {
            return true;
        }
        cursor = nodes.get(parent).and_then(|node| node.parent_id.as_ref());
    }
    false
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), PlanError> {
    if value.trim().is_empty() {
        return Err(PlanError::InvalidText {
            field,
            reason: "must not be blank",
        });
    }
    if value.len() > maximum {
        return Err(PlanError::InvalidText {
            field,
            reason: "is longer than the allowed maximum",
        });
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\n')
    {
        return Err(PlanError::InvalidText {
            field,
            reason: "contains unsupported control characters",
        });
    }
    Ok(())
}
