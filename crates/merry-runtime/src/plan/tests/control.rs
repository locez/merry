use crate::plan::{
    PlanApprovalInput, PlanChangeInput, PlanExecutionIntent, PlanNodeInput, UpdatePlanInput,
    domain::PlanState, execution::PlanAttemptActor,
};
use merry_core::{
    PlanActivationSource, PlanAttemptOutcome, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId,
    PlanNodeStatus, PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot,
    PlanSchedulerStatus, SessionId,
};

fn plan_with_review_requirement() -> PlanState {
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
                executor_policy: PlanExecutorPolicy::Delegate,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    })
    .expect("plan definition succeeds");
    plan
}

#[test]
fn review_only_approval_enters_execution_without_expanding_permissions() {
    let mut plan = plan_with_review_requirement();
    let output = plan
        .approve(PlanApprovalInput {
            review_resolution_ref: "interactive-user-approval".to_owned(),
            capability_envelope: Some(Default::default()),
            authorization_refs: vec!["existing-runtime-authorization".to_owned()],
            requirement_resolution_refs: Default::default(),
        })
        .expect("review approval succeeds");

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
    plan.approve(PlanApprovalInput {
        review_resolution_ref: "interactive-user-approval".to_owned(),
        capability_envelope: Some(Default::default()),
        authorization_refs: vec!["existing-runtime-authorization".to_owned()],
        requirement_resolution_refs: Default::default(),
    })
    .expect("review approval succeeds");
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
    plan.approve(PlanApprovalInput {
        review_resolution_ref: "interactive-user-approval".to_owned(),
        capability_envelope: Some(Default::default()),
        authorization_refs: vec!["existing-runtime-authorization".to_owned()],
        requirement_resolution_refs: Default::default(),
    })
    .expect("review approval succeeds");
    let root_id = plan.snapshot().root_node_id.clone().expect("root exists");
    let actor = PlanAttemptActor {
        executor_session_id: SessionId::new("cancelled-worker").expect("valid session id"),
    };
    let started = plan
        .start_attempt(&root_id, actor.clone(), 1_000)
        .expect("attempt starts");

    let draining = plan
        .request_cancellation("user cancelled the plan")
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
        .expect("worker cancellation commits");
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
