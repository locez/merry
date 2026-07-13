use super::*;
use crate::plan::{
    BeginPlanInput, ControlPlanAttemptInput, PlanChangeInput, PlanExecutionIntent, PlanNodeInput,
    ReportPlanAttemptInput, UpdatePlanInput, execution::PlanAttemptActor,
};
use merry_core::{
    PlanAttemptOutcome, PlanCapabilityEnvelopeSnapshot, PlanDirectiveConstraints,
    PlanDirectiveKind, PlanDirectiveStatus, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanNodeResult, PlanNodeStatus, PlanRecoveryPolicySnapshot,
};
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn local_ready_node_is_leased_to_the_root_coordinator_session() {
    let root_session = session_id("runtime-plan-local-lane");
    let runtime = Runtime::builder(root_session.clone())
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "execute local plan node".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let update = runtime
        .update_plan(single_local_plan_input())
        .await
        .expect("local plan definition succeeds");
    let root_id = update.client_key_ids["root"].clone();
    let mut events = runtime.subscribe_plan_events();
    runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot::default(),
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan is authorized");

    let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot.leases.iter().any(|lease| {
                lease.node_id == root_id && lease.status == merry_core::PlanLeaseStatus::Live
            }) {
                break snapshot;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("local lease should be reserved");
    let lease = snapshot
        .leases
        .iter()
        .find(|lease| lease.node_id == root_id)
        .expect("root lease exists");
    assert_eq!(lease.executor_session_id, root_session);
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .find(|node| node.id == root_id)
            .expect("root exists")
            .status,
        PlanNodeStatus::InProgress
    );
}

#[tokio::test(flavor = "current_thread")]
async fn resume_interrupts_stale_leases_without_replaying_completed_nodes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = crate::FileSessionStore::new(temp.path());
    let session_id = session_id("runtime-plan-resume-stale-lease");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");
    runtime
        .inner
        .plan_controller
        .begin(BeginPlanInput {
            reason: "prepare resume fixture".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let update = runtime
        .inner
        .plan_controller
        .update(super::plan_scheduler::parallel_plan_input())
        .await
        .expect("plan definition succeeds");
    runtime
        .inner
        .plan_controller
        .authorize_execution(
            PlanCapabilityEnvelopeSnapshot {
                write_scope: vec!["left".to_owned(), "right".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan authorization succeeds");

    let completed_actor = PlanAttemptActor {
        executor_session_id: merry_core::SessionId::new("completed-worker").unwrap(),
    };
    let completed = runtime
        .inner
        .plan_controller
        .start_attempt(
            update.client_key_ids["left"].clone(),
            completed_actor.clone(),
            1_000,
        )
        .await
        .expect("completed attempt starts")
        .output;
    runtime
        .inner
        .plan_controller
        .attempt_report(
            completed_actor,
            completed_report(
                completed.lease.lease_id.clone(),
                completed.lease.node_revision,
                "completed before resume",
            ),
            2_000,
        )
        .await
        .expect("completed attempt commits");

    let stale_actor = PlanAttemptActor {
        executor_session_id: merry_core::SessionId::new("stale-worker").unwrap(),
    };
    let stale = runtime
        .inner
        .plan_controller
        .start_attempt(update.client_key_ids["right"].clone(), stale_actor, 3_000)
        .await
        .expect("stale attempt starts")
        .output;
    let directive = runtime
        .inner
        .plan_controller
        .directive(
            ControlPlanAttemptInput {
                attempt_id: stale.attempt.attempt_id.clone(),
                expected_lease_id: stale.lease.lease_id.clone(),
                expected_node_revision: stale.lease.node_revision,
                kind: PlanDirectiveKind::Converge,
                reason: "finish the current acceptance target".to_owned(),
                instruction: Some("do not expand further".to_owned()),
                constraints: Some(PlanDirectiveConstraints::default()),
                requested_output: vec!["terminal evidence".to_owned()],
            },
            3_100,
        )
        .await
        .expect("directive commits")
        .output
        .directive;
    runtime
        .save_session_to(store.clone())
        .await
        .expect("session saves with live lease");
    drop(runtime);

    let resumed = Runtime::builder(session_id.clone())
        .resume_from_store(store.clone())
        .await
        .expect("runtime resumes");
    let recovered = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = resumed
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot
                .attempts
                .iter()
                .find(|attempt| attempt.attempt_id == stale.attempt.attempt_id)
                .is_some_and(|attempt| attempt.outcome == Some(PlanAttemptOutcome::Interrupted))
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stale lease is interrupted after resume");

    assert_eq!(
        recovered
            .attempts
            .iter()
            .filter(|attempt| attempt.outcome == Some(PlanAttemptOutcome::Completed))
            .count(),
        1
    );
    assert_eq!(
        recovered
            .nodes
            .iter()
            .find(|node| node.id == update.client_key_ids["left"])
            .expect("completed node remains")
            .status,
        PlanNodeStatus::Completed
    );
    assert_eq!(
        recovered
            .leases
            .iter()
            .find(|lease| lease.lease_id == stale.lease.lease_id)
            .expect("stale lease remains")
            .status,
        merry_core::PlanLeaseStatus::Expired
    );
    assert_eq!(
        recovered
            .directives
            .iter()
            .find(|candidate| candidate.directive_id == directive.directive_id)
            .expect("attempt directive remains")
            .status,
        PlanDirectiveStatus::Expired
    );
    assert_eq!(recovered.attempts.len(), 2);

    resumed
        .save_session()
        .await
        .expect("recovered session saves");
    drop(resumed);
    let resumed_again = Runtime::builder(session_id)
        .resume_from_store(store)
        .await
        .expect("runtime resumes idempotently");
    let snapshot = resumed_again
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert_eq!(snapshot.attempts.len(), 2);
    assert_eq!(
        snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.outcome == Some(PlanAttemptOutcome::Completed))
            .count(),
        1
    );
}

fn completed_report(
    lease_id: merry_core::PlanLeaseId,
    node_revision: u64,
    conclusion: &str,
) -> ReportPlanAttemptInput {
    ReportPlanAttemptInput {
        lease_id,
        expected_node_revision: node_revision,
        outcome: PlanAttemptOutcome::Completed,
        result: Some(PlanNodeResult {
            conclusion: conclusion.to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            changed_paths: Vec::new(),
            verification: vec!["deterministic completion".to_owned()],
            open_questions: Vec::new(),
        }),
        diagnostic: None,
        decomposition: None,
        acknowledged_directive_ids: Vec::new(),
        applied_directive_ids: Vec::new(),
    }
}

fn single_local_plan_input() -> UpdatePlanInput {
    UpdatePlanInput {
        reason: "define one local coordinator node".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Complete local coordinator work".to_owned(),
                acceptance: vec!["local result is verified".to_owned()],
                executor_policy: PlanExecutorPolicy::Local,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    }
}
