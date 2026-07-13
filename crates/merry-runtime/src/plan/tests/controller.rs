use crate::{
    FileSessionStore,
    plan::{
        BeginPlanInput, PlanChangeInput, PlanController, PlanControllerError, PlanExecutionIntent,
        PlanNodeInput, ReportPlanAttemptInput, UpdatePlanInput,
        controller::PlanControllerEventReceiver, execution::PlanAttemptActor,
    },
    session::SessionState,
    session_store::{SessionStoreCommitPause, SessionStoreStagePause},
};
use merry_core::{
    PlanAttemptOutcome, PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanNodeResult, PlanNodeStatus, PlanPhase, PlanRecoveryPolicySnapshot, RuntimeJournalPayload,
    SessionId,
};
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

#[tokio::test(flavor = "current_thread")]
async fn plan_commit_rebases_its_sequence_frontier_when_session_activity_arrives_during_staging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pause = SessionStoreStagePause::new();
    let store = FileSessionStore::new(temp.path()).with_stage_pause_for_tests(pause.clone());
    let session = Arc::new(Mutex::new(SessionState::new(session_id())));
    let (controller, mut events) = PlanController::start(
        Arc::clone(&session),
        Some(store.clone()),
        NonZeroUsize::new(16).expect("non-zero buffer"),
    );
    let task = tokio::spawn({
        let controller = controller.clone();
        async move {
            controller
                .begin(input("rebase concurrent session activity"))
                .await
        }
    });

    pause.wait_until_staged().await;
    let unrelated =
        session
            .lock()
            .await
            .record_transient_event(RuntimeJournalPayload::AssistantOutputDelta {
                delta: "root model is still producing output".to_owned(),
            });
    pause.resume();
    task.await
        .expect("begin task joins")
        .expect("plan commit retries with the new sequence frontier");

    let plan_event = events.recv().await.expect("committed plan event");
    assert_ne!(plan_event.sequence, unrelated.sequence);
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("plan sidecar resumes");
    assert!(loaded.active_plan().is_some());
    assert_eq!(loaded.next_sequence(), session.lock().await.next_sequence());
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_attempt_reports_are_serialized_without_lost_updates() {
    let (controller, _events) = controller(None);
    controller
        .begin(input("concurrent execution"))
        .await
        .expect("begin succeeds");
    let update = controller
        .update(UpdatePlanInput {
            reason: "define parallel leaves".to_owned(),
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
    let left_actor = PlanAttemptActor {
        executor_session_id: SessionId::new("worker-left").unwrap(),
    };
    let right_actor = PlanAttemptActor {
        executor_session_id: SessionId::new("worker-right").unwrap(),
    };
    let left = controller
        .start_attempt(
            update.client_key_ids["left"].clone(),
            left_actor.clone(),
            10,
        )
        .await
        .expect("left starts")
        .output;
    let right = controller
        .start_attempt(
            update.client_key_ids["right"].clone(),
            right_actor.clone(),
            10,
        )
        .await
        .expect("right starts")
        .output;

    let (left_report, right_report) = tokio::join!(
        controller.attempt_report(
            left_actor,
            completed_report(&left.lease, "left complete"),
            20,
        ),
        controller.attempt_report(
            right_actor,
            completed_report(&right.lease, "right complete"),
            20,
        ),
    );
    left_report.expect("left report commits");
    right_report.expect("right report commits");

    let snapshot = controller.snapshot().await.unwrap().unwrap();
    for key in ["left", "right"] {
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .find(|node| node.id == update.client_key_ids[key])
                .expect("parallel node exists")
                .status,
            PlanNodeStatus::Completed
        );
    }
    assert_eq!(
        snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.outcome.is_some())
            .count(),
        2
    );
}

fn plan_leaf(client_key: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: format!("Complete {client_key}"),
        acceptance: vec![format!("{client_key} verified")],
        executor_policy: PlanExecutorPolicy::Delegate,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children: Vec::new(),
    }
}

fn plan_root(children: Vec<PlanNodeInput>) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some("root".to_owned()),
        objective: "Complete all work".to_owned(),
        acceptance: vec!["all leaves verified".to_owned()],
        executor_policy: PlanExecutorPolicy::Local,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children,
    }
}

fn completed_report(
    lease: &merry_core::PlanLeaseSnapshot,
    conclusion: &str,
) -> ReportPlanAttemptInput {
    ReportPlanAttemptInput {
        lease_id: lease.lease_id.clone(),
        expected_node_revision: lease.node_revision,
        outcome: PlanAttemptOutcome::Completed,
        result: Some(PlanNodeResult {
            conclusion: conclusion.to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            changed_paths: Vec::new(),
            verification: vec!["test verification".to_owned()],
            open_questions: Vec::new(),
        }),
        diagnostic: None,
        decomposition: None,
        acknowledged_directive_ids: Vec::new(),
        applied_directive_ids: Vec::new(),
    }
}
