use crate::plan::projection::{
    CHILD_LINKED_SCOPE_GUIDANCE, CHILD_SCOPED_UPDATE_GUIDANCE,
    COORDINATOR_LINKED_SUMMARIES_GUIDANCE, COORDINATOR_ROOT_SCOPE_GUIDANCE,
    LINKED_CHILD_DECOMPOSITION_GUIDANCE, PLAN_SEMANTIC_CHECKPOINT_GUIDANCE,
    RUNTIME_OWNED_EXECUTION_GUIDANCE,
};
use crate::plan::{
    PlanChangeInput, PlanExecutionIntent, PlanNodeInput, PlanState, UpdatePlanInput,
    execution::PlanAttemptActor,
    projection::{coordinator_plan_control_message, plan_subagent_control_message},
};
use merry_core::{
    PlanActivationSource, PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanId, PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, SessionId,
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

    let child_guidance = &projection["child_guidance"];
    assert_eq!(
        child_guidance["instruction"], CHILD_LINKED_SCOPE_GUIDANCE,
        "child scope guidance must use the shared contract"
    );
    let rules = child_guidance["rules"]
        .as_array()
        .expect("child guidance rules are an array");
    for rule in [
        CHILD_SCOPED_UPDATE_GUIDANCE,
        RUNTIME_OWNED_EXECUTION_GUIDANCE,
        PLAN_SEMANTIC_CHECKPOINT_GUIDANCE,
    ] {
        assert!(
            rules
                .iter()
                .any(|candidate| candidate.as_str() == Some(rule)),
            "child guidance must contain the shared rule {rule:?}"
        );
    }

    let mut changed_snapshot = plan.snapshot().clone();
    changed_snapshot.revision += 1;
    changed_snapshot.phase = PlanPhase::Completed;
    let changed_projection = subagent_projection(
        plan_subagent_control_message(
            &changed_snapshot,
            &update.client_key_ids["target"],
            &started.attempt.attempt_id,
            &started.lease.lease_id,
        )
        .expect("changed subagent projection exists"),
    );
    assert_eq!(
        projection["child_guidance"], changed_projection["child_guidance"],
        "child guidance must be static across plan revisions and phases"
    );
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
    assert_coordinator_rules(&projection);
}

#[test]
fn coordinator_context_excludes_activity_and_wait_transport_guidance() {
    let mut plan = empty_plan("plan-coordinator-transport-boundary");
    plan.update(UpdatePlanInput {
        reason: "define transport-neutral coordinator guidance".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: None,
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: node(
                "root",
                "Transport-neutral root contract",
                PlanExecutorPolicy::Local,
                Vec::new(),
            ),
        },
    })
    .expect("plan definition succeeds");

    let messages = [
        coordinator_plan_control_message(plan.snapshot()),
        crate::plan::projection::coordinator_plan_inactive_control_message(),
    ];
    for message in messages {
        for forbidden in ["activity", "Activity", "wait_subagents", "polling", "UI"] {
            assert!(
                !message.contains(forbidden),
                "coordinator projection must not contain {forbidden:?}"
            );
        }
    }
}

#[test]
fn inactive_coordinator_context_explains_the_same_static_delegation_contract() {
    let projection = coordinator_projection_from_message(
        crate::plan::projection::coordinator_plan_inactive_control_message(),
    );

    assert_eq!(projection["phase"], "inactive");
    assert_coordinator_rules(&projection);
}

#[test]
fn coordinator_guidance_rules_are_static_across_plan_phases_and_revisions() {
    let plan = empty_plan("plan-coordinator-static-guidance");
    let planning = coordinator_projection(&plan);
    let mut changed_snapshot = plan.snapshot().clone();
    changed_snapshot.revision = 41;
    changed_snapshot.phase = PlanPhase::Executing;
    let changed =
        coordinator_projection_from_message(coordinator_plan_control_message(&changed_snapshot));

    assert_eq!(
        planning["coordinator_guidance"]["rules"], changed["coordinator_guidance"]["rules"],
        "coordinator guidance rules must not vary with plan revision or phase"
    );
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
    coordinator_projection_from_message(coordinator_plan_control_message(plan.snapshot()))
}

fn coordinator_projection_from_message(message: String) -> serde_json::Value {
    let json = message
        .strip_prefix("<plan_context>\n")
        .and_then(|value| value.strip_suffix("\n</plan_context>"))
        .expect("projection has stable wrapper");
    serde_json::from_str(json).expect("projection is JSON")
}

fn subagent_projection(message: String) -> serde_json::Value {
    let json = message
        .strip_prefix("<plan_subagent_context>\n")
        .and_then(|value| value.strip_suffix("\n</plan_subagent_context>"))
        .expect("projection has stable wrapper");
    serde_json::from_str(json).expect("projection is JSON")
}

fn assert_coordinator_rules(projection: &serde_json::Value) {
    let rendered = projection.to_string();
    for fragment in [
        COORDINATOR_ROOT_SCOPE_GUIDANCE,
        LINKED_CHILD_DECOMPOSITION_GUIDANCE,
        COORDINATOR_LINKED_SUMMARIES_GUIDANCE,
        RUNTIME_OWNED_EXECUTION_GUIDANCE,
    ] {
        assert!(
            rendered.contains(fragment),
            "coordinator guidance must contain the shared fragment {fragment:?}"
        );
    }
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
