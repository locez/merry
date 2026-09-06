use crate::{
    ToolExecutionContext,
    plan::{PlanChangeInput, PlanExecutionIntent, PlanNodeInput, UpdatePlanInput},
    runtime::{
        Runtime,
        tests::plan_surface::{noop_tool, pending_call, record_pending, session_id},
    },
};
use merry_core::{
    PendingToolCallBatch, PlanExecutorPolicy, PlanHarnessSnapshot, PlanRecoveryPolicySnapshot,
    RuntimeJournalPayload, SessionId, ToolCallBatchId, ToolCallResultStatus,
};
use serde_json::json;

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
async fn define_plan_update_starts_a_fresh_run_without_a_node_id() {
    let runtime = Runtime::builder(session_id("plan-define-rerun-tool"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(crate::plan::BeginPlanInput {
            reason: "establish the run that will be replaced".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activates");
    runtime
        .update_plan(UpdatePlanInput {
            reason: "define the run that will be replaced".to_owned(),
            execution_intent: PlanExecutionIntent::ExecuteIfAuthorized,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: PlanNodeInput {
                    id: None,
                    client_key: Some("initial-root".to_owned()),
                    objective: "Initial run".to_owned(),
                    acceptance: vec!["Initial run is defined".to_owned()],
                    status: None,
                    executor_policy: PlanExecutorPolicy::default(),
                    harness: PlanHarnessSnapshot::default(),
                    recovery_policy: PlanRecoveryPolicySnapshot::default(),
                    depends_on: Vec::new(),
                    children: Vec::new(),
                },
            },
        })
        .await
        .expect("initial plan definition succeeds");
    let before = runtime
        .plan_snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("active plan exists");

    let call = pending_call(
        "call-define-rerun-plan",
        "update_plan",
        json!({
            "reason": "the user requested a clean rerun",
            "execution_intent": "continue_planning",
            "coordinator_node_id": null,
            "max_concurrency_hint": null,
            "change": {
                "type": "define_plan",
                "expected_plan_revision": before.revision,
                "root": {
                    "id": null,
                    "client_key": "rerun-root",
                    "objective": "Run the requested work again",
                    "acceptance": ["Focused checks pass"],
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
        .expect("update_plan rerun executes");
    let result = events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("update_plan rerun resolves");
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);

    let after = runtime
        .plan_snapshot()
        .await
        .expect("new plan snapshot reads")
        .expect("new active plan exists");
    assert_ne!(after.plan_id, before.plan_id);
    assert_eq!(after.phase, merry_core::PlanPhase::Planning);
    assert!(
        after
            .nodes
            .iter()
            .any(|node| node.client_key.as_deref() == Some("rerun-root"))
    );
    let session = runtime.inner.session.lock().await;
    assert!(session.terminal_plans().iter().any(
        |plan| plan.plan_id == before.plan_id && plan.phase == merry_core::PlanPhase::Cancelled
    ));
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
async fn mixed_plan_and_runtime_tool_batch_commits_at_batch_boundary() {
    let temp = tempfile::tempdir().expect("session store tempdir");
    let store = crate::FileSessionStore::new(temp.path());
    let session = SessionId::new("plan-mixed-tool-batch").expect("valid session id");
    let runtime = Runtime::builder(session.clone())
        .coordinator_plan_tools()
        .register_tool(noop_tool())
        .session_store(store.clone())
        .build()
        .expect("runtime builds");

    let update = UpdatePlanInput {
        reason: "define the mixed batch plan".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: None,
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Complete the mixed tool batch".to_owned(),
                acceptance: vec!["both tool results are durable".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Delegate,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    };
    let update_call = pending_call(
        "call-mixed-plan-update",
        "update_plan",
        serde_json::to_value(update).expect("plan update serializes"),
    );
    let ordinary_call = pending_call("call-mixed-ordinary", "registered_tool", json!({}));
    {
        let mut session_state = runtime.inner.session.lock().await;
        let turn_id = session_state
            .begin_model_turn()
            .expect("batch model turn begins");
        session_state
            .record_tool_call_batch_pending(
                turn_id,
                PendingToolCallBatch::new(
                    ToolCallBatchId::new("mixed-tool-batch").expect("valid batch id"),
                    vec![update_call.clone(), ordinary_call.clone()],
                )
                .expect("batch is valid"),
            )
            .expect("batch tool calls are pending");
        session_state
            .close_model_response(turn_id, true)
            .expect("batch model turn closes");
    }

    let permit = runtime
        .acquire_active_step_permit()
        .expect("batch acquires active step permit");
    let execution = runtime
        .execute_tool_call_batch_with_active_permit(
            vec![update_call, ordinary_call],
            ToolExecutionContext::default(),
            &permit,
        )
        .await;
    let (events, error) = execution.into_parts();
    assert!(error.is_none(), "mixed tool batch failed: {error:?}");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event.payload,
                merry_core::RuntimeJournalPayload::ToolCallResolved { .. }
            ))
            .count(),
        2
    );
    assert!(runtime.pending_tool_calls().await.is_empty());
    drop(permit);

    let resumed = Runtime::builder(session)
        .coordinator_plan_tools()
        .resume_from_store(store)
        .await
        .expect("batch savepoint should resume");
    assert!(resumed.pending_tool_calls().await.is_empty());
    assert!(
        resumed
            .plan_snapshot()
            .await
            .expect("resumed plan snapshot reads")
            .is_some()
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
