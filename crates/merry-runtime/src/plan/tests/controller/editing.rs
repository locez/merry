use crate::plan::{
    PlanChangeInput, PlanControllerError, PlanError, PlanExecutionIntent, UpdatePlanInput,
    tests::controller::{controller, input, plan_leaf, plan_root, replacement_for},
};
use merry_core::{
    PlanCapabilityEnvelopeSnapshot, PlanLinkStatus, PlanPhase, RuntimeJournalPayload, SubagentId,
    SubagentTaskId,
};

#[tokio::test(flavor = "current_thread")]
async fn concurrent_begin_requests_share_one_active_plan() {
    let (controller, mut events) = controller(None);
    let (first, second) = tokio::join!(
        controller.begin(input("first activation")),
        controller.begin(input("second activation")),
    );
    let first = first.expect("first begin succeeds");
    let second = second.expect("second begin is idempotent");

    assert_eq!(first.plan_id, second.plan_id);
    assert_eq!(first.phase, PlanPhase::Planning);
    assert_eq!(
        controller.snapshot().await.unwrap().unwrap().plan_id,
        first.plan_id
    );

    let first_event = events.recv().await.expect("plan event");
    assert!(matches!(
        first_event.payload,
        RuntimeJournalPayload::PlanUpdated { .. }
    ));
    assert!(
        events.try_recv().is_err(),
        "idempotent begin emits no second update"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn define_plan_update_creates_a_new_plan_without_targeting_a_node() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("complete before rerun"))
        .await
        .expect("begin succeeds");
    controller
        .update(UpdatePlanInput {
            reason: "define one completed task".to_owned(),
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
    controller
        .authorize_execution(
            PlanCapabilityEnvelopeSnapshot::default(),
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("execution authorization succeeds");
    let link = controller
        .bind_subagent(
            "root".to_owned(),
            SubagentId::new("agent").expect("valid agent id"),
            SubagentTaskId::new("task").expect("valid task id"),
            10,
        )
        .await
        .expect("link binds");
    let completed = controller
        .update_subagent_link(link.binding_id, PlanLinkStatus::Completed, 20)
        .await
        .expect("completion commits");

    let restarted = controller
        .update(UpdatePlanInput {
            reason: "run the request again from the beginning".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: completed.revision,
                root: plan_leaf("rerun-root"),
            },
        })
        .await
        .expect("define_plan restart succeeds");
    assert_ne!(restarted.snapshot.plan_id, completed.plan_id);
    assert_eq!(restarted.snapshot.phase, PlanPhase::Planning);
    assert!(
        restarted
            .snapshot
            .nodes
            .iter()
            .any(|node| node.client_key.as_deref() == Some("rerun-root"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn define_plan_update_does_not_discard_live_linked_work() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("protect active work during rerun"))
        .await
        .expect("begin succeeds");
    controller
        .update(UpdatePlanInput {
            reason: "define active work".to_owned(),
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
    controller
        .authorize_execution(
            PlanCapabilityEnvelopeSnapshot::default(),
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("execution authorization succeeds");
    controller
        .bind_subagent(
            "root".to_owned(),
            SubagentId::new("active-agent").expect("valid agent id"),
            SubagentTaskId::new("active-task").expect("valid task id"),
            10,
        )
        .await
        .expect("link binds");

    let error = controller
        .update(UpdatePlanInput {
            reason: "do not interrupt active work".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 999,
                root: plan_leaf("replacement"),
            },
        })
        .await
        .expect_err("define_plan replacement must wait for live work");
    assert!(matches!(
        error,
        PlanControllerError::Plan {
            source: PlanError::ActiveAttemptsPreventControl {
                operation: "replace active plan"
            }
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn coordinator_cannot_replace_an_active_linked_node_or_its_ancestor() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("protect active linked ownership"))
        .await
        .expect("begin succeeds");
    let initial = controller
        .update(UpdatePlanInput {
            reason: "define a delegated branch and a local sibling".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_root(vec![plan_leaf("delegated"), plan_leaf("local")]),
            },
        })
        .await
        .expect("initial plan succeeds");
    let delegated_id = initial.client_key_to_runtime_node_id["delegated"].clone();
    let root_id = initial.snapshot.root_node_id.clone().expect("root id");
    let binding_id = controller
        .bind_subagent(
            "delegated".to_owned(),
            SubagentId::new("agent-guard").expect("valid agent id"),
            SubagentTaskId::new("task-guard").expect("valid task id"),
            10,
        )
        .await
        .expect("binding succeeds")
        .binding_id;

    let delegated = initial
        .snapshot
        .nodes
        .iter()
        .find(|node| node.id == delegated_id)
        .expect("delegated node exists");
    let error = controller
        .update(UpdatePlanInput {
            reason: "attempt to rewrite active delegated work".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: delegated_id.clone(),
                expected_node_revision: delegated.updated_revision,
                subtree: replacement_for(delegated, "Rewritten delegated work"),
            },
        })
        .await
        .expect_err("active linked node must be protected");
    assert!(matches!(
        error,
        PlanControllerError::Plan {
            source: PlanError::ActiveSubagentOwnsSubtree { node_id }
        } if node_id == delegated_id
    ));

    let root = initial
        .snapshot
        .nodes
        .iter()
        .find(|node| node.id == root_id)
        .expect("root exists");
    let error = controller
        .update(UpdatePlanInput {
            reason: "attempt to replace the whole tree while child is active".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: root_id,
                expected_node_revision: root.updated_revision,
                subtree: replacement_for(root, "Rewritten whole plan"),
            },
        })
        .await
        .expect_err("ancestor replacement must be protected");
    assert!(matches!(
        error,
        PlanControllerError::Plan {
            source: PlanError::ActiveSubagentOwnsSubtree { .. }
        }
    ));

    let linked = controller
        .snapshot()
        .await
        .expect("snapshot reads")
        .expect("active plan exists")
        .nodes
        .into_iter()
        .find(|node| node.id == delegated_id)
        .expect("linked node remains");
    assert!(linked.links.iter().any(|snapshot| {
        snapshot.binding_id == binding_id && snapshot.status == PlanLinkStatus::Active
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn coordinator_can_revise_an_unrelated_sibling_while_linked_child_is_active() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("allow unrelated work during delegation"))
        .await
        .expect("begin succeeds");
    let initial = controller
        .update(UpdatePlanInput {
            reason: "define delegated and local work".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_root(vec![plan_leaf("delegated"), plan_leaf("local")]),
            },
        })
        .await
        .expect("initial plan succeeds");
    controller
        .bind_subagent(
            "delegated".to_owned(),
            SubagentId::new("agent-sibling").expect("valid agent id"),
            SubagentTaskId::new("task-sibling").expect("valid task id"),
            10,
        )
        .await
        .expect("binding succeeds");

    let local_id = initial.client_key_to_runtime_node_id["local"].clone();
    let local = initial
        .snapshot
        .nodes
        .iter()
        .find(|node| node.id == local_id)
        .expect("local node exists");
    let output = controller
        .update(UpdatePlanInput {
            reason: "revise only local work".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: local_id.clone(),
                expected_node_revision: local.updated_revision,
                subtree: replacement_for(local, "Revised local work"),
            },
        })
        .await
        .expect("unrelated local branch remains mutable");
    assert_eq!(
        output
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == local_id)
            .expect("revised local node remains")
            .objective,
        "Revised local work"
    );
}
