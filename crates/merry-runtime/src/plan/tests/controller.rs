use crate::{
    FileSessionStore,
    plan::{
        BeginPlanInput, PlanChangeInput, PlanController, PlanExecutionIntent, PlanNodeInput,
        PlanState, ReportPlanAttemptInput, UpdatePlanInput,
        controller::PlanControllerEventReceiver,
    },
    session::SessionState,
};
use merry_core::{
    PlanActivationSource, PlanAttemptOutcome, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId,
    PlanNodeId, PlanNodeResult, PlanRecoveryPolicySnapshot, PlanResourcePolicySnapshot, SessionId,
};
use std::{num::NonZeroUsize, sync::Arc};
use tokio::sync::Mutex;

fn session_id() -> SessionId {
    SessionId::new("plan-controller-test").expect("valid session id")
}

fn input(reason: &str) -> BeginPlanInput {
    BeginPlanInput {
        reason: reason.to_owned(),
        governing_skill_id: None,
    }
}

fn controller(store: Option<FileSessionStore>) -> (PlanController, PlanControllerEventReceiver) {
    PlanController::start(
        Arc::new(Mutex::new(SessionState::new(session_id()))),
        store,
        NonZeroUsize::new(16).expect("non-zero buffer"),
    )
}

fn plan_leaf(client_key: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: format!("Complete {client_key}"),
        acceptance: vec![format!("{client_key} verified")],
        status: None,
        executor_policy: PlanExecutorPolicy::Delegate,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children: Vec::new(),
    }
}

fn plan_root(children: Vec<PlanNodeInput>) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some("root".to_owned()),
        objective: "Complete all work".to_owned(),
        acceptance: vec!["all leaves verified".to_owned()],
        status: None,
        executor_policy: PlanExecutorPolicy::Local,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children,
    }
}

fn executing_plan_for_durability() -> (PlanState, PlanNodeId) {
    let mut plan = PlanState::empty(
        PlanId::new("durable-attempt-plan").expect("valid plan id"),
        PlanActivationSource::Coordinator {
            reason: "durability test".to_owned(),
            governing_skill_id: None,
        },
        PlanResourcePolicySnapshot::default(),
    );
    let output = plan
        .update(UpdatePlanInput {
            reason: "define durable attempt work".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: Some(1),
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: plan_root(vec![plan_leaf("work")]),
            },
        })
        .expect("durability plan defines");
    let node_id = output.client_key_to_runtime_node_id["work"].clone();
    plan.enter_execution(
        Default::default(),
        vec!["durability authorization".to_owned()],
    )
    .expect("durability plan enters execution");
    (plan, node_id)
}

fn replacement_for(node: &merry_core::PlanNodeSnapshot, objective: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: Some(node.id.clone()),
        client_key: None,
        objective: objective.to_owned(),
        acceptance: node.acceptance.clone(),
        status: None,
        executor_policy: node.executor_policy,
        harness: node.harness.clone(),
        recovery_policy: node.recovery_policy.clone(),
        depends_on: node
            .depends_on
            .iter()
            .cloned()
            .map(|id| crate::plan::PlanNodeReferenceInput::Id { id })
            .collect(),
        children: Vec::new(),
    }
}

fn completed_report(
    _lease: &merry_core::PlanLeaseSnapshot,
    conclusion: &str,
) -> ReportPlanAttemptInput {
    ReportPlanAttemptInput {
        outcome: PlanAttemptOutcome::Completed,
        result: Some(PlanNodeResult {
            conclusion: conclusion.to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            changed_paths: Vec::new(),
            verification: vec!["test verification".to_owned()],
            open_questions: Vec::new(),
        }),
        diagnostic: None,
        decomposition: None,
        acknowledged_directive_ids: Vec::new(),
        applied_directive_ids: Vec::new(),
    }
}

mod durability;

mod editing;

mod subagent_links;
