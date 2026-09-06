use super::{controller, controller_with_session, linked_scope, node, session_id};
use crate::FileSessionStore;
use crate::plan::{PlanController, SubagentPlanChangeInput, SubagentPlanUpdateInput};
use crate::session::SessionState;
use merry_core::RuntimeJournalPayload;
use std::num::NonZeroUsize;
use std::sync::Arc;

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

#[tokio::test(flavor = "current_thread")]
async fn scoped_update_persistence_failure_keeps_memory_disk_and_events_unchanged() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let (controller, mut setup_events, session) = controller_with_session(Some(store.clone()));
    let (scope, _owned_id, _sibling_id) = linked_scope(&controller).await;
    while setup_events.try_recv().is_ok() {}
    let before = controller
        .snapshot()
        .await
        .expect("snapshot succeeds")
        .expect("active plan");
    let failing_store = store.clone().with_commit_failure_for_tests();
    let (failing_controller, mut events) = PlanController::start(
        Arc::clone(&session),
        Some(failing_store),
        NonZeroUsize::new(16).expect("non-zero buffer"),
    );
    let failing_scope = failing_controller.subagent_scope(
        scope.plan_id.clone(),
        scope.root_node_id.clone(),
        scope.binding_id.clone(),
    );

    let error = failing_scope
        .update(SubagentPlanUpdateInput {
            reason: "force scoped persistence failure".to_owned(),
            change: SubagentPlanChangeInput::DefineChildren {
                expected_plan_revision: before.revision,
                children: vec![node("not-persisted", "Must not be installed")],
            },
        })
        .await
        .expect_err("failed scoped commit must reject");
    assert!(matches!(
        error,
        crate::plan::PlanControllerError::SessionStore { .. }
    ));
    let in_memory = failing_controller
        .snapshot()
        .await
        .expect("snapshot succeeds")
        .expect("active plan remains installed");
    assert_eq!(in_memory.revision, before.revision);
    assert!(
        !in_memory
            .nodes
            .iter()
            .any(|node| node.client_key.as_deref() == Some("not-persisted"))
    );
    assert!(events.try_recv().is_err(), "failed update emits no event");

    let persisted = SessionState::load_from(&store, &session_id())
        .await
        .expect("persisted session loads");
    let persisted_plan = persisted.active_plan().expect("persisted active plan");
    assert_eq!(persisted_plan.snapshot().revision, before.revision);
    assert!(
        !persisted_plan
            .snapshot()
            .nodes
            .iter()
            .any(|node| node.client_key.as_deref() == Some("not-persisted"))
    );
}
