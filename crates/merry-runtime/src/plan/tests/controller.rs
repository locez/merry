use crate::{
    FileSessionStore,
    plan::{
        BeginPlanInput, PlanController, PlanControllerError,
        controller::PlanControllerEventReceiver,
    },
    session::SessionState,
    session_store::SessionStoreCommitPause,
};
use merry_core::{PlanPhase, RuntimeJournalPayload, SessionId};
use std::{num::NonZeroUsize, sync::Arc};
use tokio::sync::Mutex;

fn session_id() -> SessionId {
    SessionId::new("plan-controller-test").expect("valid session id")
}

fn input(reason: &str) -> BeginPlanInput {
    BeginPlanInput {
        reason: reason.to_owned(),
        governing_skill_id: None,
    }
}

fn controller(store: Option<FileSessionStore>) -> (PlanController, PlanControllerEventReceiver) {
    PlanController::start(
        Arc::new(Mutex::new(SessionState::new(session_id()))),
        store,
        NonZeroUsize::new(16).expect("non-zero buffer"),
    )
}

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
async fn persistence_failure_leaves_active_plan_uninstalled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path()).with_commit_failure_for_tests();
    let (controller, mut events) = controller(Some(store));

    let error = controller
        .begin(input("persisted activation"))
        .await
        .expect_err("commit failure must reject activation");

    assert!(matches!(error, PlanControllerError::SessionStore { .. }));
    assert!(controller.snapshot().await.unwrap().is_none());
    assert!(events.try_recv().is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn plan_event_waits_for_directory_durability() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pause = SessionStoreCommitPause::new();
    let store = FileSessionStore::new(temp.path()).with_commit_pause_for_tests(pause.clone());
    let (controller, mut events) = controller(Some(store));
    let task = tokio::spawn({
        let controller = controller.clone();
        async move { controller.begin(input("durable activation")).await }
    });

    pause.wait_until_committed().await;
    assert!(events.try_recv().is_err());
    pause.resume();
    task.await
        .expect("begin task joins")
        .expect("begin succeeds after directory durability");

    assert!(matches!(
        events.recv().await.expect("durable plan event").payload,
        RuntimeJournalPayload::PlanUpdated { .. }
    ));
}
