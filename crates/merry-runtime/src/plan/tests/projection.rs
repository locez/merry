use crate::plan::{
    PlanChangeInput, PlanExecutionIntent, PlanNodeInput, PlanState, UpdatePlanInput,
    execution::PlanAttemptActor, projection::worker_plan_control_message,
};
use merry_core::{
    PlanActivationSource, PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanId, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, SessionId,
};

#[test]
fn worker_context_projects_ancestor_path_without_unrelated_siblings() {
    let mut plan = PlanState::empty(
        PlanId::new("plan-projection").unwrap(),
        PlanActivationSource::User,
        PlanResourcePolicySnapshot::default(),
    );
    let update = plan
        .update(UpdatePlanInput {
            reason: "define selective worker context".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(2),
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: node(
                    "root",
                    "Root contract",
                    PlanExecutorPolicy::Local,
                    vec![
                        node(
                            "target",
                            "Target worker",
                            PlanExecutorPolicy::Delegate,
                            Vec::new(),
                        ),
                        node(
                            "unrelated",
                            "Unrelated sibling secret",
                            PlanExecutorPolicy::Delegate,
                            Vec::new(),
                        ),
                    ],
                ),
            },
        })
        .expect("plan definition succeeds");
    plan.enter_execution(
        PlanCapabilityEnvelopeSnapshot::default(),
        vec!["test authorization".to_owned()],
    )
    .expect("plan enters execution");
    let started = plan
        .start_attempt(
            &update.client_key_ids["target"],
            PlanAttemptActor {
                executor_session_id: SessionId::new("projection-worker").unwrap(),
            },
            1_000,
        )
        .expect("target attempt starts");

    let message = worker_plan_control_message(
        plan.snapshot(),
        &update.client_key_ids["target"],
        &started.attempt.attempt_id,
        &started.lease.lease_id,
    )
    .expect("worker projection exists");
    let json = message
        .strip_prefix("<plan_worker_context>\n")
        .and_then(|value| value.strip_suffix("\n</plan_worker_context>"))
        .expect("projection has stable wrapper");
    let projection: serde_json::Value = serde_json::from_str(json).expect("projection is JSON");

    assert_eq!(projection["node"]["objective"], "Target worker");
    assert_eq!(projection["ancestor_path"][0]["objective"], "Root contract");
    assert_eq!(projection["ancestor_path"].as_array().unwrap().len(), 1);
    assert!(!message.contains("Unrelated sibling secret"));
}

fn node(
    client_key: &str,
    objective: &str,
    executor_policy: PlanExecutorPolicy,
    children: Vec<PlanNodeInput>,
) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: objective.to_owned(),
        acceptance: vec![format!("{objective} is verified")],
        executor_policy,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children,
    }
}
