use crate::plan::{
    PlanApprovalInput, PlanChangeInput, PlanExecutionIntent, PlanNodeInput, UpdatePlanInput,
    domain::{PlanError, PlanState},
    execution::PlanAttemptActor,
};
use merry_core::{
    PlanActivationSource, PlanAttemptOutcome, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId,
    PlanNodeStatus, PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot,
    PlanSchedulerStatus, SessionId,
};

fn plan_with_review_requirement() -> PlanState {
    plan_with_recovery_limit(PlanRecoveryPolicySnapshot::default().max_transient_attempts)
}

fn plan_with_recovery_limit(max_transient_attempts: u8) -> PlanState {
    let mut plan = PlanState::empty(
        PlanId::new("plan-control").expect("valid plan id"),
        PlanActivationSource::User,
        PlanResourcePolicySnapshot::default(),
    );
    plan.update(UpdatePlanInput {
        reason: "define reviewed plan".to_owned(),
        execution_intent: PlanExecutionIntent::RequestUserReview,
        coordinator_node_id: None,
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Complete reviewed work".to_owned(),
                acceptance: vec!["work is verified".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Delegate,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot {
                    max_transient_attempts,
                    ..PlanRecoveryPolicySnapshot::default()
                },
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    })
    .expect("plan definition succeeds");
    plan
}

fn draft_plan() -> PlanState {
    draft_plan_with_id("plan-draft-control")
}

fn draft_plan_with_id(plan_id: &str) -> PlanState {
    let mut plan = PlanState::empty(
        PlanId::new(plan_id).expect("valid plan id"),
        PlanActivationSource::User,
        PlanResourcePolicySnapshot::default(),
    );
    plan.update(UpdatePlanInput {
        reason: "define a plan draft".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Execute the approved draft".to_owned(),
                acceptance: vec!["the draft completes".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Local,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    })
    .expect("draft definition succeeds");
    plan
}

fn approval_input(plan: &PlanState, expected_plan_revision: u64) -> PlanApprovalInput {
    PlanApprovalInput {
        plan_id: plan.snapshot().plan_id.clone(),
        expected_plan_revision,
        review_resolution_ref: "interactive-user-approval".to_owned(),
        capability_envelope: Some(Default::default()),
        authorization_refs: vec!["existing-runtime-authorization".to_owned()],
        requirement_resolution_refs: Default::default(),
    }
}

#[test]
fn approving_a_non_empty_planning_draft_starts_execution() {
    let mut plan = draft_plan();
    let approval = approval_input(&plan, 1);

    let output = plan
        .approve(approval)
        .expect("a planning draft can be approved directly");

    assert_eq!(output.previous_phase, PlanPhase::Planning);
    assert_eq!(output.snapshot.phase, PlanPhase::Executing);
    assert_eq!(
        output.snapshot.scheduler_status,
        PlanSchedulerStatus::Active
    );
}

#[test]
fn approval_rejects_a_different_plan_with_the_same_revision() {
    let first = draft_plan_with_id("plan-approval-first");
    let stale_approval = approval_input(&first, 1);
    let mut replacement = draft_plan_with_id("plan-approval-replacement");

    let error = replacement
        .approve(stale_approval)
        .expect_err("approval must bind the exact plan identity");

    assert!(matches!(error, PlanError::StalePlanIdentity { .. }));
    assert_eq!(replacement.snapshot().phase, PlanPhase::Planning);
}

#[test]
fn review_only_approval_enters_execution_without_expanding_permissions() {
    let mut plan = plan_with_review_requirement();
    let approval = approval_input(&plan, 1);
    let output = plan.approve(approval).expect("review approval succeeds");

    assert_eq!(output.snapshot.phase, PlanPhase::Executing);
    assert_eq!(
        output.snapshot.scheduler_status,
        PlanSchedulerStatus::Active
    );
    assert!(output.snapshot.approval_requirements.iter().all(
        |requirement| requirement.status == merry_core::PlanApprovalRequirementStatus::Resolved
    ));
}

#[test]
fn pause_and_resume_only_change_new_lease_admission() {
    let mut plan = plan_with_review_requirement();
    let approval = approval_input(&plan, 1);
    plan.approve(approval).expect("review approval succeeds");
    assert_eq!(plan.ready_node_ids().len(), 1);

    let paused = plan
        .pause_scheduling("user paused new work")
        .expect("pause succeeds");
    assert_eq!(
        paused.snapshot.scheduler_status,
        PlanSchedulerStatus::Paused
    );
    assert!(plan.ready_node_ids().is_empty());
    let resumed = plan
        .resume_scheduling("user resumed new work")
        .expect("resume succeeds");
    assert_eq!(
        resumed.snapshot.scheduler_status,
        PlanSchedulerStatus::Active
    );
    assert_eq!(plan.ready_node_ids().len(), 1);
}

#[test]
fn cancellation_drains_live_attempts_then_finishes_the_plan() {
    let mut plan = plan_with_review_requirement();
    let approval = approval_input(&plan, 1);
    plan.approve(approval).expect("review approval succeeds");
    let root_id = plan.snapshot().root_node_id.clone().expect("root exists");
    let actor = PlanAttemptActor {
        executor_session_id: SessionId::new("cancelled-subagent").expect("valid session id"),
    };
    let started = plan
        .start_attempt(&root_id, actor.clone(), 1_000)
        .expect("attempt starts");

    let draining = plan
        .request_cancellation("user cancelled the plan", 1_500)
        .expect("cancellation request succeeds");
    assert_eq!(
        draining.snapshot.scheduler_status,
        PlanSchedulerStatus::Draining
    );
    assert_eq!(draining.snapshot.phase, PlanPhase::Executing);

    let cancelled = plan
        .cancel_attempt(
            &actor,
            &started.lease.lease_id,
            "user cancelled the plan",
            2_000,
        )
        .expect("subagent cancellation commits");
    assert_eq!(cancelled.snapshot.phase, PlanPhase::Cancelled);
    assert_eq!(
        cancelled.attempt.outcome,
        Some(PlanAttemptOutcome::Cancelled)
    );
    assert_eq!(
        cancelled
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == root_id)
            .expect("root remains")
            .status,
        PlanNodeStatus::Blocked
    );
}

#[test]
fn cancellation_closes_a_local_attempt_without_a_lease() {
    let mut plan = draft_plan();
    let approval = approval_input(&plan, 1);
    plan.approve(approval).expect("draft approval succeeds");
    let root_id = plan.snapshot().root_node_id.clone().expect("root exists");
    let coordinator = PlanAttemptActor {
        executor_session_id: SessionId::new("coordinator").expect("valid session id"),
    };
    let started = plan
        .start_local_attempt(&root_id, coordinator, 1_000)
        .expect("local attempt starts");

    let cancelled = plan
        .request_cancellation("user cancelled local work", 2_000)
        .expect("local cancellation commits");

    assert_eq!(cancelled.snapshot.phase, PlanPhase::Cancelled);
    assert_eq!(cancelled.finished_attempts.len(), 1);
    assert_eq!(
        cancelled.finished_attempts[0].attempt_id,
        started.attempt.attempt_id
    );
    assert_eq!(
        cancelled.finished_attempts[0].outcome,
        Some(PlanAttemptOutcome::Cancelled)
    );
    assert!(cancelled.snapshot.leases.is_empty());
    assert!(
        cancelled
            .snapshot
            .attempts
            .iter()
            .all(|attempt| attempt.outcome.is_some())
    );
}

#[test]
fn terminal_report_winning_the_cancel_race_still_settles_draining_plan() {
    let mut plan = plan_with_review_requirement();
    let approval = approval_input(&plan, 1);
    plan.approve(approval).expect("review approval succeeds");
    let root_id = plan.snapshot().root_node_id.clone().expect("root exists");
    let actor = PlanAttemptActor {
        executor_session_id: SessionId::new("racing-subagent").expect("valid session id"),
    };
    plan.start_attempt(&root_id, actor.clone(), 1_000)
        .expect("attempt starts");
    plan.request_cancellation("user cancelled while report was in flight", 1_500)
        .expect("cancellation starts draining");

    let reported = plan
        .report_attempt(
            &actor,
            crate::plan::ReportPlanAttemptInput {
                outcome: PlanAttemptOutcome::Completed,
                result: Some(merry_core::PlanNodeResult {
                    conclusion: "report won the cancellation race".to_owned(),
                    evidence_refs: Vec::new(),
                    artifact_refs: Vec::new(),
                    changed_paths: Vec::new(),
                    verification: Vec::new(),
                    open_questions: Vec::new(),
                }),
                diagnostic: None,
                decomposition: None,
                acknowledged_directive_ids: Vec::new(),
                applied_directive_ids: Vec::new(),
            },
            2_000,
        )
        .expect("terminal report remains valid while draining");

    assert_eq!(reported.snapshot.phase, PlanPhase::Cancelled);
    assert!(
        reported
            .snapshot
            .attempts
            .iter()
            .all(|attempt| attempt.outcome.is_some())
    );
}
