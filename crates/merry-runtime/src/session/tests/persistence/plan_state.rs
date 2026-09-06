use crate::{
    FileSessionStore, PlanError, PlanPersistenceLocation,
    plan::{
        ControlPlanAttemptInput, PlanChangeInput, PlanExecutionIntent, PlanNodeInput, PlanState,
        ReportPlanProgressInput, UpdatePlanInput, execution::PlanAttemptActor,
    },
    session::tests::{
        SessionState,
        persistence::{
            current_document, persisted_plan_leaf, persisted_plan_node_id, persisted_test_plan,
            session_id_with_suffix,
        },
        session_id,
    },
};
use merry_core::{
    PlanActivationSource, PlanAttemptOutcome, PlanDirectiveConstraints, PlanDirectiveKind,
    PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanNodeStatus, PlanRecoveryPolicySnapshot,
    PlanResourcePolicySnapshot,
};

#[tokio::test]
async fn session_state_current_format_without_plan_round_trips_current_format() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let document = current_document();
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("current document serializes"),
        )
        .await
        .expect("current state writes");

    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("current state loads without a plan");
    assert!(loaded.active_plan().is_none());
    loaded.save_to(&store).await.expect("current state saves");

    let rewritten: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("rewritten state reads"),
    )
    .expect("rewritten state is JSON");
    assert_eq!(rewritten["format_version"], 4);
    assert_eq!(rewritten["active_plan"], serde_json::Value::Null);
    assert_eq!(rewritten["terminal_plans"], serde_json::json!([]));
}

#[tokio::test]
async fn session_state_round_trip_preserves_active_plan_snapshot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    let plan = persisted_test_plan();
    let expected = plan.snapshot().clone();
    session.set_active_plan(plan);

    session.save_to(&store).await.expect("plan session saves");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("plan session loads");

    assert_eq!(
        loaded.active_plan().expect("active plan").snapshot(),
        &expected
    );
    assert!(loaded.terminal_plans().is_empty());
}

#[tokio::test]
async fn session_load_preserves_typed_plan_validation_error_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    session.set_active_plan(persisted_test_plan());
    session.save_to(&store).await.expect("plan session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is JSON");
    document["active_plan"]["snapshot"]["nodes"][0]["recovery_policy"]["max_transient_attempts"] =
        serde_json::json!(9);
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("corrupt state serializes"),
        )
        .await
        .expect("corrupt state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("invalid persisted retry policy must reject load");
    let message = error.to_string();
    assert!(matches!(
        error,
        crate::SessionStoreError::InvalidPlan {
            location: PlanPersistenceLocation::Active,
            source: PlanError::TooManyTransientAttempts {
                actual: 9,
                maximum: 8
            }
        }
    ));
    assert!(message.contains("active plan"));
    assert!(message.contains("9"));
}

#[tokio::test]
async fn session_load_preserves_oversized_plan_snapshot_error_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut session = SessionState::new(session_id());
    session.set_active_plan(persisted_test_plan());
    session.save_to(&store).await.expect("plan session saves");

    let mut document: serde_json::Value = serde_json::from_slice(
        &store
            .read_state_bytes(&session_id())
            .await
            .expect("state reads"),
    )
    .expect("state is JSON");
    document["active_plan"]["snapshot"]["execution_authorization_refs"] =
        serde_json::to_value(vec!["x".repeat(1024); 300]).expect("refs serialize");
    store
        .write_state_bytes(
            &session_id(),
            &serde_json::to_vec_pretty(&document).expect("corrupt state serializes"),
        )
        .await
        .expect("corrupt state writes");

    let error = SessionState::load_from(&store, &session_id())
        .await
        .expect_err("oversized persisted snapshot must reject load");
    match error {
        crate::SessionStoreError::InvalidPlan {
            location: PlanPersistenceLocation::Active,
            source: PlanError::SnapshotTooLarge { actual, maximum },
        } => {
            assert!(actual > maximum);
            assert_eq!(maximum, 256 * 1024);
        }
        other => panic!("unexpected persisted plan error: {other:?}"),
    }
}

#[tokio::test]
async fn session_state_round_trip_preserves_plan_attempt_lease_directive_recovery_and_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let mut plan = PlanState::empty(
        PlanId::new("complex-persisted-plan").expect("valid plan id"),
        PlanActivationSource::Coordinator {
            reason: "persist execution state".to_owned(),
            governing_skill_id: None,
        },
        PlanResourcePolicySnapshot::default(),
    );
    let update = plan
        .update(UpdatePlanInput {
            reason: "define a plan with live and recoverable work".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(2),
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: PlanNodeInput {
                    id: None,
                    client_key: Some("root".to_owned()),
                    objective: "Persist all execution records".to_owned(),
                    acceptance: vec!["records survive reload".to_owned()],
                    status: None,
                    executor_policy: PlanExecutorPolicy::Local,
                    harness: PlanHarnessSnapshot::default(),
                    recovery_policy: PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: vec![
                        persisted_plan_leaf("live-initial", "Keep live work"),
                        persisted_plan_leaf("expired-initial", "Recover expired work"),
                        persisted_plan_leaf("obsolete", "Obsolete work"),
                    ],
                },
            },
        })
        .expect("complex plan definition succeeds");
    let _initial_update = update;
    plan.update(UpdatePlanInput {
        reason: "supersede the obsolete authored history".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: plan.snapshot().revision,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root-current".to_owned()),
                objective: "Persist all current execution records".to_owned(),
                acceptance: vec!["current records survive reload".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Local,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: vec![
                    persisted_plan_leaf("live-current", "Keep live work"),
                    persisted_plan_leaf("expired-current", "Recover expired work"),
                ],
            },
        },
    })
    .expect("subtree replacement succeeds");
    plan.enter_execution(
        Default::default(),
        vec!["persist test authorization".to_owned()],
    )
    .expect("execution starts");

    let live_node_id = persisted_plan_node_id(&plan, "live-current");
    let expired_node_id = persisted_plan_node_id(&plan, "expired-current");
    let live_actor = PlanAttemptActor {
        executor_session_id: session_id_with_suffix("persist-live-executor"),
    };
    let live = plan
        .start_attempt(&live_node_id, live_actor.clone(), 10_000)
        .expect("live attempt starts");
    let live_directive = plan
        .issue_directive(
            ControlPlanAttemptInput {
                attempt_id: live.attempt.attempt_id.clone(),
                kind: PlanDirectiveKind::Steer,
                reason: "persist this live directive".to_owned(),
                instruction: Some("keep the current verification path".to_owned()),
                constraints: Some(PlanDirectiveConstraints::default()),
                requested_output: vec!["current checkpoint".to_owned()],
            },
            10_100,
        )
        .expect("live directive queues");
    plan.report_progress(
        &live_actor,
        ReportPlanProgressInput {
            summary: "live progress is durable".to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            next_action: Some("continue verification".to_owned()),
            checkpoint_ref: Some("persisted-checkpoint".to_owned()),
            acknowledged_directive_ids: vec![live_directive.directive.directive_id],
            applied_directive_ids: Vec::new(),
            request_coordinator_review: Some(true),
        },
        10_200,
    )
    .expect("live progress records");

    let expired_actor = PlanAttemptActor {
        executor_session_id: session_id_with_suffix("persist-expired-executor"),
    };
    let expired = plan
        .start_attempt(&expired_node_id, expired_actor, 2_000)
        .expect("recoverable attempt starts");
    plan.issue_directive(
        ControlPlanAttemptInput {
            attempt_id: expired.attempt.attempt_id,
            kind: PlanDirectiveKind::RequestStatus,
            reason: "persist this expiring directive".to_owned(),
            instruction: None,
            constraints: None,
            requested_output: Vec::new(),
        },
        2_100,
    )
    .expect("expiring directive queues");
    plan.interrupt_expired_leases(expired.lease.lease_expires_at_ms)
        .expect("expired lease is interrupted");

    let expected = plan.snapshot().clone();
    assert!(
        expected
            .attempts
            .iter()
            .any(|attempt| attempt.outcome.is_none())
    );
    assert!(
        expected
            .attempts
            .iter()
            .any(|attempt| attempt.outcome == Some(PlanAttemptOutcome::Interrupted))
    );
    assert!(
        expected
            .nodes
            .iter()
            .any(|node| node.status == PlanNodeStatus::Superseded)
    );
    assert!(
        expected
            .directives
            .iter()
            .any(|directive| directive.status == merry_core::PlanDirectiveStatus::Acknowledged)
    );
    assert!(
        expected
            .directives
            .iter()
            .any(|directive| directive.status == merry_core::PlanDirectiveStatus::Expired)
    );

    let mut session = SessionState::new(session_id());
    session.set_active_plan(plan);
    session.save_to(&store).await.expect("complex plan saves");
    let loaded = SessionState::load_from(&store, &session_id())
        .await
        .expect("complex plan loads");

    assert_eq!(
        loaded.active_plan().expect("active plan").snapshot(),
        &expected
    );
}
