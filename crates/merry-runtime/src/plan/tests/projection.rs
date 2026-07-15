use crate::plan::{
    PlanChangeInput, PlanExecutionIntent, PlanNodeInput, PlanState, UpdatePlanInput,
    execution::PlanAttemptActor,
    projection::{coordinator_plan_control_message, plan_subagent_control_message},
};
use merry_core::{
    PlanActivationSource, PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanId, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, SessionId,
};

#[test]
fn subagent_context_projects_ancestor_path_without_unrelated_siblings() {
    let mut plan = PlanState::empty(
        PlanId::new("plan-projection").unwrap(),
        PlanActivationSource::User,
        PlanResourcePolicySnapshot::default(),
    );
    let update = plan
        .update(UpdatePlanInput {
            reason: "define selective subagent context".to_owned(),
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
                            "Target subagent",
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
                executor_session_id: SessionId::new("projection-subagent").unwrap(),
            },
            1_000,
        )
        .expect("target attempt starts");

    let message = plan_subagent_control_message(
        plan.snapshot(),
        &update.client_key_ids["target"],
        &started.attempt.attempt_id,
        &started.lease.lease_id,
    )
    .expect("subagent projection exists");
    let json = message
        .strip_prefix("<plan_subagent_context>\n")
        .and_then(|value| value.strip_suffix("\n</plan_subagent_context>"))
        .expect("projection has stable wrapper");
    let projection: serde_json::Value = serde_json::from_str(json).expect("projection is JSON");

    assert_eq!(projection["node"]["objective"], "Target subagent");
    assert_eq!(projection["ancestor_path"][0]["objective"], "Root contract");
    assert_eq!(projection["ancestor_path"].as_array().unwrap().len(), 1);
    assert!(!message.contains("Unrelated sibling secret"));
}

#[test]
fn coordinator_context_explains_planning_actions_and_runtime_owned_completion() {
    let mut plan = empty_plan("plan-coordinator-planning");
    plan.update(UpdatePlanInput {
        reason: "define work before execution".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: node(
                "root",
                "Root contract",
                PlanExecutorPolicy::Local,
                Vec::new(),
            ),
        },
    })
    .expect("plan definition succeeds");

    let projection = coordinator_projection(&plan);
    assert_eq!(
        projection["coordinator_guidance"]["phase_action"],
        "define_or_refine_plan"
    );
    let instruction = projection["coordinator_guidance"]["instruction"]
        .as_str()
        .expect("planning instruction is text");
    assert!(instruction.contains("first valid update creates the plan"));
    assert!(instruction.contains("ordinary work remains available"));
    let rules = projection["coordinator_guidance"]["rules"]
        .as_array()
        .expect("rules are an array");
    assert!(rules.iter().any(|rule| {
        rule.as_str()
            .is_some_and(|rule| rule.contains("runtime-owned"))
    }));
    assert!(rules.iter().any(|rule| {
        rule.as_str()
            .is_some_and(|rule| rule.contains("auxiliary projection"))
    }));
}

#[test]
fn coordinator_context_tells_model_to_wait_at_user_approval_boundary() {
    let mut plan = empty_plan("plan-coordinator-approval");
    plan.update(UpdatePlanInput {
        reason: "request explicit review".to_owned(),
        execution_intent: PlanExecutionIntent::RequestUserReview,
        coordinator_node_id: None,
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: node(
                "root",
                "Reviewed root contract",
                PlanExecutorPolicy::Local,
                Vec::new(),
            ),
        },
    })
    .expect("review plan definition succeeds");

    let projection = coordinator_projection(&plan);
    assert_eq!(projection["phase"], "awaiting_approval");
    assert_eq!(
        projection["coordinator_guidance"]["phase_action"],
        "wait_for_user_approval"
    );
    let instruction = projection["coordinator_guidance"]["instruction"]
        .as_str()
        .expect("approval instruction is text");
    assert!(instruction.contains("pending approval requirement"));
    assert!(instruction.contains("ordinary tools"));
}

fn empty_plan(id: &str) -> PlanState {
    PlanState::empty(
        PlanId::new(id).unwrap(),
        PlanActivationSource::User,
        PlanResourcePolicySnapshot::default(),
    )
}

fn coordinator_projection(plan: &PlanState) -> serde_json::Value {
    let message = coordinator_plan_control_message(plan.snapshot());
    let json = message
        .strip_prefix("<plan_context>\n")
        .and_then(|value| value.strip_suffix("\n</plan_context>"))
        .expect("projection has stable wrapper");
    serde_json::from_str(json).expect("projection is JSON")
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
        status: None,
        executor_policy,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children,
    }
}
