use super::*;
use crate::{
    RegisteredTool, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
    plan::{PlanChangeInput, PlanExecutionIntent, PlanNodeInput, UpdatePlanInput},
};
use merry_core::{
    PendingToolCall, PlanExecutorPolicy, PlanHarnessSnapshot, PlanRecoveryPolicySnapshot,
    ToolCallArguments, ToolCallId, ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
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

#[tokio::test(flavor = "current_thread")]
async fn active_plan_does_not_restrict_main_registered_tools() {
    let runtime = Runtime::builder(session_id("plan-main-tool-admission"))
        .coordinator_plan_tools()
        .register_tool(noop_tool())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(crate::plan::BeginPlanInput {
            reason: "record an advisory plan".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activates");

    let call = pending_call("call-main-tool", "registered_tool", json!({}));
    record_pending(&runtime, call.clone()).await;
    let events = runtime
        .execute_tool_call(call.id(), ToolExecutionContext::default())
        .await
        .expect("main registered tool remains executable");
    let result = events.iter().find_map(|event| match &event.payload {
        RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
        _ => None,
    });
    assert_eq!(
        result.expect("tool resolves").status(),
        ToolCallResultStatus::Succeeded
    );
}

#[tokio::test(flavor = "current_thread")]
async fn first_update_defines_a_plan_without_a_separate_activation_call() {
    let runtime = Runtime::builder(session_id("plan-first-update"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");
    let update = UpdatePlanInput {
        reason: "define the requested work".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: None,
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Complete the requested work".to_owned(),
                acceptance: vec!["the work is verified".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Delegate,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    };
    let call = pending_call(
        "call-first-update",
        "update_plan",
        serde_json::to_value(update).expect("update input serializes"),
    );
    record_pending(&runtime, call.clone()).await;
    runtime
        .execute_tool_call(call.id(), ToolExecutionContext::default())
        .await
        .expect("first update executes");

    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot reads")
        .expect("first update created a plan");
    assert_eq!(snapshot.plan_id.as_str(), "plan-1");
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.nodes.len(), 1);

    let read_call = pending_call(
        "call-read-current-plan",
        "read_plan",
        json!({"max_depth": 4}),
    );
    record_pending(&runtime, read_call.clone()).await;
    let events = runtime
        .execute_tool_call(read_call.id(), ToolExecutionContext::default())
        .await
        .expect("read_plan should return the current snapshot");
    let result = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("read_plan result should be recorded");
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("read_plan artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("read_plan artifact should contain JSON text"),
    )
    .expect("read_plan artifact should parse");
    assert_eq!(
        payload["guidance"]["do_not_repeat_until_state_change"],
        true
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reading_without_an_active_plan_returns_a_non_retrying_recovery() {
    let runtime = Runtime::builder(session_id("plan-read-without-active-plan"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");
    let call = pending_call(
        "call-read-without-active-plan",
        "read_plan",
        json!({"max_depth": 4}),
    );
    record_pending(&runtime, call.clone()).await;

    let events = runtime
        .execute_tool_call(call.id(), ToolExecutionContext::default())
        .await
        .expect("read_plan should resolve with structured recovery");
    let result = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("read_plan result should be recorded");
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("recovery artifact should be readable");
    let payload: serde_json::Value = serde_json::from_str(
        content
            .as_text()
            .expect("recovery artifact should contain JSON text"),
    )
    .expect("recovery artifact should parse");
    assert_eq!(payload["error"]["code"], "no_active_plan");
    assert_eq!(payload["recovery"]["next_tool"], "update_plan");
    assert!(
        payload["recovery"]["instruction"]
            .as_str()
            .expect("recovery instruction should be text")
            .contains("Do not call read_plan again")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn plan_authorization_does_not_start_unbound_execution_or_replay_old_attempts() {
    let runtime = Runtime::builder(session_id("plan-no-automatic-replay"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(crate::plan::BeginPlanInput {
            reason: "prepare an advisory plan".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activates");
    runtime
        .update_plan(UpdatePlanInput {
            reason: "define an advisory delegated task".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: PlanNodeInput {
                    id: None,
                    client_key: Some("root".to_owned()),
                    objective: "Wait for an explicit subagent binding".to_owned(),
                    acceptance: vec!["the bound child reports completion".to_owned()],
                    status: None,
                    executor_policy: PlanExecutorPolicy::Delegate,
                    harness: PlanHarnessSnapshot::default(),
                    recovery_policy: PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: Vec::new(),
                },
            },
        })
        .await
        .expect("plan definition succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("authorization succeeds");

    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot reads")
        .expect("plan exists");
    assert_eq!(snapshot.phase, merry_core::PlanPhase::Executing);
    assert!(snapshot.attempts.is_empty());
    assert!(snapshot.leases.is_empty());
    assert!(snapshot.nodes.iter().all(|node| node.links.is_empty()));
}

#[tokio::test(flavor = "current_thread")]
async fn resuming_a_plan_does_not_replay_an_inflight_legacy_attempt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = crate::FileSessionStore::new(temp.path());
    let session_id = merry_core::SessionId::new("plan-inert-resume").expect("valid session id");
    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(crate::plan::BeginPlanInput {
            reason: "prepare a persisted plan".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activates");
    runtime
        .update_plan(UpdatePlanInput {
            reason: "define a delegated task".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: PlanNodeInput {
                    id: None,
                    client_key: Some("root".to_owned()),
                    objective: "Persist the plan without replaying execution".to_owned(),
                    acceptance: vec!["the plan remains inspectable".to_owned()],
                    status: None,
                    executor_policy: PlanExecutorPolicy::Delegate,
                    harness: PlanHarnessSnapshot::default(),
                    recovery_policy: PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: Vec::new(),
                },
            },
        })
        .await
        .expect("plan definition succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("authorization succeeds");
    let root_id = runtime
        .plan_snapshot()
        .await
        .expect("snapshot reads")
        .expect("plan exists")
        .root_node_id
        .expect("root exists");
    runtime
        .inner
        .plan_controller
        .start_attempt(
            root_id,
            crate::plan::execution::PlanAttemptActor {
                executor_session_id: merry_core::SessionId::new("stale-subagent")
                    .expect("valid executor session id"),
            },
            100,
        )
        .await
        .expect("legacy fixture attempt starts");
    runtime
        .save_session_to(store.clone())
        .await
        .expect("session saves");
    drop(runtime);

    let resumed = Runtime::builder(session_id)
        .coordinator_plan_tools()
        .resume_from_store(store)
        .await
        .expect("runtime resumes");
    let snapshot = resumed
        .plan_snapshot()
        .await
        .expect("snapshot reads")
        .expect("plan exists");
    assert_eq!(snapshot.attempts.len(), 1);
    assert_eq!(
        snapshot.attempts[0].outcome,
        Some(merry_core::PlanAttemptOutcome::Interrupted)
    );
    assert_eq!(snapshot.leases.len(), 1);
    assert_eq!(
        snapshot.leases[0].status,
        merry_core::PlanLeaseStatus::Expired
    );
    assert_eq!(snapshot.phase, merry_core::PlanPhase::Blocked);
}
