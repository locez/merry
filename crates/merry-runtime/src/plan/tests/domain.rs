use crate::plan::{
    PlanChangeInput, PlanError, PlanExecutionIntent, PlanNodeInput, PlanNodeReferenceInput,
    UpdatePlanInput, domain::PlanState,
};
use merry_core::{
    PlanActivationSource, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanPhase,
    PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot,
};

fn leaf(client_key: &str, objective: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: objective.to_owned(),
        acceptance: vec![format!("{objective} accepted")],
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
        acceptance: vec!["all children verified".to_owned()],
        executor_policy: PlanExecutorPolicy::Local,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children,
    }
}

fn empty_plan() -> PlanState {
    PlanState::empty(
        PlanId::new("plan-1").expect("valid plan id"),
        PlanActivationSource::Coordinator {
            reason: "test".to_owned(),
            governing_skill_id: None,
        },
        PlanResourcePolicySnapshot::default(),
    )
}

#[test]
fn define_plan_resolves_client_key_dependencies_and_assigns_runtime_ids() {
    let mut plan = empty_plan();
    let first = leaf("first", "Collect evidence");
    let mut second = leaf("second", "Validate evidence");
    second.depends_on = vec![PlanNodeReferenceInput::ClientKey {
        client_key: "first".to_owned(),
    }];

    let output = plan
        .update(UpdatePlanInput {
            reason: "initial plan".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(2),
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![first, second]),
            },
        })
        .expect("valid plan");

    assert_eq!(output.snapshot.phase, PlanPhase::Planning);
    assert_eq!(output.snapshot.revision, 1);
    assert_eq!(output.client_key_ids.len(), 3);
    let first_id = output.client_key_ids["first"].clone();
    let second_id = output.client_key_ids["second"].clone();
    assert_eq!(
        output
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == second_id)
            .expect("second node")
            .depends_on,
        vec![first_id]
    );
}

#[test]
fn define_plan_rejects_dependency_cycles() {
    let mut plan = empty_plan();
    let mut first = leaf("first", "First");
    first.depends_on = vec![PlanNodeReferenceInput::ClientKey {
        client_key: "second".to_owned(),
    }];
    let mut second = leaf("second", "Second");
    second.depends_on = vec![PlanNodeReferenceInput::ClientKey {
        client_key: "first".to_owned(),
    }];

    let error = plan
        .update(UpdatePlanInput {
            reason: "cyclic plan".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![first, second]),
            },
        })
        .expect_err("dependency cycle must reject");

    assert!(matches!(error, PlanError::DependencyCycle));
    assert_eq!(plan.snapshot().revision, 0);
}

#[test]
fn replace_subtree_allows_unrelated_global_revision_advance() {
    let mut plan = empty_plan();
    let initial = plan
        .update(UpdatePlanInput {
            reason: "initial plan".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![leaf("left", "Left"), leaf("right", "Right")]),
            },
        })
        .expect("valid initial plan");
    let target = initial.client_key_ids["left"].clone();
    let target_revision = plan.node(&target).expect("left node").updated_revision;
    plan.advance_unrelated_revision_for_test();

    let mut replacement = leaf("replacement-root", "Left revised");
    replacement.id = Some(target.clone());
    replacement.client_key = None;
    let output = plan
        .update(UpdatePlanInput {
            reason: "evidence changed left branch".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: target.clone(),
                expected_node_revision: target_revision,
                subtree: replacement,
            },
        })
        .expect("unrelated revision must not stale node CAS");

    assert!(output.snapshot.revision > target_revision);
    assert_eq!(
        plan.node(&target).expect("replaced target").objective,
        "Left revised"
    );
}

#[test]
fn replacement_rejects_dangling_incoming_dependency() {
    let mut plan = empty_plan();
    let target = PlanNodeInput {
        children: vec![leaf("endpoint", "Dependency endpoint")],
        ..leaf("target", "Replaceable branch")
    };
    let mut outside = leaf("outside", "Outside consumer");
    outside.depends_on = vec![PlanNodeReferenceInput::ClientKey {
        client_key: "endpoint".to_owned(),
    }];
    let initial = plan
        .update(UpdatePlanInput {
            reason: "initial plan".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![target, outside]),
            },
        })
        .expect("valid initial plan");
    let target_id = initial.client_key_ids["target"].clone();
    let revision = plan.node(&target_id).expect("target").updated_revision;
    let mut replacement = leaf("replacement", "Replacement without endpoint");
    replacement.id = Some(target_id.clone());
    replacement.client_key = None;

    let error = plan
        .update(UpdatePlanInput {
            reason: "remove dependency endpoint".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: target_id,
                expected_node_revision: revision,
                subtree: replacement,
            },
        })
        .expect_err("incoming dependency must prevent replacement");

    assert!(matches!(
        error,
        PlanError::IncomingDependencyWouldDangle { .. }
    ));
}

#[test]
fn child_harness_cannot_expand_parent_write_scope() {
    let mut plan = empty_plan();
    let mut parent = leaf("parent", "Parent");
    parent.harness.write_scope = vec!["crates/runtime".to_owned()];
    let mut child = leaf("child", "Child");
    child.harness.write_scope = vec!["crates/cli".to_owned()];
    parent.children.push(child);

    let error = plan
        .update(UpdatePlanInput {
            reason: "invalid scope".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![parent]),
            },
        })
        .expect_err("child scope expansion must reject");

    assert!(matches!(
        error,
        PlanError::CapabilityEnvelopeExceeded { .. }
    ));
}

#[test]
fn replace_subtree_requires_current_target_node_revision() {
    let mut plan = empty_plan();
    let initial = plan
        .update(UpdatePlanInput {
            reason: "initial".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![leaf("target", "Target")]),
            },
        })
        .expect("valid plan");
    let target_id = initial.client_key_ids["target"].clone();
    let mut replacement = leaf("replacement", "Replacement");
    replacement.id = Some(target_id.clone());
    replacement.client_key = None;

    let error = plan
        .update(UpdatePlanInput {
            reason: "stale replacement".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: target_id.clone(),
                expected_node_revision: 0,
                subtree: replacement,
            },
        })
        .expect_err("stale node revision must reject");

    assert!(matches!(
        error,
        PlanError::StaleNodeRevision { node_id, .. } if node_id == target_id
    ));
}
