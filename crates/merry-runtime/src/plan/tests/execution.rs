use crate::plan::{
    ControlPlanAttemptInput, PlanChangeInput, PlanDecompositionInput, PlanExecutionIntent,
    PlanNodeInput, PlanNodeReferenceInput, ReportPlanAttemptInput, ReportPlanProgressInput,
    UpdatePlanInput, domain::PlanState, execution::PlanAttemptActor,
};
use merry_core::{
    ErrorInfo, PlanActivationSource, PlanAttemptOutcome, PlanCapabilityEnvelopeSnapshot,
    PlanDirectiveConstraints, PlanDirectiveKind, PlanDirectiveStatus, PlanExecutorPolicy,
    PlanHarnessSnapshot, PlanId, PlanNodeResult, PlanNodeStatus, PlanRecoveryPolicySnapshot,
    PlanResourcePolicySnapshot, SessionId,
};

fn leaf(client_key: &str, objective: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: objective.to_owned(),
        acceptance: vec![format!("{objective} accepted")],
        status: None,
        executor_policy: PlanExecutorPolicy::Delegate,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children: Vec::new(),
    }
}

fn root(children: Vec<PlanNodeInput>) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some("root".to_owned()),
        objective: "Complete the plan".to_owned(),
        acceptance: vec!["all work verified".to_owned()],
        status: None,
        executor_policy: PlanExecutorPolicy::Local,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children,
    }
}

fn executing_plan(
    root: PlanNodeInput,
) -> (
    PlanState,
    std::collections::BTreeMap<String, merry_core::PlanNodeId>,
) {
    let mut plan = PlanState::empty(
        PlanId::new("plan-execution").expect("valid plan id"),
        PlanActivationSource::Coordinator {
            reason: "test execution".to_owned(),
            governing_skill_id: None,
        },
        PlanResourcePolicySnapshot::default(),
    );
    let output = plan
        .update(UpdatePlanInput {
            reason: "define execution plan".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(3),
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root,
            },
        })
        .expect("plan definition succeeds");
    plan.enter_execution(
        PlanCapabilityEnvelopeSnapshot::default(),
        vec!["test".to_owned()],
    )
    .expect("execution authorization succeeds");
    (plan, output.client_key_ids)
}

fn actor(name: &str) -> PlanAttemptActor {
    PlanAttemptActor {
        executor_session_id: SessionId::new(name).expect("valid executor session id"),
    }
}

fn completed_result(conclusion: &str) -> PlanNodeResult {
    PlanNodeResult {
        conclusion: conclusion.to_owned(),
        evidence_refs: Vec::new(),
        artifact_refs: Vec::new(),
        changed_paths: Vec::new(),
        verification: vec!["deterministic test passed".to_owned()],
        open_questions: Vec::new(),
    }
}

#[test]
fn ready_set_is_deterministic_and_dependency_completion_releases_next_leaf() {
    let first = leaf("first", "First");
    let mut second = leaf("second", "Second");
    second.depends_on = vec![PlanNodeReferenceInput::ClientKey {
        client_key: "first".to_owned(),
    }];
    let third = leaf("third", "Third");
    let (mut plan, ids) = executing_plan(root(vec![first, second, third]));

    assert_eq!(
        plan.ready_node_ids(),
        [ids["first"].clone(), ids["third"].clone()]
    );
    let subagent = actor("subagent-first");
    let _started = plan
        .start_attempt(&ids["first"], subagent.clone(), 1_000)
        .expect("first attempt starts");
    let report = plan
        .report_attempt(
            &subagent,
            ReportPlanAttemptInput {
                outcome: PlanAttemptOutcome::Completed,
                result: Some(completed_result("first complete")),
                diagnostic: None,
                decomposition: None,
                acknowledged_directive_ids: Vec::new(),
                applied_directive_ids: Vec::new(),
            },
            2_000,
        )
        .expect("first attempt completes");

    assert!(report.ready_node_ids.contains(&ids["second"]));
    assert!(report.ready_node_ids.contains(&ids["third"]));
}

#[test]
fn multiple_progress_reports_stay_inside_one_attempt_and_apply_directive_explicitly() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Long running work")]));
    let subagent = actor("subagent-progress");
    let started = plan
        .start_attempt(&ids["work"], subagent.clone(), 1_000)
        .expect("attempt starts");
    let directive = plan
        .issue_directive(
            ControlPlanAttemptInput {
                attempt_id: started.attempt.attempt_id.clone(),
                kind: PlanDirectiveKind::Converge,
                reason: "Enough evidence has been collected".to_owned(),
                instruction: Some("Finish the current verification path".to_owned()),
                constraints: Some(PlanDirectiveConstraints {
                    allow_decomposition: false,
                    ..PlanDirectiveConstraints::default()
                }),
                requested_output: vec!["terminal verification summary".to_owned()],
            },
            1_500,
        )
        .expect("directive is queued");

    let first_progress = plan
        .report_progress(
            &subagent,
            ReportPlanProgressInput {
                summary: "Collected the primary evidence".to_owned(),
                evidence_refs: Vec::new(),
                artifact_refs: Vec::new(),
                next_action: Some("Run final verification".to_owned()),
                checkpoint_ref: Some("checkpoint-a".to_owned()),
                acknowledged_directive_ids: vec![directive.directive.directive_id.clone()],
                applied_directive_ids: Vec::new(),
                request_coordinator_review: Some(false),
            },
            2_000,
        )
        .expect("first progress commits");
    assert_eq!(
        first_progress.updated_directives[0].status,
        PlanDirectiveStatus::Acknowledged
    );

    let second_progress = plan
        .report_progress(
            &subagent,
            ReportPlanProgressInput {
                summary: "Final verification is in progress".to_owned(),
                evidence_refs: Vec::new(),
                artifact_refs: Vec::new(),
                next_action: Some("Return the result".to_owned()),
                checkpoint_ref: None,
                acknowledged_directive_ids: Vec::new(),
                applied_directive_ids: vec![directive.directive.directive_id],
                request_coordinator_review: Some(false),
            },
            3_000,
        )
        .expect("second progress commits");

    assert_eq!(plan.snapshot().attempts.len(), 1);
    assert_eq!(
        second_progress.updated_directives[0].status,
        PlanDirectiveStatus::Applied
    );
    assert_eq!(
        plan.snapshot().attempts[0].last_applied_directive_sequence,
        directive.directive.sequence
    );
}

#[test]
fn queued_directive_is_delivered_once_at_an_explicit_subagent_boundary() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Steered work")]));
    let subagent = actor("subagent-delivery");
    let started = plan
        .start_attempt(&ids["work"], subagent.clone(), 1_000)
        .expect("attempt starts");
    let queued = plan
        .issue_directive(
            ControlPlanAttemptInput {
                attempt_id: started.attempt.attempt_id.clone(),
                kind: PlanDirectiveKind::RequestStatus,
                reason: "Coordinator needs a bounded status update".to_owned(),
                instruction: None,
                constraints: None,
                requested_output: vec!["current evidence and next action".to_owned()],
            },
            1_500,
        )
        .expect("directive is queued");
    assert_eq!(queued.directive.status, PlanDirectiveStatus::Queued);

    let delivered = plan
        .deliver_queued_directives(&subagent, &started.lease.lease_id, 2_000)
        .expect("safe-boundary delivery succeeds");
    assert_eq!(delivered.updated_directives.len(), 1);
    assert_eq!(
        delivered.updated_directives[0].status,
        PlanDirectiveStatus::Delivered
    );
    assert_eq!(delivered.updated_directives[0].delivered_at_ms, Some(2_000));

    let revision = delivered.snapshot.revision;
    let repeated = plan
        .deliver_queued_directives(&subagent, &started.lease.lease_id, 2_500)
        .expect("repeated delivery is idempotent");
    assert!(repeated.updated_directives.is_empty());
    assert_eq!(repeated.snapshot.revision, revision);
}

#[test]
fn expired_lease_is_interrupted_and_requeued_without_replaying_directives() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Recoverable work")]));
    let subagent = actor("subagent-expired");
    let started = plan
        .start_attempt(&ids["work"], subagent, 1_000)
        .expect("attempt starts");
    let directive = plan
        .issue_directive(
            ControlPlanAttemptInput {
                attempt_id: started.attempt.attempt_id.clone(),
                kind: PlanDirectiveKind::RequestStatus,
                reason: "Check subagent liveness".to_owned(),
                instruction: None,
                constraints: None,
                requested_output: Vec::new(),
            },
            2_000,
        )
        .expect("directive queues");

    let before_expiry = plan
        .interrupt_expired_leases(started.lease.lease_expires_at_ms - 1)
        .expect("pre-expiry review succeeds");
    assert!(before_expiry.interrupted_attempts.is_empty());

    let recovered = plan
        .interrupt_expired_leases(started.lease.lease_expires_at_ms)
        .expect("expired lease is recovered");
    assert_eq!(recovered.interrupted_attempts.len(), 1);
    assert_eq!(
        recovered.interrupted_attempts[0].outcome,
        Some(PlanAttemptOutcome::Interrupted)
    );
    assert_eq!(
        recovered.snapshot.leases[0].status,
        merry_core::PlanLeaseStatus::Expired
    );
    assert_eq!(
        recovered.snapshot.directives[0].directive_id,
        directive.directive.directive_id
    );
    assert_eq!(
        recovered.snapshot.directives[0].status,
        PlanDirectiveStatus::Expired
    );
    assert_eq!(
        recovered
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == ids["work"])
            .expect("work node remains")
            .status,
        PlanNodeStatus::Pending
    );
    assert_eq!(recovered.ready_node_ids, [ids["work"].clone()]);

    let replacement = plan
        .start_attempt(&ids["work"], actor("subagent-retry"), 40_000)
        .expect("interrupted work receives a fresh attempt");
    assert_ne!(replacement.attempt.attempt_id, started.attempt.attempt_id);
}

#[test]
fn expired_lease_scan_does_not_interrupt_a_local_attempt() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Long local work")]));
    let coordinator = actor("coordinator-local");
    let started = plan
        .start_local_attempt(&ids["work"], coordinator, 1_000)
        .expect("local attempt starts");

    let scanned = plan
        .interrupt_expired_leases(u64::MAX)
        .expect("lease scan succeeds");

    assert!(scanned.interrupted_attempts.is_empty());
    assert!(scanned.snapshot.leases.is_empty());
    assert_eq!(
        scanned.snapshot.attempts[0].attempt_id,
        started.attempt.attempt_id
    );
    assert_eq!(scanned.snapshot.attempts[0].outcome, None);
    assert_eq!(
        scanned
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == ids["work"])
            .expect("local node remains")
            .status,
        PlanNodeStatus::InProgress
    );
}

#[test]
fn elapsed_time_requests_review_at_a_safe_boundary_without_cancelling_the_attempt() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Slow valid work")]));
    let subagent = actor("subagent-review");
    let started = plan
        .start_attempt(&ids["work"], subagent.clone(), 1_000)
        .expect("attempt starts");
    let window = plan
        .snapshot()
        .resource_policy_snapshot
        .no_durable_progress_review_window_ms;

    let early = plan
        .review_progress_at_boundary(&subagent, &started.lease.lease_id, 1_000 + window - 1)
        .expect("early review check succeeds");
    assert!(early.updated_progress.is_none());

    let due = plan
        .review_progress_at_boundary(&subagent, &started.lease.lease_id, 1_000 + window)
        .expect("due review check succeeds");
    assert!(due.updated_progress.is_some());
    assert!(
        due.updated_progress
            .as_ref()
            .expect("review progress exists")
            .request_coordinator_review
    );
    assert_eq!(
        due.snapshot.leases[0].status,
        merry_core::PlanLeaseStatus::Live
    );
    assert_eq!(
        due.snapshot
            .nodes
            .iter()
            .find(|node| node.id == ids["work"])
            .expect("work node remains")
            .status,
        PlanNodeStatus::InProgress
    );
}

#[test]
fn durable_progress_moves_the_review_window_forward() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Progressing work")]));
    let subagent = actor("subagent-durable-progress");
    let started = plan
        .start_attempt(&ids["work"], subagent.clone(), 1_000)
        .expect("attempt starts");
    let window = plan
        .snapshot()
        .resource_policy_snapshot
        .no_durable_progress_review_window_ms;
    plan.report_progress(
        &subagent,
        ReportPlanProgressInput {
            summary: "Durable evidence was recorded".to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            next_action: Some("Continue the same path".to_owned()),
            checkpoint_ref: None,
            acknowledged_directive_ids: Vec::new(),
            applied_directive_ids: Vec::new(),
            request_coordinator_review: Some(false),
        },
        500_000,
    )
    .expect("durable progress commits");

    let early = plan
        .review_progress_at_boundary(&subagent, &started.lease.lease_id, 500_000 + window - 1)
        .expect("review check succeeds");
    assert!(early.updated_progress.is_none());
    let due = plan
        .review_progress_at_boundary(&subagent, &started.lease.lease_id, 500_000 + window)
        .expect("review becomes due from the durable progress point");
    assert!(due.updated_progress.is_some());
}

#[test]
fn terminal_attempt_report_is_exactly_once() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Work")]));
    let subagent = actor("subagent-once");
    let _started = plan
        .start_attempt(&ids["work"], subagent.clone(), 10)
        .expect("attempt starts");
    let report = ReportPlanAttemptInput {
        outcome: PlanAttemptOutcome::Completed,
        result: Some(completed_result("done")),
        diagnostic: None,
        decomposition: None,
        acknowledged_directive_ids: Vec::new(),
        applied_directive_ids: Vec::new(),
    };
    plan.report_attempt(&subagent, report.clone(), 20)
        .expect("first report succeeds");
    let before = plan.snapshot().clone();
    let error = plan
        .report_attempt(&subagent, report, 30)
        .expect_err("duplicate report rejects");

    assert!(matches!(
        error,
        crate::plan::PlanError::NoActiveAttemptForExecutor { .. }
    ));
    assert_eq!(plan.snapshot(), &before);
}

#[test]
fn transient_failure_after_runtime_recorded_side_effect_does_not_retry() {
    let mut work = leaf("work", "Mutating work");
    work.recovery_policy.max_transient_attempts = 3;
    let (mut plan, ids) = executing_plan(root(vec![work]));
    let subagent = actor("subagent-with-side-effect");
    plan.start_attempt(&ids["work"], subagent.clone(), 10)
        .expect("attempt starts");
    plan.record_runtime_effect(
        &subagent,
        vec!["crates/merry-runtime/src/plan.rs".to_owned()],
        15,
    )
    .expect("runtime effect attribution succeeds");

    let report = plan
        .report_attempt(
            &subagent,
            ReportPlanAttemptInput {
                outcome: PlanAttemptOutcome::TransientFailure,
                result: None,
                diagnostic: Some(
                    ErrorInfo::new("provider_unavailable", "provider failed after a write")
                        .expect("valid diagnostic"),
                ),
                decomposition: None,
                acknowledged_directive_ids: Vec::new(),
                applied_directive_ids: Vec::new(),
            },
            20,
        )
        .expect("transient report commits");

    assert_eq!(
        report
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == ids["work"])
            .expect("work node remains")
            .status,
        PlanNodeStatus::Blocked
    );
    assert_eq!(report.snapshot.phase, merry_core::PlanPhase::Blocked);
}

#[test]
fn subagent_decomposition_adds_only_direct_children_and_releases_them() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Decomposable work")]));
    let subagent = actor("subagent-decompose");
    let _started = plan
        .start_attempt(&ids["work"], subagent.clone(), 10)
        .expect("attempt starts");
    let report = plan
        .report_attempt(
            &subagent,
            ReportPlanAttemptInput {
                outcome: PlanAttemptOutcome::Decomposed,
                result: None,
                diagnostic: None,
                decomposition: Some(PlanDecompositionInput {
                    reason: "Two independent checks are required".to_owned(),
                    children: vec![leaf("check-a", "Check A"), leaf("check-b", "Check B")],
                }),
                acknowledged_directive_ids: Vec::new(),
                applied_directive_ids: Vec::new(),
            },
            20,
        )
        .expect("decomposition succeeds");

    assert_eq!(report.client_key_ids.len(), 2);
    assert_eq!(
        report
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == ids["work"])
            .expect("parent exists")
            .status,
        PlanNodeStatus::Expanded
    );
    assert_eq!(report.ready_node_ids.len(), 2);
}

#[test]
fn nested_subagent_decomposition_rejects_without_resolving_attempt() {
    let (mut plan, ids) = executing_plan(root(vec![leaf("work", "Decomposable work")]));
    let subagent = actor("subagent-invalid-decompose");
    let _started = plan
        .start_attempt(&ids["work"], subagent.clone(), 10)
        .expect("attempt starts");
    let mut nested = leaf("nested-parent", "Nested parent");
    nested.children.push(leaf("nested-child", "Nested child"));
    let error = plan
        .report_attempt(
            &subagent,
            ReportPlanAttemptInput {
                outcome: PlanAttemptOutcome::Decomposed,
                result: None,
                diagnostic: None,
                decomposition: Some(PlanDecompositionInput {
                    reason: "invalid nested expansion".to_owned(),
                    children: vec![nested],
                }),
                acknowledged_directive_ids: Vec::new(),
                applied_directive_ids: Vec::new(),
            },
            20,
        )
        .expect_err("nested decomposition rejects");

    assert!(matches!(error, crate::plan::PlanError::NestedDecomposition));
    assert_eq!(plan.snapshot().attempts[0].outcome, None);
}

#[test]
fn transient_retry_uses_a_new_attempt_and_blocks_at_policy_limit() {
    let mut work = leaf("work", "Retryable work");
    work.recovery_policy.max_transient_attempts = 2;
    let (mut plan, ids) = executing_plan(root(vec![work]));
    let first_subagent = actor("subagent-retry-one");
    let first = plan
        .start_attempt(&ids["work"], first_subagent.clone(), 10)
        .expect("first attempt starts");
    plan.report_attempt(
        &first_subagent,
        transient_failure(first.lease.lease_id, first.lease.node_revision),
        20,
    )
    .expect("first transient failure requeues");
    assert!(plan.ready_node_ids().contains(&ids["work"]));

    let second_subagent = actor("subagent-retry-two");
    let second = plan
        .start_attempt(&ids["work"], second_subagent.clone(), 30)
        .expect("second attempt starts");
    assert_ne!(first.attempt.attempt_id, second.attempt.attempt_id);
    plan.report_attempt(
        &second_subagent,
        transient_failure(second.lease.lease_id, second.lease.node_revision),
        40,
    )
    .expect("second transient failure commits");

    assert_eq!(
        plan.node(&ids["work"]).expect("work node").status,
        PlanNodeStatus::Blocked
    );
}

#[test]
fn transient_retry_backoff_delays_the_next_attempt_without_changing_attempt_identity() {
    let mut work = leaf("work", "Backed off work");
    work.recovery_policy.max_transient_attempts = 2;
    work.recovery_policy.retry_backoff_ms = 100;
    let (mut plan, ids) = executing_plan(root(vec![work]));
    let first_subagent = actor("subagent-backoff-one");
    let first = plan
        .start_attempt(&ids["work"], first_subagent.clone(), 10)
        .expect("first attempt starts");
    plan.report_attempt(
        &first_subagent,
        transient_failure(first.lease.lease_id, first.lease.node_revision),
        20,
    )
    .expect("transient failure commits");

    assert!(!plan.ready_node_ids_at(119).contains(&ids["work"]));
    assert!(matches!(
        plan.start_attempt(&ids["work"], actor("subagent-too-early"), 119),
        Err(crate::plan::PlanError::NodeNotReady { .. })
    ));
    assert!(plan.ready_node_ids_at(120).contains(&ids["work"]));
    let second = plan
        .start_attempt(&ids["work"], actor("subagent-backoff-two"), 120)
        .expect("retry starts when backoff elapses");

    assert_ne!(first.attempt.attempt_id, second.attempt.attempt_id);
    assert_eq!(plan.snapshot().attempts.len(), 2);
}

fn transient_failure(
    _lease_id: merry_core::PlanLeaseId,
    _node_revision: u64,
) -> ReportPlanAttemptInput {
    ReportPlanAttemptInput {
        outcome: PlanAttemptOutcome::TransientFailure,
        result: None,
        diagnostic: Some(ErrorInfo::new("provider_unavailable", "temporary outage").unwrap()),
        decomposition: None,
        acknowledged_directive_ids: Vec::new(),
        applied_directive_ids: Vec::new(),
    }
}
