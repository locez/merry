use super::{controller, linked_scope, node};
use crate::plan::{PlanError, SubagentPlanChangeInput, SubagentPlanUpdateInput};
use merry_core::{PlanLinkStatus, SubagentId, SubagentTaskId};

#[tokio::test(flavor = "current_thread")]
async fn scoped_define_children_only_exposes_and_updates_bound_subtree() {
    let (controller, _events) = controller(None);
    let (scope, owned_id, sibling_id) = linked_scope(&controller).await;

    let child = node("child", "Child-owned work");
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
async fn scoped_read_and_update_require_an_active_binding() {
    for status in [
        PlanLinkStatus::Completed,
        PlanLinkStatus::Failed,
        PlanLinkStatus::Cancelled,
        PlanLinkStatus::Blocked,
        PlanLinkStatus::Superseded,
    ] {
        let (controller, mut events) = controller(None);
        let (scope, _owned_id, _sibling_id) = linked_scope(&controller).await;
        while events.try_recv().is_ok() {}

        let terminal = controller
            .update_subagent_link(scope.binding_id.clone(), status, 10)
            .await
            .expect("terminal link update succeeds");
        while events.try_recv().is_ok() {}

        let read_error = scope
            .read()
            .await
            .expect_err("terminal binding read must be rejected");
        assert!(matches!(
            read_error,
            crate::plan::PlanControllerError::Plan {
                source: PlanError::SubagentScopeViolation { .. }
            }
        ));

        let update_error = scope
            .update(SubagentPlanUpdateInput {
                reason: "reject update after linked work ended".to_owned(),
                change: SubagentPlanChangeInput::DefineChildren {
                    expected_plan_revision: terminal.revision,
                    children: vec![node("terminal-child", "Must not be installed")],
                },
            })
            .await
            .expect_err("terminal binding update must be rejected");
        assert!(matches!(
            update_error,
            crate::plan::PlanControllerError::Plan {
                source: PlanError::SubagentScopeViolation { .. }
            }
        ));

        let after = controller
            .snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("active plan remains available");
        assert_eq!(after.revision, terminal.revision);
        assert!(
            events.try_recv().is_err(),
            "rejected operations emit no event"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_projection_filters_links_to_the_current_binding() {
    let (controller, _events) = controller(None);
    let (scope, _owned_id, _sibling_id) = linked_scope(&controller).await;
    let defined = scope
        .update(SubagentPlanUpdateInput {
            reason: "define a linked descendant".to_owned(),
            change: SubagentPlanChangeInput::DefineChildren {
                expected_plan_revision: 2,
                children: vec![node("descendant", "Descendant work")],
            },
        })
        .await
        .expect("descendant definition succeeds");
    let descendant_id = defined.client_key_to_runtime_node_id["descendant"].clone();
    let other_link = controller
        .bind_subagent(
            "descendant".to_owned(),
            SubagentId::new("other-agent").expect("valid agent id"),
            SubagentTaskId::new("other-task").expect("valid task id"),
            11,
        )
        .await
        .expect("other binding succeeds");

    let projected = scope.read().await.expect("scoped read succeeds");
    let root = projected
        .nodes
        .iter()
        .find(|node| node.id == scope.root_node_id)
        .expect("scope root remains visible");
    assert!(root.links.iter().any(|link| {
        link.binding_id == scope.binding_id && link.status == PlanLinkStatus::Active
    }));
    let descendant = projected
        .nodes
        .iter()
        .find(|node| node.id == descendant_id)
        .expect("descendant remains visible");
    assert!(
        descendant
            .links
            .iter()
            .all(|link| link.binding_id != other_link.binding_id),
        "projection must hide links owned by another binding"
    );
}
