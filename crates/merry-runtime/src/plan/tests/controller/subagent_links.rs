use crate::plan::{
    PlanChangeInput, PlanExecutionIntent, UpdatePlanInput,
    tests::controller::{controller, input, plan_leaf, plan_root},
};
use merry_core::{
    PlanCapabilityEnvelopeSnapshot, PlanLinkStatus, PlanNodeStatus, PlanPhase, SubagentId,
    SubagentTaskId,
};

#[tokio::test(flavor = "current_thread")]
async fn linked_subagent_lifecycle_updates_plan_execution_summary() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("bind real subagent work"))
        .await
        .expect("begin succeeds");
    controller
        .update(UpdatePlanInput {
            reason: "define linked task".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_leaf("root"),
            },
        })
        .await
        .expect("plan update succeeds");

    let link = controller
        .bind_subagent(
            "root".to_owned(),
            SubagentId::new("agent-1").expect("valid agent id"),
            SubagentTaskId::new("task-1").expect("valid task id"),
            10,
        )
        .await
        .expect("link binds");
    let active = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .unwrap();
    let node = active
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("root"));
    assert_eq!(node.unwrap().execution_summary.active, 1);

    let binding_id = link.binding_id.clone();
    controller
        .update_subagent_link(binding_id.clone(), PlanLinkStatus::Completed, 20)
        .await
        .expect("link completion commits");
    let completed = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .unwrap();
    assert_eq!(completed.phase, PlanPhase::Completed);
    let node = completed
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("root"));
    assert_eq!(node.unwrap().execution_summary.completed, 1);
    assert_eq!(node.unwrap().execution_summary.active, 0);
    assert!(
        node.unwrap().links.iter().any(|link| {
            link.binding_id == binding_id && link.status == PlanLinkStatus::Completed
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn completing_all_declared_children_completes_the_root_plan() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("complete the declared plan"))
        .await
        .expect("begin succeeds");
    controller
        .update(UpdatePlanInput {
            reason: "define parallel work and its acceptance".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(2),
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_root(vec![plan_leaf("left"), plan_leaf("right")]),
            },
        })
        .await
        .expect("plan update succeeds");
    controller
        .authorize_execution(
            PlanCapabilityEnvelopeSnapshot::default(),
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("execution authorization succeeds");

    let left = controller
        .bind_subagent(
            "left".to_owned(),
            SubagentId::new("agent-left").expect("valid agent id"),
            SubagentTaskId::new("task-left").expect("valid task id"),
            10,
        )
        .await
        .expect("left link binds");
    let right = controller
        .bind_subagent(
            "right".to_owned(),
            SubagentId::new("agent-right").expect("valid agent id"),
            SubagentTaskId::new("task-right").expect("valid task id"),
            11,
        )
        .await
        .expect("right link binds");

    controller
        .update_subagent_link(left.binding_id, PlanLinkStatus::Completed, 20)
        .await
        .expect("left completion commits");
    controller
        .update_subagent_link(right.binding_id, PlanLinkStatus::Completed, 21)
        .await
        .expect("right completion commits");

    let snapshot = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .expect("active plan exists");
    assert_eq!(snapshot.phase, PlanPhase::Completed);
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("root"))
        .expect("root node exists");
    assert_eq!(root.status, PlanNodeStatus::Completed);
    assert!(
        root.result.is_some(),
        "runtime should record root completion"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn rebinding_a_terminal_link_supersedes_stale_failure_and_reopens_plan() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("retry a failed linked child"))
        .await
        .expect("begin succeeds");
    controller
        .update(UpdatePlanInput {
            reason: "define retryable linked task".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_leaf("retryable"),
            },
        })
        .await
        .expect("plan update succeeds");

    let first = controller
        .bind_subagent(
            "retryable".to_owned(),
            SubagentId::new("agent-failed").expect("valid agent id"),
            SubagentTaskId::new("task-failed").expect("valid task id"),
            10,
        )
        .await
        .expect("first link binds");
    controller
        .update_subagent_link(first.binding_id.clone(), PlanLinkStatus::Failed, 20)
        .await
        .expect("first link failure commits");

    let second = controller
        .bind_subagent(
            "retryable".to_owned(),
            SubagentId::new("agent-retry").expect("valid agent id"),
            SubagentTaskId::new("task-retry").expect("valid task id"),
            30,
        )
        .await
        .expect("replacement link binds");
    let reopened = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .expect("active plan exists");
    let node = reopened
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("retryable"))
        .expect("retryable node exists");
    let old = node
        .links
        .iter()
        .find(|link| link.binding_id == first.binding_id)
        .expect("old link remains as history");
    assert_eq!(old.status, PlanLinkStatus::Superseded);
    assert_eq!(old.superseded_by, Some(second.binding_id.clone()));
    assert_eq!(node.execution_summary.failed, 0);
    assert_eq!(node.execution_summary.active, 1);
    assert_eq!(node.status, PlanNodeStatus::InProgress);
    assert_eq!(reopened.phase, PlanPhase::Executing);

    controller
        .update_subagent_link(second.binding_id, PlanLinkStatus::Completed, 40)
        .await
        .expect("replacement completion commits");
    let completed = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .expect("active plan exists");
    let node = completed
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("retryable"))
        .expect("retryable node exists");
    assert_eq!(node.execution_summary.failed, 0);
    assert_eq!(node.execution_summary.completed, 1);
    assert_eq!(completed.phase, PlanPhase::Completed);
}

#[tokio::test(flavor = "current_thread")]
async fn rebinding_a_blocked_link_reopens_plan_without_counting_failure() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("retry a blocked linked child"))
        .await
        .expect("begin succeeds");
    controller
        .update(UpdatePlanInput {
            reason: "define retryable blocked linked task".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_leaf("blocked-retryable"),
            },
        })
        .await
        .expect("plan update succeeds");

    let first = controller
        .bind_subagent(
            "blocked-retryable".to_owned(),
            SubagentId::new("agent-blocked").expect("valid agent id"),
            SubagentTaskId::new("task-blocked").expect("valid task id"),
            10,
        )
        .await
        .expect("first link binds");
    controller
        .update_subagent_link(first.binding_id.clone(), PlanLinkStatus::Blocked, 20)
        .await
        .expect("blocked link update commits");

    let blocked = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .expect("active plan exists");
    let node = blocked
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("blocked-retryable"))
        .expect("blocked node exists");
    assert_eq!(node.status, PlanNodeStatus::Blocked);
    assert_eq!(node.execution_summary.blocked, 1);
    assert_eq!(node.execution_summary.failed, 0);
    assert_eq!(blocked.phase, PlanPhase::Blocked);

    let second = controller
        .bind_subagent(
            "blocked-retryable".to_owned(),
            SubagentId::new("agent-retry-blocked").expect("valid agent id"),
            SubagentTaskId::new("task-retry-blocked").expect("valid task id"),
            30,
        )
        .await
        .expect("replacement link binds");
    let reopened = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .expect("active plan exists");
    let node = reopened
        .nodes
        .iter()
        .find(|node| node.client_key.as_deref() == Some("blocked-retryable"))
        .expect("retryable node exists");
    assert_eq!(node.execution_summary.blocked, 0);
    assert_eq!(node.execution_summary.active, 1);
    assert_eq!(node.status, PlanNodeStatus::InProgress);
    assert_eq!(reopened.phase, PlanPhase::Executing);
    assert!(node.links.iter().any(|link| {
        link.binding_id == first.binding_id
            && link.status == PlanLinkStatus::Superseded
            && link.superseded_by == Some(second.binding_id.clone())
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn linked_subagent_bindings_are_unique_across_plan_nodes() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("bind multiple subagents"))
        .await
        .expect("begin succeeds");
    controller
        .update(UpdatePlanInput {
            reason: "define multiple linked tasks".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_root(vec![plan_leaf("left"), plan_leaf("right")]),
            },
        })
        .await
        .expect("plan update succeeds");
    controller
        .authorize_execution(
            PlanCapabilityEnvelopeSnapshot::default(),
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("execution authorization succeeds");

    let left = controller
        .bind_subagent(
            "left".to_owned(),
            SubagentId::new("agent-left").expect("valid agent id"),
            SubagentTaskId::new("task-left").expect("valid task id"),
            10,
        )
        .await
        .expect("left link binds");
    let right = controller
        .bind_subagent(
            "right".to_owned(),
            SubagentId::new("agent-right").expect("valid agent id"),
            SubagentTaskId::new("task-right").expect("valid task id"),
            11,
        )
        .await
        .expect("right link binds");

    assert_ne!(
        left.binding_id, right.binding_id,
        "binding ids must identify links across the whole plan"
    );

    controller
        .update_subagent_link(left.binding_id, PlanLinkStatus::Completed, 20)
        .await
        .expect("left link completion commits");
    controller
        .update_subagent_link(right.binding_id, PlanLinkStatus::Completed, 21)
        .await
        .expect("right link completion commits");

    let snapshot = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .expect("active plan exists");
    for client_key in ["left", "right"] {
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.client_key.as_deref() == Some(client_key))
            .expect("linked node exists");
        assert_eq!(
            node.execution_summary.active, 0,
            "{client_key} remains active"
        );
        assert_eq!(
            node.execution_summary.completed, 1,
            "{client_key} did not complete"
        );
        assert_eq!(node.links[0].status, PlanLinkStatus::Completed);
        assert!(node.links[0].terminal_at_ms.is_some());
    }
    assert_eq!(snapshot.phase, PlanPhase::Completed);
}
