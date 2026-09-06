use super::{controller, controller_with_session, linked_scope, node};
use crate::plan::{
    BeginPlanInput, PlanChangeInput, PlanError, PlanExecutionIntent, PlanNodeInput,
    PlanNodeReferenceInput, SubagentPlanChangeInput, SubagentPlanUpdateInput, UpdatePlanInput,
};
use merry_core::{
    PlanExecutionSummary, PlanLinkSnapshot, PlanLinkStatus, PlanNodeResult, PlanNodeStatus,
    SubagentId, SubagentTaskId,
};

#[tokio::test(flavor = "current_thread")]
async fn scoped_updates_reject_stale_plan_and_node_revisions() {
    let (controller, mut events) = controller(None);
    let (scope, _owned_id, _sibling_id) = linked_scope(&controller).await;
    while events.try_recv().is_ok() {}
    let before = controller
        .snapshot()
        .await
        .expect("snapshot succeeds")
        .expect("active plan");

    let stale_plan_error = scope
        .update(SubagentPlanUpdateInput {
            reason: "reject stale scoped plan revision".to_owned(),
            change: SubagentPlanChangeInput::DefineChildren {
                expected_plan_revision: before.revision.saturating_sub(1),
                children: vec![node("stale-plan-child", "Stale plan child")],
            },
        })
        .await
        .expect_err("stale plan revision must reject");
    assert!(matches!(
        stale_plan_error,
        crate::plan::PlanControllerError::Plan {
            source: PlanError::StalePlanRevision { .. }
        }
    ));
    assert_eq!(
        controller
            .snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("active plan")
            .revision,
        before.revision
    );

    let defined = scope
        .update(SubagentPlanUpdateInput {
            reason: "define a stale revision target".to_owned(),
            change: SubagentPlanChangeInput::DefineChildren {
                expected_plan_revision: before.revision,
                children: vec![node("stale-node-target", "Stale node target")],
            },
        })
        .await
        .expect("target definition succeeds");
    while events.try_recv().is_ok() {}
    let target_id = defined.client_key_to_runtime_node_id["stale-node-target"].clone();
    let target = defined
        .snapshot
        .nodes
        .iter()
        .find(|node| node.id == target_id)
        .expect("target exists");
    let stale_node_error = scope
        .update(SubagentPlanUpdateInput {
            reason: "reject stale scoped node revision".to_owned(),
            change: SubagentPlanChangeInput::ReplaceSubtree {
                target_node_id: target_id,
                expected_node_revision: target.updated_revision.saturating_sub(1),
                subtree: replacement_for(target, "Stale node replacement"),
            },
        })
        .await
        .expect_err("stale node revision must reject");
    assert!(matches!(
        stale_node_error,
        crate::plan::PlanControllerError::Plan {
            source: PlanError::StaleNodeRevision { .. }
        }
    ));
    let after = controller
        .snapshot()
        .await
        .expect("snapshot succeeds")
        .expect("active plan");
    assert_eq!(after.revision, defined.snapshot.revision);
    assert!(events.try_recv().is_err(), "stale update emits no event");
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
        crate::plan::PlanControllerError::Plan {
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
        crate::plan::PlanControllerError::Plan {
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
        crate::plan::PlanControllerError::Plan {
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
    let target_id = defined.client_key_to_runtime_node_id["target"].clone();
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

#[tokio::test(flavor = "current_thread")]
async fn scoped_replace_preserves_existing_descendant_runtime_state() {
    let (controller, _events, session) = controller_with_session(None);
    let (scope, _owned_id, _sibling_id) = linked_scope(&controller).await;
    let defined = scope
        .update(SubagentPlanUpdateInput {
            reason: "define a runtime-tracked descendant".to_owned(),
            change: SubagentPlanChangeInput::DefineChildren {
                expected_plan_revision: 2,
                children: vec![node("runtime-target", "Runtime tracked target")],
            },
        })
        .await
        .expect("child definition succeeds");
    let target_id = defined.client_key_to_runtime_node_id["runtime-target"].clone();
    let runtime_result = PlanNodeResult {
        conclusion: "The previous runtime result remains authoritative".to_owned(),
        evidence_refs: Vec::new(),
        artifact_refs: Vec::new(),
        changed_paths: vec!["src/runtime.rs".to_owned()],
        verification: vec!["runtime check".to_owned()],
        open_questions: Vec::new(),
    };
    let runtime_summary = PlanExecutionSummary {
        active: 1,
        completed: 2,
        failed: 3,
        cancelled: 4,
        blocked: 5,
    };
    let runtime_link = PlanLinkSnapshot {
        plan_id: defined.snapshot.plan_id.clone(),
        node_id: target_id.clone(),
        binding_id: merry_core::PlanBindingId::new("historical-binding").expect("valid binding id"),
        subagent_id: SubagentId::new("historical-agent").expect("valid subagent id"),
        task_id: SubagentTaskId::new("historical-task").expect("valid task id"),
        status: PlanLinkStatus::Completed,
        linked_at_ms: 10,
        terminal_at_ms: Some(20),
        superseded_by: None,
    };
    {
        let mut session = session.lock().await;
        let plan = session.active_plan_mut().expect("active plan exists");
        let target = plan
            .snapshot
            .nodes
            .iter_mut()
            .find(|node| node.id == target_id)
            .expect("target exists");
        target.result = Some(runtime_result.clone());
        target.execution_summary = runtime_summary.clone();
        target.links = vec![runtime_link.clone()];
    }
    let current = controller
        .snapshot()
        .await
        .expect("snapshot succeeds")
        .expect("active plan")
        .nodes
        .into_iter()
        .find(|node| node.id == target_id)
        .expect("target remains present");

    let output = scope
        .update(SubagentPlanUpdateInput {
            reason: "revise without replacing runtime state".to_owned(),
            change: SubagentPlanChangeInput::ReplaceSubtree {
                target_node_id: target_id.clone(),
                expected_node_revision: current.updated_revision,
                subtree: replacement_for(&current, "Revised runtime tracked target"),
            },
        })
        .await
        .expect("scoped replacement succeeds");
    let target = output
        .snapshot
        .nodes
        .iter()
        .find(|node| node.id == target_id)
        .expect("target remains projected");
    assert_eq!(target.result, Some(runtime_result.clone()));
    assert_eq!(target.execution_summary, runtime_summary);
    assert!(
        target.links.is_empty(),
        "scoped view must hide links owned by another binding"
    );

    let full = controller
        .snapshot()
        .await
        .expect("full snapshot succeeds")
        .expect("active plan remains present");
    let target = full
        .nodes
        .iter()
        .find(|node| node.id == target_id)
        .expect("target remains in full snapshot");
    assert_eq!(target.result, Some(runtime_result));
    assert_eq!(target.execution_summary, runtime_summary);
    assert_eq!(target.links, vec![runtime_link]);
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

#[tokio::test(flavor = "current_thread")]
async fn omitted_status_deserializes_and_preserves_existing_declared_status() {
    let parsed: PlanNodeInput = serde_json::from_str(
        r#"{
                "client_key": "json-root",
                "objective": "JSON root",
                "acceptance": ["JSON root is verified"],
                "depends_on": [],
                "children": []
            }"#,
    )
    .expect("status omission is valid JSON input");
    assert_eq!(parsed.status, None);

    let (controller, _events) = controller(None);
    controller
        .begin(BeginPlanInput {
            reason: "preserve a declared status through scoped update".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("begin succeeds");
    let mut root = node("root", "Initial status");
    root.status = Some(PlanNodeStatus::Completed);
    let initial = controller
        .update(UpdatePlanInput {
            reason: "declare completed root".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root,
            },
        })
        .await
        .expect("initial status update succeeds");
    let root_id = initial.snapshot.root_node_id.clone().expect("root id");
    let link = controller
        .bind_subagent(
            "root".to_owned(),
            SubagentId::new("status-agent").expect("valid subagent id"),
            SubagentTaskId::new("status-task").expect("valid task id"),
            1,
        )
        .await
        .expect("binding succeeds");
    let scope = controller.subagent_scope(
        initial.snapshot.plan_id.clone(),
        root_id.clone(),
        link.binding_id,
    );
    let current = controller
        .snapshot()
        .await
        .expect("snapshot succeeds")
        .expect("active plan")
        .nodes
        .into_iter()
        .find(|node| node.id == root_id)
        .expect("root remains present");
    let output = scope
        .update(SubagentPlanUpdateInput {
            reason: "status omission must preserve declaration".to_owned(),
            change: SubagentPlanChangeInput::ReplaceSubtree {
                target_node_id: root_id.clone(),
                expected_node_revision: current.updated_revision,
                subtree: replacement_for(&current, current.objective.as_str()),
            },
        })
        .await
        .expect("omitted status keeps the current declaration");
    let root = output
        .snapshot
        .nodes
        .iter()
        .find(|node| node.id == root_id)
        .expect("root remains present");
    assert_eq!(root.declared_status, PlanNodeStatus::Completed);
    assert_eq!(root.status, PlanNodeStatus::InProgress);
}
