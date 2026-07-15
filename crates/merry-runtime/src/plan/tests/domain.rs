use crate::plan::{
    PlanChangeInput, PlanError, PlanExecutionIntent, PlanNodeInput, PlanNodeReferenceInput,
    ReportPlanAttemptInput, UpdatePlanInput, domain::PlanState, execution::PlanAttemptActor,
};
use merry_core::{
    PlanActivationSource, PlanApprovalRequirementKind, PlanAttemptOutcome,
    PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId,
    PlanNodeResult, PlanPhase, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, SessionId,
};
use serde_json::json;

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
        acceptance: vec!["all children verified".to_owned()],
        status: None,
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
fn initial_execute_if_authorized_enters_execution_without_redundant_review() {
    let mut plan = empty_plan();
    let mut executable_root = root(Vec::new());
    executable_root.harness.allowed_tools =
        vec![merry_core::ToolName::new("run_process").expect("valid tool name")];
    executable_root.harness.read_scope = vec!["crates/merry-runtime".to_owned()];
    executable_root.harness.write_scope = vec!["crates/merry-runtime".to_owned()];
    executable_root.harness.forbidden_paths = vec![".git".to_owned()];

    let output = plan
        .update(UpdatePlanInput {
            reason: "the user asked to use this plan and execute it".to_owned(),
            execution_intent: PlanExecutionIntent::ExecuteIfAuthorized,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: executable_root,
            },
        })
        .expect("pre-authorized plan should enter execution");

    assert_eq!(output.snapshot.phase, PlanPhase::Executing);
    assert!(output.snapshot.approval_requirements.is_empty());
    assert!(output.snapshot.execution_contract_fingerprint.is_some());
    assert_eq!(
        output.snapshot.authorized_capability_envelope,
        Some(PlanCapabilityEnvelopeSnapshot {
            allowed_tools: vec![merry_core::ToolName::new("run_process").expect("valid tool name")],
            read_scope: vec!["crates/merry-runtime".to_owned()],
            write_scope: vec!["crates/merry-runtime".to_owned()],
            forbidden_paths: vec![".git".to_owned()],
            destructive_external_authority: false,
        })
    );
}

#[test]
fn initial_request_user_review_still_waits_for_explicit_approval() {
    let mut plan = empty_plan();

    let output = plan
        .update(UpdatePlanInput {
            reason: "the user asked to review the plan before execution".to_owned(),
            execution_intent: PlanExecutionIntent::RequestUserReview,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(Vec::new()),
            },
        })
        .expect("review request should commit an approval boundary");

    assert_eq!(output.snapshot.phase, PlanPhase::AwaitingApproval);
    assert_eq!(
        output
            .snapshot
            .approval_requirements
            .iter()
            .filter(|requirement| {
                requirement.status == merry_core::PlanApprovalRequirementStatus::Pending
            })
            .map(|requirement| &requirement.kind)
            .collect::<Vec<_>>(),
        vec![&PlanApprovalRequirementKind::UserReviewRequested]
    );
}

#[test]
fn existing_plan_enters_execution_without_replacing_its_tree() {
    let mut plan = empty_plan();
    let initial = plan
        .update(UpdatePlanInput {
            reason: "define a plan for user review".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(2),
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![
                    leaf("inspect", "Inspect the workspace"),
                    leaf("verify", "Verify the result"),
                ]),
            },
        })
        .expect("initial plan is valid");
    let current_node_ids = initial
        .snapshot
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();

    let start_input = serde_json::from_value::<UpdatePlanInput>(json!({
        "reason": "the user approved the existing plan",
        "execution_intent": "execute_if_authorized",
        "coordinator_node_id": null,
        "max_concurrency_hint": 2,
        "change": {
            "type": "use_current_plan",
            "expected_plan_revision": initial.snapshot.revision
        }
    }))
    .expect("use_current_plan is a valid update_plan operation");
    let started = plan
        .update(start_input)
        .expect("existing plan starts without tree replacement");

    assert_eq!(started.snapshot.phase, PlanPhase::Executing);
    assert_eq!(
        started
            .snapshot
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>(),
        current_node_ids
    );
    assert!(
        started
            .snapshot
            .nodes
            .iter()
            .all(|node| node.status != merry_core::PlanNodeStatus::Superseded)
    );
}

#[test]
fn workspace_root_scope_contains_concrete_child_scopes() {
    let mut plan = empty_plan();
    let mut plan_root = root(Vec::new());
    plan_root.harness.read_scope = vec![".".to_owned()];
    plan_root.harness.write_scope = vec![".".to_owned()];
    let mut child = leaf("child", "Edit one crate");
    child.harness.read_scope = vec!["crates/merry-runtime".to_owned()];
    child.harness.write_scope = vec!["crates/merry-runtime".to_owned()];
    plan_root.children.push(child);

    let output = plan
        .update(UpdatePlanInput {
            reason: "use the whole workspace as the parent capability scope".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_root,
            },
        })
        .expect("workspace root is a valid normalized scope");

    assert_eq!(output.snapshot.nodes.len(), 2);
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

#[test]
fn replace_subtree_can_revise_the_same_live_target_more_than_once() {
    let mut plan = empty_plan();
    let mut target = leaf("target", "Initial target");
    target.children.push(leaf("initial-child", "Initial child"));
    let initial = plan
        .update(UpdatePlanInput {
            reason: "initial recursive branch".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![target]),
            },
        })
        .expect("initial plan succeeds");
    let target_id = initial.client_key_ids["target"].clone();

    let mut first_replacement = leaf("unused", "First replacement");
    first_replacement.id = Some(target_id.clone());
    first_replacement.client_key = None;
    first_replacement
        .children
        .push(leaf("first-child", "First replacement child"));
    plan.update(UpdatePlanInput {
        reason: "first evidence-driven revision".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: None,
        change: PlanChangeInput::ReplaceSubtree {
            target_node_id: target_id.clone(),
            expected_node_revision: plan
                .node(&target_id)
                .expect("target exists")
                .updated_revision,
            subtree: first_replacement,
        },
    })
    .expect("first replacement succeeds");

    let mut second_replacement = leaf("unused", "Second replacement");
    second_replacement.id = Some(target_id.clone());
    second_replacement.client_key = None;
    second_replacement
        .children
        .push(leaf("second-child", "Second replacement child"));
    let second = plan
        .update(UpdatePlanInput {
            reason: "second evidence-driven revision".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: target_id.clone(),
                expected_node_revision: plan
                    .node(&target_id)
                    .expect("target remains live")
                    .updated_revision,
                subtree: second_replacement,
            },
        })
        .expect("superseded history must not block a second replacement");

    assert_eq!(
        second
            .snapshot
            .nodes
            .iter()
            .find(|node| node.id == target_id)
            .expect("target remains present")
            .objective,
        "Second replacement"
    );
    assert!(second.snapshot.nodes.iter().any(|node| {
        node.status == merry_core::PlanNodeStatus::Superseded
            && node.objective == "First replacement child"
    }));
}

#[test]
fn execution_contract_fingerprint_ignores_runtime_node_state() {
    let mut plan = empty_plan();
    plan.update(UpdatePlanInput {
        reason: "initial executable plan".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: None,
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: root(Vec::new()),
        },
    })
    .expect("plan definition succeeds");
    plan.enter_execution(Default::default(), vec!["test authorization".to_owned()])
        .expect("execution authorization succeeds");
    let fingerprint = plan.contract_fingerprint();
    let root_id = plan.snapshot().root_node_id.clone().expect("root exists");
    let actor = PlanAttemptActor {
        executor_session_id: SessionId::new("contract-subagent").expect("valid session id"),
    };
    let _started = plan
        .start_attempt(&root_id, actor.clone(), 1_000)
        .expect("attempt starts");
    plan.report_attempt(
        &actor,
        ReportPlanAttemptInput {
            outcome: PlanAttemptOutcome::Completed,
            result: Some(PlanNodeResult {
                conclusion: "Root acceptance is satisfied".to_owned(),
                evidence_refs: Vec::new(),
                artifact_refs: Vec::new(),
                changed_paths: Vec::new(),
                verification: vec!["deterministic check passed".to_owned()],
                open_questions: Vec::new(),
            }),
            diagnostic: None,
            decomposition: None,
            acknowledged_directive_ids: Vec::new(),
            applied_directive_ids: Vec::new(),
        },
        2_000,
    )
    .expect("attempt completes");

    assert_eq!(plan.contract_fingerprint(), fingerprint);
}

#[test]
fn root_objective_change_requires_reapproval() {
    let mut plan = empty_plan();
    plan.update(UpdatePlanInput {
        reason: "initial executable plan".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: None,
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: root(Vec::new()),
        },
    })
    .expect("plan definition succeeds");
    plan.enter_execution(Default::default(), vec!["test authorization".to_owned()])
        .expect("execution authorization succeeds");
    let root_id = plan.snapshot().root_node_id.clone().expect("root exists");
    let root_revision = plan.node(&root_id).expect("root node").updated_revision;
    let mut replacement = root(Vec::new());
    replacement.id = Some(root_id.clone());
    replacement.client_key = None;
    replacement.objective = "Complete a materially different objective".to_owned();

    let output = plan
        .update(UpdatePlanInput {
            reason: "root objective changed".to_owned(),
            execution_intent: PlanExecutionIntent::ExecuteIfAuthorized,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: root_id.clone(),
                expected_node_revision: root_revision,
                subtree: replacement,
            },
        })
        .expect("root change is committed for review");

    assert_eq!(output.snapshot.phase, PlanPhase::AwaitingApproval);
    assert!(
        output
            .snapshot
            .approval_requirements
            .iter()
            .any(|requirement| {
                matches!(
                    requirement.kind,
                    PlanApprovalRequirementKind::RootObjectiveChange
                )
            })
    );
}

#[test]
fn runtime_owned_harness_fields_do_not_change_through_plan_revision() {
    let mut plan = empty_plan();
    let mut initial_root = root(Vec::new());
    initial_root.harness.write_scope = vec!["crates/runtime".to_owned()];
    plan.update(UpdatePlanInput {
        reason: "initial executable plan".to_owned(),
        execution_intent: PlanExecutionIntent::ExecuteIfAuthorized,
        coordinator_node_id: None,
        max_concurrency_hint: None,
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: initial_root,
        },
    })
    .expect("plan definition succeeds");
    assert_eq!(plan.snapshot().phase, PlanPhase::Executing);
    let root_id = plan.snapshot().root_node_id.clone().expect("root exists");
    let root_revision = plan.node(&root_id).expect("root node").updated_revision;
    let mut replacement = root(Vec::new());
    replacement.id = Some(root_id.clone());
    replacement.client_key = None;
    replacement.harness.write_scope = vec!["crates/cli".to_owned()];

    let output = plan
        .update(UpdatePlanInput {
            reason: "request broader write scope".to_owned(),
            execution_intent: PlanExecutionIntent::ExecuteIfAuthorized,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::ReplaceSubtree {
                target_node_id: root_id.clone(),
                expected_node_revision: root_revision,
                subtree: replacement,
            },
        })
        .expect("capability proposal is committed for review");

    assert_eq!(output.snapshot.phase, PlanPhase::Executing);
    let node = output
        .snapshot
        .nodes
        .iter()
        .find(|node| node.id == root_id)
        .expect("root remains present");
    assert_eq!(node.harness.write_scope, vec!["crates/runtime"]);
}
