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
            client_key_ids: output.client_key_ids,
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
            && link.status != PlanLinkStatus::Superseded
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
mod tests {
    use super::super::{
        BeginPlanInput, PlanChangeInput, PlanController, PlanError, PlanExecutionIntent,
        PlanNodeInput, PlanNodeReferenceInput, SubagentPlanChangeInput, SubagentPlanUpdateInput,
        UpdatePlanInput,
    };
    use crate::{FileSessionStore, session::SessionState};
    use merry_core::{
        PlanActivationSource, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanNodeId,
        PlanNodeStatus, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot,
        RuntimeJournalPayload, SessionId, SubagentId, SubagentTaskId,
    };
    use std::{num::NonZeroUsize, sync::Arc};
    use tokio::sync::Mutex;

    fn session_id() -> SessionId {
        SessionId::new("subagent-scope-test").expect("valid session id")
    }

    fn controller(
        store: Option<FileSessionStore>,
    ) -> (
        PlanController,
        super::super::controller::PlanControllerEventReceiver,
    ) {
        PlanController::start(
            Arc::new(Mutex::new(SessionState::new(session_id()))),
            store,
            NonZeroUsize::new(16).expect("non-zero buffer"),
        )
    }

    fn node(client_key: &str, objective: &str) -> PlanNodeInput {
        PlanNodeInput {
            id: None,
            client_key: Some(client_key.to_owned()),
            objective: objective.to_owned(),
            acceptance: vec![format!("{objective} is verified")],
            status: None,
            executor_policy: PlanExecutorPolicy::default(),
            harness: PlanHarnessSnapshot::default(),
            recovery_policy: PlanRecoveryPolicySnapshot::default(),
            depends_on: Vec::new(),
            children: Vec::new(),
        }
    }

    fn root(children: Vec<PlanNodeInput>) -> PlanNodeInput {
        PlanNodeInput {
            children,
            ..node("root", "Complete all work")
        }
    }

    async fn linked_scope(
        controller: &PlanController,
    ) -> (super::super::PlanSubagentScope, PlanNodeId, PlanNodeId) {
        controller
            .begin(BeginPlanInput {
                reason: "create a plan for scoped child ownership".to_owned(),
                governing_skill_id: None,
            })
            .await
            .expect("begin succeeds");
        let plan = controller
            .update(UpdatePlanInput {
                reason: "define coordinator work items".to_owned(),
                execution_intent: PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: PlanChangeInput::DefinePlan {
                    expected_plan_revision: 0,
                    root: root(vec![
                        node("owned", "Owned child work"),
                        node("sibling", "Sibling work"),
                    ]),
                },
            })
            .await
            .expect("coordinator update succeeds");
        let owned_id = plan.client_key_ids["owned"].clone();
        let sibling_id = plan.client_key_ids["sibling"].clone();
        let link = controller
            .bind_subagent(
                "owned".to_owned(),
                SubagentId::new("agent-scope").expect("valid subagent id"),
                SubagentTaskId::new("task-scope").expect("valid task id"),
                1,
            )
            .await
            .expect("binding succeeds");
        let scope = controller.subagent_scope(
            plan.snapshot.plan_id.clone(),
            owned_id.clone(),
            link.binding_id,
        );
        (scope, owned_id, sibling_id)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_define_children_only_exposes_and_updates_bound_subtree() {
        let (controller, _events) = controller(None);
        let (scope, owned_id, sibling_id) = linked_scope(&controller).await;

        let mut child = node("child", "Child-owned work");
        child
            .children
            .push(node("grandchild", "Nested child-owned work"));
        let output = scope
            .update(SubagentPlanUpdateInput {
                reason: "decompose the bound work item".to_owned(),
                change: SubagentPlanChangeInput::DefineChildren {
                    expected_plan_revision: 2,
                    children: vec![child],
                },
            })
            .await
            .expect("scoped define succeeds");

        assert_eq!(output.snapshot.revision, 3);
        assert!(
            output
                .snapshot
                .nodes
                .iter()
                .any(|node| node.client_key.as_deref() == Some("child"))
        );
        let scoped = scope.read().await.expect("scoped read succeeds");
        assert!(scoped.nodes.iter().any(|node| node.id == owned_id));
        assert!(!scoped.nodes.iter().any(|node| node.id == sibling_id));

        let full = controller
            .snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("active plan");
        assert!(full.nodes.iter().any(|node| node.id == sibling_id));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_define_rejects_external_dependency_without_partial_state() {
        let (controller, mut events) = controller(None);
        let (scope, _owned_id, sibling_id) = linked_scope(&controller).await;
        while events.try_recv().is_ok() {}
        let before = controller
            .snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("active plan");

        let mut child = node("invalid-child", "Invalid external dependency");
        child.depends_on = vec![PlanNodeReferenceInput::Id { id: sibling_id }];
        let error = scope
            .update(SubagentPlanUpdateInput {
                reason: "reject a dependency outside the child scope".to_owned(),
                change: SubagentPlanChangeInput::DefineChildren {
                    expected_plan_revision: before.revision,
                    children: vec![child],
                },
            })
            .await
            .expect_err("external dependency must reject");

        assert!(matches!(
            error,
            super::super::PlanControllerError::Plan {
                source: PlanError::SubagentScopeViolation { .. }
            }
        ));
        let after = controller
            .snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("active plan");
        assert_eq!(after.revision, before.revision);
        assert!(events.try_recv().is_err(), "failed update emits no event");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_replace_rejects_escape_and_runtime_root_changes() {
        let (controller, _events) = controller(None);
        let (scope, owned_id, sibling_id) = linked_scope(&controller).await;
        let before = controller
            .snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("active plan");
        let sibling = before
            .nodes
            .iter()
            .find(|node| node.id == sibling_id)
            .expect("sibling exists");
        let mut escaped = replacement_for(sibling, "attempt to replace sibling");
        escaped.status = None;
        let error = scope
            .update(SubagentPlanUpdateInput {
                reason: "reject target outside binding".to_owned(),
                change: SubagentPlanChangeInput::ReplaceSubtree {
                    target_node_id: sibling_id,
                    expected_node_revision: sibling.updated_revision,
                    subtree: escaped,
                },
            })
            .await
            .expect_err("sibling replacement must reject");
        assert!(matches!(
            error,
            super::super::PlanControllerError::Plan {
                source: PlanError::SubagentScopeViolation { .. }
            }
        ));

        let root = before
            .nodes
            .iter()
            .find(|node| node.id == owned_id)
            .expect("owned root exists");
        let mut changed_runtime = replacement_for(root, root.objective.as_str());
        changed_runtime.harness.write_scope = vec!["outside".to_owned()];
        let error = scope
            .update(SubagentPlanUpdateInput {
                reason: "reject a runtime scope change".to_owned(),
                change: SubagentPlanChangeInput::ReplaceSubtree {
                    target_node_id: owned_id,
                    expected_node_revision: root.updated_revision,
                    subtree: changed_runtime,
                },
            })
            .await
            .expect_err("runtime field change must reject");
        assert!(matches!(
            error,
            super::super::PlanControllerError::Plan {
                source: PlanError::SubagentScopeViolation { .. }
            }
        ));
        let after = controller
            .snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("active plan");
        assert_eq!(after.revision, before.revision);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_replace_can_revise_one_target_repeatedly() {
        let (controller, _events) = controller(None);
        let (scope, _owned_id, _sibling_id) = linked_scope(&controller).await;
        let defined = scope
            .update(SubagentPlanUpdateInput {
                reason: "define a child target".to_owned(),
                change: SubagentPlanChangeInput::DefineChildren {
                    expected_plan_revision: 2,
                    children: vec![node("target", "Initial target")],
                },
            })
            .await
            .expect("child definition succeeds");
        let target_id = defined.client_key_ids["target"].clone();
        let mut first = replacement_for(
            defined
                .snapshot
                .nodes
                .iter()
                .find(|node| node.id == target_id)
                .expect("target exists"),
            "First target revision",
        );
        first.children.push(node("nested", "Nested target work"));
        let first_output = scope
            .update(SubagentPlanUpdateInput {
                reason: "revise target once".to_owned(),
                change: SubagentPlanChangeInput::ReplaceSubtree {
                    target_node_id: target_id.clone(),
                    expected_node_revision: defined
                        .snapshot
                        .nodes
                        .iter()
                        .find(|node| node.id == target_id)
                        .expect("target exists")
                        .updated_revision,
                    subtree: first,
                },
            })
            .await
            .expect("first target revision succeeds");
        let current = first_output
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == target_id)
            .expect("target remains live");
        let second = scope
            .update(SubagentPlanUpdateInput {
                reason: "revise target twice".to_owned(),
                change: SubagentPlanChangeInput::ReplaceSubtree {
                    target_node_id: target_id.clone(),
                    expected_node_revision: current.updated_revision,
                    subtree: replacement_for(current, "Second target revision"),
                },
            })
            .await
            .expect("second target revision succeeds");
        assert_eq!(
            second
                .snapshot
                .nodes
                .iter()
                .find(|node| node.id == target_id)
                .expect("target remains live")
                .objective,
            "Second target revision"
        );
        assert!(second.snapshot.revision > first_output.snapshot.revision);
    }

    fn replacement_for(node: &merry_core::PlanNodeSnapshot, objective: &str) -> PlanNodeInput {
        PlanNodeInput {
            id: Some(node.id.clone()),
            client_key: None,
            objective: objective.to_owned(),
            acceptance: node.acceptance.clone(),
            status: None,
            executor_policy: node.executor_policy,
            harness: node.harness.clone(),
            recovery_policy: node.recovery_policy.clone(),
            depends_on: node
                .depends_on
                .iter()
                .cloned()
                .map(|id| PlanNodeReferenceInput::Id { id })
                .collect(),
            children: Vec::new(),
        }
    }

    #[test]
    fn authored_status_accepts_declarative_values_and_rejects_runtime_values() {
        for status in [
            PlanNodeStatus::Pending,
            PlanNodeStatus::InProgress,
            PlanNodeStatus::Completed,
            PlanNodeStatus::Failed,
        ] {
            let mut plan = super::super::domain::PlanState::empty(
                PlanId::new("status-plan").expect("valid plan id"),
                PlanActivationSource::Coordinator {
                    reason: "status test".to_owned(),
                    governing_skill_id: None,
                },
                PlanResourcePolicySnapshot::default(),
            );
            let mut root = node("root", "Status test");
            root.status = Some(status);
            let result = plan.update(UpdatePlanInput {
                reason: "accept authored status".to_owned(),
                execution_intent: PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: PlanChangeInput::DefinePlan {
                    expected_plan_revision: 0,
                    root,
                },
            });
            assert!(result.is_ok(), "{status:?} should be authored");
        }

        let mut plan = super::super::domain::PlanState::empty(
            PlanId::new("runtime-status-plan").expect("valid plan id"),
            PlanActivationSource::Coordinator {
                reason: "status test".to_owned(),
                governing_skill_id: None,
            },
            PlanResourcePolicySnapshot::default(),
        );
        let mut root = node("runtime-only", "Runtime-only status");
        root.status = Some(PlanNodeStatus::Expanded);
        let error = plan
            .update(UpdatePlanInput {
                reason: "reject runtime-owned status".to_owned(),
                execution_intent: PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: PlanChangeInput::DefinePlan {
                    expected_plan_revision: 0,
                    root,
                },
            })
            .expect_err("runtime-only authored status must reject");
        assert!(matches!(
            error,
            PlanError::InvalidAuthoredNodeStatus {
                status: PlanNodeStatus::Expanded
            }
        ));
    }

    #[test]
    fn omitted_status_does_not_replace_existing_declared_status() {
        let mut plan = super::super::domain::PlanState::empty(
            PlanId::new("status-preserve-plan").expect("valid plan id"),
            PlanActivationSource::Coordinator {
                reason: "status test".to_owned(),
                governing_skill_id: None,
            },
            PlanResourcePolicySnapshot::default(),
        );
        let initial = node("root", "Initial status");
        let first = plan
            .update(UpdatePlanInput {
                reason: "declare in progress".to_owned(),
                execution_intent: PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: PlanChangeInput::DefinePlan {
                    expected_plan_revision: 0,
                    root: initial,
                },
            })
            .expect("initial status update succeeds");
        let root_id = first.snapshot.root_node_id.expect("root id");
        let root_revision = plan.node(&root_id).expect("root").updated_revision;
        let mut replacement = node("ignored", "Revised status text");
        replacement.id = Some(root_id.clone());
        replacement.client_key = None;
        replacement.status = None;
        let output = plan
            .update(UpdatePlanInput {
                reason: "status omission must preserve declaration".to_owned(),
                execution_intent: PlanExecutionIntent::ContinuePlanning,
                coordinator_node_id: None,
                max_concurrency_hint: None,
                change: PlanChangeInput::ReplaceSubtree {
                    target_node_id: root_id.clone(),
                    expected_node_revision: root_revision,
                    subtree: replacement,
                },
            })
            .expect("omitted status keeps the current declaration");
        let root = output
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == root_id)
            .expect("root remains present");
        assert_eq!(root.declared_status, PlanNodeStatus::Pending);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_update_persists_and_emits_after_durable_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(temp.path());
        let (controller, mut events) = controller(Some(store.clone()));
        let (scope, _owned_id, _sibling_id) = linked_scope(&controller).await;
        let before = controller
            .snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("active plan");

        scope
            .update(SubagentPlanUpdateInput {
                reason: "persist child-owned declaration".to_owned(),
                change: SubagentPlanChangeInput::DefineChildren {
                    expected_plan_revision: before.revision,
                    children: vec![node("persisted-child", "Persisted child")],
                },
            })
            .await
            .expect("scoped update succeeds");
        for _ in 0..4 {
            assert!(matches!(
                events.recv().await.expect("plan event").payload,
                RuntimeJournalPayload::PlanUpdated { .. }
            ));
        }

        let loaded = SessionState::load_from(&store, &session_id())
            .await
            .expect("persisted session loads");
        let loaded_plan = loaded.active_plan().expect("persisted active plan");
        assert_eq!(loaded_plan.snapshot().revision, before.revision + 1);
        assert!(
            loaded_plan
                .snapshot()
                .nodes
                .iter()
                .any(|node| node.client_key.as_deref() == Some("persisted-child"))
        );
    }
}
