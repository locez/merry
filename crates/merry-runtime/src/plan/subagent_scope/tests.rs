mod persistence;
mod projection;
mod updates;

use crate::FileSessionStore;
use crate::plan::{
    BeginPlanInput, PlanChangeInput, PlanController, PlanExecutionIntent, PlanNodeInput,
    UpdatePlanInput,
};
use crate::session::SessionState;
use merry_core::{
    PlanExecutorPolicy, PlanHarnessSnapshot, PlanNodeId, PlanRecoveryPolicySnapshot, SessionId,
    SubagentId, SubagentTaskId,
};
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::Mutex;

fn session_id() -> SessionId {
    SessionId::new("subagent-scope-test").expect("valid session id")
}

fn controller(
    store: Option<FileSessionStore>,
) -> (
    PlanController,
    crate::plan::controller::PlanControllerEventReceiver,
) {
    let (controller, events, _session) = controller_with_session(store);
    (controller, events)
}

fn controller_with_session(
    store: Option<FileSessionStore>,
) -> (
    PlanController,
    crate::plan::controller::PlanControllerEventReceiver,
    Arc<Mutex<SessionState>>,
) {
    let session = Arc::new(Mutex::new(SessionState::new(session_id())));
    let (controller, events) = PlanController::start(
        Arc::clone(&session),
        store,
        NonZeroUsize::new(16).expect("non-zero buffer"),
    );
    (controller, events, session)
}

fn node(client_key: &str, objective: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: objective.to_owned(),
        acceptance: vec![format!("{objective} is verified")],
        status: None,
        executor_policy: PlanExecutorPolicy::default(),
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children: Vec::new(),
    }
}

fn root(children: Vec<PlanNodeInput>) -> PlanNodeInput {
    PlanNodeInput {
        children,
        ..node("root", "Complete all work")
    }
}

async fn linked_scope(
    controller: &PlanController,
) -> (crate::plan::PlanSubagentScope, PlanNodeId, PlanNodeId) {
    controller
        .begin(BeginPlanInput {
            reason: "create a plan for scoped child ownership".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("begin succeeds");
    let plan = controller
        .update(UpdatePlanInput {
            reason: "define coordinator work items".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: root(vec![
                    node("owned", "Owned child work"),
                    node("sibling", "Sibling work"),
                ]),
            },
        })
        .await
        .expect("coordinator update succeeds");
    let owned_id = plan.client_key_to_runtime_node_id["owned"].clone();
    let sibling_id = plan.client_key_to_runtime_node_id["sibling"].clone();
    let link = controller
        .bind_subagent(
            "owned".to_owned(),
            SubagentId::new("agent-scope").expect("valid subagent id"),
            SubagentTaskId::new("task-scope").expect("valid task id"),
            1,
        )
        .await
        .expect("binding succeeds");
    let scope = controller.subagent_scope(
        plan.snapshot.plan_id.clone(),
        owned_id.clone(),
        link.binding_id,
    );
    (scope, owned_id, sibling_id)
}
