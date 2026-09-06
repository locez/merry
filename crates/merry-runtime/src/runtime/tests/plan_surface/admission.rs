use crate::{
    ToolExecutionContext,
    plan::{SubagentPlanChangeInput, SubagentPlanUpdateInput},
    runtime::{
        Runtime,
        tests::plan_surface::{
            linked_plan_scope, pending_call, plan_node, record_pending, session_id,
        },
    },
};
use merry_core::{RuntimeJournalPayload, ToolCallResultStatus};
use serde_json::json;
use tokio::time::{Duration, timeout};

#[tokio::test(flavor = "current_thread")]
async fn linked_child_plan_surface_is_scoped_and_coordinator_scope_is_rejected() {
    let (_coordinator, scope, _plan_id, _owned_id, _sibling_id) =
        linked_plan_scope("plan-child-surface").await;
    let child = Runtime::builder(session_id("plan-linked-child"))
        .plan_subagent_scope(scope.clone())
        .build()
        .expect("linked child runtime builds");
    let plan_names = child
        .inner
        .tool_registry
        .tool_specs()
        .into_iter()
        .filter_map(|spec| match spec.name().as_str() {
            "read_plan" | "update_plan" => Some(spec.name().as_str().to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(plan_names, ["read_plan", "update_plan"]);

    let unbound = Runtime::builder(session_id("plan-unbound-child"))
        .build()
        .expect("unbound child runtime builds");
    assert!(
        unbound
            .inner
            .tool_registry
            .tool_specs()
            .into_iter()
            .all(|spec| !matches!(spec.name().as_str(), "read_plan" | "update_plan"))
    );

    let rejected = Runtime::builder(session_id("plan-coordinator-scope"))
        .coordinator_plan_tools()
        .plan_subagent_scope(scope)
        .build();
    assert!(matches!(
        rejected,
        Err(crate::RuntimeError::InvalidStepInput { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_plan_tools_reject_outside_reads_and_emit_durable_updates() {
    let (coordinator, scope, plan_id, owned_id, sibling_id) =
        linked_plan_scope("plan-scoped-execution").await;
    let child = Runtime::builder(session_id("plan-scoped-child"))
        .plan_subagent_scope(scope)
        .build()
        .expect("linked child runtime builds");

    let other_plan_call = pending_call(
        "call-scoped-other-plan",
        "read_plan",
        json!({
            "plan_id": "plan-other",
            "node_id": owned_id,
            "max_depth": 4
        }),
    );
    record_pending(&child, other_plan_call.clone()).await;
    let other_plan_result = child
        .execute_tool_call(other_plan_call.id(), ToolExecutionContext::default())
        .await
        .expect("out-of-plan read resolves as a tool rejection")
        .into_iter()
        .find_map(|event| match event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("other-plan read records a result");
    assert_eq!(other_plan_result.status(), ToolCallResultStatus::Failed);

    let sibling_call = pending_call(
        "call-scoped-sibling",
        "read_plan",
        json!({
            "plan_id": plan_id,
            "node_id": sibling_id,
            "max_depth": 4
        }),
    );
    record_pending(&child, sibling_call.clone()).await;
    let sibling_result = child
        .execute_tool_call(sibling_call.id(), ToolExecutionContext::default())
        .await
        .expect("sibling read resolves as a tool rejection")
        .into_iter()
        .find_map(|event| match event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("sibling read records a result");
    assert_eq!(sibling_result.status(), ToolCallResultStatus::Failed);

    let mut plan_events = coordinator.subscribe_plan_events();
    let before = coordinator
        .plan_snapshot()
        .await
        .expect("coordinator plan snapshot reads")
        .expect("coordinator plan exists");
    let update = SubagentPlanUpdateInput {
        reason: "define work below the linked child task".to_owned(),
        change: SubagentPlanChangeInput::DefineChildren {
            expected_plan_revision: before.revision,
            children: vec![plan_node("nested", "Nested child-owned work")],
        },
    };
    let update_call = pending_call(
        "call-scoped-update",
        "update_plan",
        serde_json::to_value(update).expect("scoped update serializes"),
    );
    record_pending(&child, update_call.clone()).await;
    let update_events = child
        .execute_tool_call(update_call.id(), ToolExecutionContext::default())
        .await
        .expect("scoped update resolves");
    let update_result = update_events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("scoped update records a result");
    assert_eq!(update_result.status(), ToolCallResultStatus::Succeeded);

    let plan_event = timeout(Duration::from_secs(1), plan_events.recv())
        .await
        .expect("scoped update emits a plan event")
        .expect("plan event receiver remains open");
    assert!(matches!(
        plan_event.payload,
        RuntimeJournalPayload::PlanUpdated { .. }
    ));
    let updated = coordinator
        .plan_snapshot()
        .await
        .expect("updated plan snapshot reads")
        .expect("updated plan exists");
    assert!(
        updated
            .nodes
            .iter()
            .any(|node| node.client_key.as_deref() == Some("nested"))
    );
    assert!(updated.nodes.iter().any(|node| node.id == sibling_id));
}

#[tokio::test(flavor = "current_thread")]
async fn unbound_plan_call_keeps_the_runtime_role_error() {
    let runtime = Runtime::builder(session_id("plan-unbound-call"))
        .build()
        .expect("unbound runtime builds");
    let call = pending_call("call-unbound-plan", "read_plan", json!({"max_depth": 2}));
    record_pending(&runtime, call.clone()).await;
    assert!(matches!(
        runtime
            .execute_tool_call(call.id(), ToolExecutionContext::default())
            .await,
        Err(crate::RuntimeError::ToolExecutionFailed { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_plan_input_is_recorded_with_nested_decode_guidance() {
    let runtime = Runtime::builder(session_id("plan-invalid-input-guidance"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");
    let call = pending_call(
        "call-invalid-plan-input",
        "update_plan",
        json!({
            "reason": "define the requested work",
            "execution_intent": "continue_planning",
            "coordinator_node_id": null,
            "max_concurrency_hint": null,
            "change": {
                "expected_plan_revision": 0,
                "root": {
                    "client_key": "root",
                    "objective": "Complete the requested work",
                    "acceptance": ["the work is verified"],
                    "depends_on": [],
                    "children": []
                }
            }
        }),
    );
    record_pending(&runtime, call.clone()).await;

    let events = runtime
        .execute_tool_call(call.id(), ToolExecutionContext::default())
        .await
        .expect("invalid plan input should resolve as a failed tool result");
    let result = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("invalid plan input should record a tool result");
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result
            .diagnostic()
            .expect("invalid plan input should include a diagnostic")
            .code(),
        "plan_input_invalid"
    );

    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("invalid plan input artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("invalid plan input result should be textual JSON"),
    )
    .expect("invalid plan input result should parse as JSON");
    let message = payload["error"]["message"]
        .as_str()
        .expect("invalid plan input should include an error message");
    assert!(message.contains("change.type"));
    assert!(message.contains("inside the change object"));
    assert_eq!(
        payload["recovery"]["example"]["change"]["type"],
        "define_plan"
    );
}
