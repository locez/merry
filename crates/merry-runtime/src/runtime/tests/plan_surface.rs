use crate::{
    RegisteredTool, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
    plan::{PlanChangeInput, PlanExecutionIntent, PlanNodeInput, UpdatePlanInput},
    runtime::{Runtime, tests::support::common::RuntimeSessionStateTestExt},
};
use merry_core::{
    PendingToolCall, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanNodeId,
    PlanRecoveryPolicySnapshot, SessionId, SubagentId, SubagentTaskId, ToolCallArguments,
    ToolCallId, ToolInputSchema, ToolName, ToolSpec,
};
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;

struct NoopTool;

impl ToolExecutor for NoopTool {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async { Ok(ToolExecutionOutcome::succeeded_json(r#"{"ok":true}"#)) })
    }
}

fn noop_tool() -> RegisteredTool {
    let schema = Schema::try_from(json!({"type": "object"})).expect("valid test schema");
    let spec = ToolSpec::new(
        ToolName::new("registered_tool").expect("valid tool name"),
        "A registered test tool.",
        ToolInputSchema::new(schema).expect("valid input schema"),
    )
    .expect("valid tool spec");
    RegisteredTool::read_only(spec, Arc::new(NoopTool))
}

fn pending_call(id: &str, name: &str, arguments: serde_json::Value) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(id).expect("valid call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolCallArguments::try_from(arguments).expect("valid tool arguments"),
    )
}

async fn record_pending(runtime: &Runtime, call: PendingToolCall) {
    let mut session = runtime.inner.session.lock().await;
    session.record_session_started_if_needed();
    session
        .record_test_tool_call_pending(call)
        .expect("pending tool call is valid");
}

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid test session id")
}

fn plan_node(client_key: &str, objective: &str) -> PlanNodeInput {
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

async fn linked_plan_scope(
    session: &str,
) -> (
    Runtime,
    crate::PlanSubagentScope,
    PlanId,
    PlanNodeId,
    PlanNodeId,
) {
    let coordinator = Runtime::builder(session_id(session))
        .coordinator_plan_tools()
        .build()
        .expect("coordinator runtime builds");
    coordinator
        .begin_plan(crate::plan::BeginPlanInput {
            reason: "activate linked child plan fixture".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("coordinator plan activates");
    let output = coordinator
        .update_plan(UpdatePlanInput {
            reason: "define linked child plan work".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: PlanNodeInput {
                    children: vec![
                        plan_node("owned", "Owned child task"),
                        plan_node("sibling", "Sibling coordinator task"),
                    ],
                    ..plan_node("root", "Complete all linked child work")
                },
            },
        })
        .await
        .expect("coordinator plan definition succeeds");
    let owned_id = output.client_key_to_runtime_node_id["owned"].clone();
    let sibling_id = output.client_key_to_runtime_node_id["sibling"].clone();
    let link = coordinator
        .inner
        .plan_controller
        .bind_subagent(
            "owned".to_owned(),
            SubagentId::new("agent-scoped-test").expect("valid subagent id"),
            SubagentTaskId::new("task-scoped-test").expect("valid task id"),
            1,
        )
        .await
        .expect("linked child binding succeeds");
    let plan_id = output.snapshot.plan_id.clone();
    let scope =
        crate::PlanSubagentScope::from_internal(coordinator.inner.plan_controller.subagent_scope(
            plan_id.clone(),
            owned_id.clone(),
            link.binding_id,
        ));
    (coordinator, scope, plan_id, owned_id, sibling_id)
}

mod activity;

mod admission;

mod lifecycle;
