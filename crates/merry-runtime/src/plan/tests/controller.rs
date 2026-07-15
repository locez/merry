use crate::{
    FileSessionStore,
    plan::{
        BeginPlanInput, PlanChangeInput, PlanController, PlanControllerError, PlanError,
        PlanExecutionIntent, PlanNodeInput, ReportPlanAttemptInput, UpdatePlanInput,
        controller::PlanControllerEventReceiver, execution::PlanAttemptActor,
    },
    session::SessionState,
    session_store::{SessionStoreCommitPause, SessionStoreStagePause},
};
use merry_core::{
    PlanAttemptOutcome, PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanLinkStatus, PlanNodeResult, PlanNodeStatus, PlanPhase, PlanRecoveryPolicySnapshot,
    RuntimeJournalPayload, SessionId, SubagentId, SubagentTaskId,
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
    let delegated_id = initial.client_key_ids["delegated"].clone();
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

    let local_id = initial.client_key_ids["local"].clone();
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

    let revision = snapshot.revision;
    let updated = controller
        .update(UpdatePlanInput {
            reason: "continue after linked children completed".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::UseCurrentPlan {
                expected_plan_revision: revision,
            },
        })
        .await
        .expect("follow-up plan update succeeds after linked children complete");
    assert_eq!(updated.snapshot.revision, revision + 1);
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
        executor_session_id: SessionId::new("subagent-left").unwrap(),
    };
    let right_actor = PlanAttemptActor {
        executor_session_id: SessionId::new("subagent-right").unwrap(),
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
        status: None,
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
        status: None,
        executor_policy: PlanExecutorPolicy::Local,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children,
    }
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
            .map(|id| crate::plan::PlanNodeReferenceInput::Id { id })
            .collect(),
        children: Vec::new(),
    }
}

fn completed_report(
    _lease: &merry_core::PlanLeaseSnapshot,
    conclusion: &str,
) -> ReportPlanAttemptInput {
    ReportPlanAttemptInput {
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
