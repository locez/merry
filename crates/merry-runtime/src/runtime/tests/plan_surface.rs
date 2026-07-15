use super::*;
use crate::{
    RegisteredTool, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
    plan::{
        PlanChangeInput, PlanExecutionIntent, PlanNodeInput, SubagentPlanChangeInput,
        SubagentPlanUpdateInput, UpdatePlanInput,
    },
};
use merry_core::{
    PendingToolCall, PlanExecutorPolicy, PlanHarnessSnapshot, PlanId, PlanNodeId,
    PlanRecoveryPolicySnapshot, SessionId, SubagentActivityPhase, SubagentActivitySnapshot,
    SubagentId, SubagentTaskId, ToolCallArguments, ToolCallId, ToolCallResultStatus,
    ToolInputSchema, ToolName, ToolSpec,
};
use schemars::Schema;
use serde_json::json;
use std::sync::Arc;
use tokio::time::{Duration, timeout};

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

#[tokio::test(flavor = "current_thread")]
async fn runtime_subagent_activity_subscription_reuses_builder_hub() {
    let hub = Arc::new(crate::SubagentActivityHub::new());
    let runtime = Runtime::builder(session_id("activity-subscription"))
        .subagent_activity_hub(Arc::clone(&hub))
        .build()
        .expect("runtime builds");
    let mut receiver = runtime.subscribe_subagent_activity();
    assert!(receiver.borrow().is_empty());

    hub.publish(SubagentActivitySnapshot {
        subagent_id: SubagentId::new("activity-agent").expect("valid agent id"),
        task_id: SubagentTaskId::new("activity-task").expect("valid task id"),
        phase: SubagentActivityPhase::Starting,
        summary: "starting".to_owned(),
        updated_at_ms: 1,
    });
    assert!(
        receiver
            .has_changed()
            .expect("activity receiver remains open")
    );
    assert_eq!(
        receiver.borrow_and_update()[0].phase,
        SubagentActivityPhase::Starting
    );
}

#[tokio::test(flavor = "current_thread")]
async fn activity_projection_does_not_change_plan_revision_or_emit_plan_event() {
    let hub = Arc::new(crate::SubagentActivityHub::new());
    let runtime = Runtime::builder(session_id("activity-plan-isolation"))
        .coordinator_plan_tools()
        .subagent_activity_hub(Arc::clone(&hub))
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(crate::plan::BeginPlanInput {
            reason: "establish an activity isolation plan".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activates");
    let before = runtime
        .plan_snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("active plan exists");
    let mut plan_events = runtime.subscribe_plan_events();

    hub.publish(SubagentActivitySnapshot {
        subagent_id: SubagentId::new("isolated-agent").expect("valid agent id"),
        task_id: SubagentTaskId::new("isolated-task").expect("valid task id"),
        phase: SubagentActivityPhase::Running,
        summary: "working".to_owned(),
        updated_at_ms: 1,
    });

    let after = runtime
        .plan_snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("active plan exists");
    assert_eq!(after.revision, before.revision);
    assert!(plan_events.try_recv().is_err());
}

async fn record_pending(runtime: &Runtime, call: PendingToolCall) {
    let mut session = runtime.inner.session.lock().await;
    session.record_session_started_if_needed();
    session
        .record_test_tool_call_pending(call)
        .expect("pending tool call is valid");
}

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
    let owned_id = output.client_key_ids["owned"].clone();
    let sibling_id = output.client_key_ids["sibling"].clone();
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
