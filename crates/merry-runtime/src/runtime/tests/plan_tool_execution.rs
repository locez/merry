use super::*;
use crate::{
    FileSessionStore,
    plan::{
        BeginPlanInput, ControlPlanAttemptInput, PlanChangeInput, PlanExecutionIntent,
        PlanNodeInput, PlanSubagentControl, ReadPlanInput, ReportPlanAttemptInput,
        ReportPlanProgressInput, UpdatePlanInput,
        execution::PlanAttemptActor,
        tools::{
            BEGIN_PLAN_TOOL_NAME, CONTROL_PLAN_ATTEMPT_TOOL_NAME, READ_PLAN_TOOL_NAME,
            REPORT_PLAN_ATTEMPT_TOOL_NAME, REPORT_PLAN_PROGRESS_TOOL_NAME, UPDATE_PLAN_TOOL_NAME,
        },
    },
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, PlanAttemptOutcome,
    PlanCapabilityEnvelopeSnapshot, PlanDirectiveConstraints, PlanDirectiveKind,
    PlanExecutorPolicy, PlanHarnessSnapshot, PlanNodeResult, PlanRecoveryPolicySnapshot,
    ToolCallArguments,
};
use serde::Serialize;
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn begin_plan_tool_commits_before_result_and_repeated_activation_is_idempotent() {
    let runtime = Runtime::builder(session_id("runtime-plan-tool-begin"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime should build");
    let first = pending_plan_tool(
        "call-begin-plan-first",
        BEGIN_PLAN_TOOL_NAME,
        &BeginPlanInput {
            reason: "coordinate a recursive task".to_owned(),
            governing_skill_id: None,
        },
    );
    record_pending(&runtime, first.clone()).await;

    let events = runtime
        .execute_tool_call(first.id(), ToolExecutionContext::default())
        .await
        .expect("begin_plan should execute");

    assert!(matches!(
        events[0].payload,
        RuntimeJournalPayload::PlanUpdated { .. }
    ));
    assert!(matches!(
        events[1].payload,
        RuntimeJournalPayload::ArtifactRecorded { .. }
    ));
    assert!(matches!(
        events[2].payload,
        RuntimeJournalPayload::ToolCallResolved { .. }
    ));
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    let first_plan_id = runtime
        .plan_snapshot()
        .await
        .expect("plan snapshot read succeeds")
        .expect("active plan exists")
        .plan_id;

    let repeated = pending_plan_tool(
        "call-begin-plan-repeated",
        BEGIN_PLAN_TOOL_NAME,
        &BeginPlanInput {
            reason: "the coordinator independently selected planning".to_owned(),
            governing_skill_id: None,
        },
    );
    record_pending(&runtime, repeated.clone()).await;
    let repeated_events = runtime
        .execute_tool_call(repeated.id(), ToolExecutionContext::default())
        .await
        .expect("repeated begin_plan should execute");

    assert_eq!(
        repeated_events
            .iter()
            .filter(|event| matches!(event.payload, RuntimeJournalPayload::PlanUpdated { .. }))
            .count(),
        0
    );
    assert_eq!(
        runtime
            .plan_snapshot()
            .await
            .expect("plan snapshot read succeeds")
            .expect("active plan exists")
            .plan_id,
        first_plan_id
    );
}

#[tokio::test(flavor = "current_thread")]
async fn update_plan_without_activation_returns_begin_plan_recovery() {
    let runtime = Runtime::builder(session_id("runtime-plan-tool-no-active-plan"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime should build");
    let update = update_input(0, "root", "Define work after activation");
    let pending = pending_plan_tool("call-update-without-plan", UPDATE_PLAN_TOOL_NAME, &update);
    record_pending(&runtime, pending.clone()).await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("missing plan resolves as a failed tool outcome");
    let payload = resolved_json(&runtime, &events).await;

    assert_eq!(payload["error"]["code"], "no_active_plan");
    assert_eq!(payload["recovery"]["next_tool"], "begin_plan");
    assert!(
        payload["recovery"]["instruction"]
            .as_str()
            .is_some_and(|value| value.contains("before retrying update_plan"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn local_plan_attempt_rejects_a_registered_tool_outside_its_harness() {
    let executor = SuccessfulToolExecutor::new();
    let runtime = Runtime::builder(session_id("runtime-plan-local-tool-harness"))
        .coordinator_plan_tools()
        .register_tool(RegisteredTool::read_only(
            required_query_tool_spec("outside_harness"),
            Arc::new(executor.clone()),
        ))
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "test local tool harness admission".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Use only the declared harness");
    let PlanChangeInput::DefinePlan { root, .. } = &mut update.change else {
        unreachable!("fixture defines a plan")
    };
    root.executor_policy = PlanExecutorPolicy::Local;
    runtime
        .update_plan(update)
        .await
        .expect("plan update succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan authorization succeeds");
    wait_for_live_local_attempt(&runtime).await;

    let pending = PendingToolCall::new(
        ToolCallId::new("call-outside-local-harness").expect("valid call id"),
        ToolName::new("outside_harness").expect("valid tool name"),
        ToolCallArguments::try_from(serde_json::json!({"query": "test"}))
            .expect("arguments are an object"),
    );
    record_pending(&runtime, pending.clone()).await;
    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("harness denial resolves the pending tool call");

    assert_eq!(executor.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events)
            .diagnostic()
            .expect("denial includes a diagnostic")
            .code(),
        "plan_harness_tool_denied"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn local_plan_attempt_rejects_a_workspace_path_outside_its_read_scope() {
    let executor = SuccessfulToolExecutor::new();
    let path_schema = schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "path": { "type": "string" } },
        "required": ["path"]
    }))
    .expect("path schema is valid");
    let spec = ToolSpec::new(
        ToolName::new("scoped_read").expect("valid tool name"),
        "Read one scoped path",
        ToolInputSchema::new(path_schema).expect("valid tool input schema"),
    )
    .expect("valid tool spec");
    let runtime = Runtime::builder(session_id("runtime-plan-local-path-harness"))
        .coordinator_plan_tools()
        .register_tool(RegisteredTool::read_only(spec, Arc::new(executor.clone())))
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "test local path harness admission".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Read only the allowed subtree");
    let PlanChangeInput::DefinePlan { root, .. } = &mut update.change else {
        unreachable!("fixture defines a plan")
    };
    root.executor_policy = PlanExecutorPolicy::Local;
    root.harness.allowed_tools = vec![ToolName::new("scoped_read").expect("valid tool name")];
    root.harness.read_scope = vec!["allowed".to_owned()];
    runtime
        .update_plan(update)
        .await
        .expect("plan update succeeds");
    runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                allowed_tools: vec![ToolName::new("scoped_read").expect("valid tool name")],
                read_scope: vec!["allowed".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan authorization succeeds");
    wait_for_live_local_attempt(&runtime).await;

    let pending = PendingToolCall::new(
        ToolCallId::new("call-outside-local-read-scope").expect("valid call id"),
        ToolName::new("scoped_read").expect("valid tool name"),
        ToolCallArguments::try_from(serde_json::json!({"path": "outside/file.txt"}))
            .expect("arguments are an object"),
    );
    record_pending(&runtime, pending.clone()).await;
    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("scope denial resolves the pending tool call");

    assert_eq!(executor.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events)
            .diagnostic()
            .expect("denial includes a diagnostic")
            .code(),
        "plan_harness_scope_denied"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn local_plan_search_without_a_path_cannot_escape_its_read_scope() {
    let executor = SuccessfulToolExecutor::new();
    let runtime = Runtime::builder(session_id("runtime-plan-local-search-root-scope"))
        .coordinator_plan_tools()
        .register_tool(RegisteredTool::read_only(
            optional_path_search_spec(),
            Arc::new(executor.clone()),
        ))
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "test omitted search path harness admission".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Search only the allowed subtree");
    let PlanChangeInput::DefinePlan { root, .. } = &mut update.change else {
        unreachable!("fixture defines a plan")
    };
    root.executor_policy = PlanExecutorPolicy::Local;
    root.harness.allowed_tools = vec![ToolName::new("workspace_search_text").unwrap()];
    root.harness.read_scope = vec!["allowed".to_owned()];
    runtime
        .update_plan(update)
        .await
        .expect("plan update succeeds");
    runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                allowed_tools: vec![ToolName::new("workspace_search_text").unwrap()],
                read_scope: vec!["allowed".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan authorization succeeds");
    wait_for_live_local_attempt(&runtime).await;

    let pending = PendingToolCall::new(
        ToolCallId::new("call-local-search-without-path").unwrap(),
        ToolName::new("workspace_search_text").unwrap(),
        ToolCallArguments::try_from(serde_json::json!({"query": "needle"})).unwrap(),
    );
    record_pending(&runtime, pending.clone()).await;
    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("scope denial resolves the pending tool call");

    assert_eq!(executor.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events).diagnostic().unwrap().code(),
        "plan_harness_scope_denied"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn delegated_plan_search_without_a_path_cannot_escape_its_read_scope() {
    let root_session_id = session_id("runtime-plan-delegated-search-root");
    let root_runtime = Runtime::builder(root_session_id)
        .coordinator_plan_tools()
        .build()
        .expect("root runtime builds");
    root_runtime
        .begin_plan(BeginPlanInput {
            reason: "test delegated omitted search path admission".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Search one delegated subtree");
    let PlanChangeInput::DefinePlan { root, .. } = &mut update.change else {
        unreachable!("fixture defines a plan")
    };
    root.executor_policy = PlanExecutorPolicy::Delegate;
    root.harness.allowed_tools = vec![ToolName::new("workspace_search_text").unwrap()];
    root.harness.read_scope = vec!["allowed".to_owned()];
    let update = root_runtime
        .update_plan(update)
        .await
        .expect("plan update succeeds");
    root_runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                allowed_tools: vec![ToolName::new("workspace_search_text").unwrap()],
                read_scope: vec!["allowed".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan authorization succeeds");
    let child_session_id = session_id("runtime-plan-delegated-search-child");
    let started = root_runtime
        .inner
        .plan_controller
        .start_attempt(
            update.client_key_ids["root"].clone(),
            PlanAttemptActor {
                executor_session_id: child_session_id.clone(),
            },
            1_000,
        )
        .await
        .expect("delegated attempt starts")
        .output;
    let plan_id = root_runtime
        .plan_snapshot()
        .await
        .unwrap()
        .expect("plan exists")
        .plan_id;
    let control = PlanSubagentControl::new(
        root_runtime.inner.plan_controller.clone(),
        plan_id,
        update.client_key_ids["root"].clone(),
        started.attempt.attempt_id,
        started.lease.lease_id,
        child_session_id.clone(),
    );
    let executor = SuccessfulToolExecutor::new();
    let child_runtime = Runtime::builder(child_session_id)
        .register_tool(RegisteredTool::read_only(
            optional_path_search_spec(),
            Arc::new(executor.clone()),
        ))
        .plan_subagent_control(control)
        .build()
        .expect("child runtime builds");
    let pending = PendingToolCall::new(
        ToolCallId::new("call-delegated-search-without-path").unwrap(),
        ToolName::new("workspace_search_text").unwrap(),
        ToolCallArguments::try_from(serde_json::json!({"query": "needle"})).unwrap(),
    );
    record_pending(&child_runtime, pending.clone()).await;
    let events = child_runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("scope denial resolves the pending tool call");

    assert_eq!(executor.call_count(), 0);
    assert_eq!(
        resolved_tool_result(&events).diagnostic().unwrap().code(),
        "plan_harness_scope_denied"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn workspace_effect_is_attributed_before_the_executor_starts() {
    let executor = ProposingToolExecutor::blocking();
    let tool = RegisteredTool::new(
        policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let runtime = Runtime::builder(session_id("runtime-plan-effect-before-execution"))
        .coordinator_plan_tools()
        .register_tool(tool)
        .allow_low_risk_workspace_patches()
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "prove effect attribution precedes execution".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Apply one fail-closed patch");
    let PlanChangeInput::DefinePlan { root, .. } = &mut update.change else {
        unreachable!("fixture defines a plan")
    };
    root.executor_policy = PlanExecutorPolicy::Local;
    root.harness.allowed_tools = vec![ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap()];
    root.harness.write_scope = vec!["notes".to_owned()];
    runtime
        .update_plan(update)
        .await
        .expect("plan update succeeds");
    runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                allowed_tools: vec![ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap()],
                write_scope: vec!["notes".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan authorization succeeds");
    wait_for_live_local_attempt(&runtime).await;

    let pending = PendingToolCall::new(
        ToolCallId::new("call-plan-effect-before-execution").unwrap(),
        ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap(),
        ToolCallArguments::new(Default::default()),
    );
    record_pending(&runtime, pending.clone()).await;
    let executing_runtime = runtime.clone();
    let call_id = pending.id().clone();
    let execution = tokio::spawn(async move {
        executing_runtime
            .execute_tool_call(&call_id, ToolExecutionContext::default())
            .await
    });
    executor.wait_for_execute_start().await;

    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert_eq!(snapshot.attempt_progress[0].observable_side_effects, 1);
    assert_eq!(
        snapshot.attempt_progress[0].changed_paths,
        ["notes/proposed.txt"]
    );

    executor.release_execute();
    execution
        .await
        .expect("tool task does not panic")
        .expect("tool execution succeeds");
}

#[tokio::test(flavor = "current_thread")]
async fn failed_effect_attribution_prevents_workspace_execution() {
    let root_runtime = Runtime::builder(session_id("runtime-plan-effect-fail-closed-root"))
        .coordinator_plan_tools()
        .build()
        .expect("root runtime builds");
    root_runtime
        .begin_plan(BeginPlanInput {
            reason: "prove attribution failure is fail-closed".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Apply one delegated patch");
    let PlanChangeInput::DefinePlan { root, .. } = &mut update.change else {
        unreachable!("fixture defines a plan")
    };
    root.executor_policy = PlanExecutorPolicy::Delegate;
    root.harness.allowed_tools = vec![ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap()];
    root.harness.write_scope = vec!["notes".to_owned()];
    let update = root_runtime
        .update_plan(update)
        .await
        .expect("plan update succeeds");
    root_runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                allowed_tools: vec![ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap()],
                write_scope: vec!["notes".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan authorization succeeds");
    let child_session_id = session_id("runtime-plan-effect-child");
    let started = root_runtime
        .inner
        .plan_controller
        .start_attempt(
            update.client_key_ids["root"].clone(),
            PlanAttemptActor {
                executor_session_id: child_session_id.clone(),
            },
            1_000,
        )
        .await
        .expect("delegated attempt starts")
        .output;
    let snapshot = root_runtime
        .plan_snapshot()
        .await
        .unwrap()
        .expect("plan exists");
    let control = PlanSubagentControl::new(
        root_runtime.inner.plan_controller.clone(),
        snapshot.plan_id,
        update.client_key_ids["root"].clone(),
        started.attempt.attempt_id,
        started.lease.lease_id,
        child_session_id.clone(),
    );
    let executor = ProposingToolExecutor::blocking_proposal();
    let child_runtime = Runtime::builder(child_session_id)
        .register_tool(
            RegisteredTool::new(
                policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
                Arc::new(executor.clone()),
                ToolActionKind::WorkspaceWrite,
            )
            .with_action_proposal(),
        )
        .allow_low_risk_workspace_patches()
        .plan_subagent_control(control.clone())
        .build()
        .expect("child runtime builds");
    let pending = PendingToolCall::new(
        ToolCallId::new("call-plan-effect-attribution-fails").unwrap(),
        ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap(),
        ToolCallArguments::new(Default::default()),
    );
    record_pending(&child_runtime, pending.clone()).await;

    let executing_runtime = child_runtime.clone();
    let call_id = pending.id().clone();
    let execution = tokio::spawn(async move {
        executing_runtime
            .execute_tool_call(&call_id, ToolExecutionContext::default())
            .await
    });
    executor.wait_for_propose_start().await;
    control
        .report_attempt(
            ReportPlanAttemptInput {
                outcome: PlanAttemptOutcome::Completed,
                result: Some(PlanNodeResult {
                    conclusion: "Attempt completed while the proposal was pending".to_owned(),
                    evidence_refs: Vec::new(),
                    artifact_refs: Vec::new(),
                    changed_paths: Vec::new(),
                    verification: vec!["terminal race fixture".to_owned()],
                    open_questions: Vec::new(),
                }),
                diagnostic: None,
                decomposition: None,
                acknowledged_directive_ids: Vec::new(),
                applied_directive_ids: Vec::new(),
            },
            Vec::new(),
            2_000,
        )
        .await
        .expect("attempt becomes terminal during proposal");
    executor.release_propose();
    let error = execution
        .await
        .expect("tool task does not panic")
        .expect_err("attribution failure must stop execution");

    assert!(matches!(error, RuntimeError::PlanEffectAttribution { .. }));
    assert_eq!(executor.propose_count(), 1);
    assert_eq!(executor.execute_count(), 0);
    assert_eq!(
        root_runtime
            .plan_snapshot()
            .await
            .unwrap()
            .expect("plan exists")
            .attempt_progress[0]
            .observable_side_effects,
        0
    );
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_subagent_attempt_rejects_later_mutating_tool_without_execution() {
    let root_runtime = Runtime::builder(session_id("runtime-plan-terminal-subagent-root"))
        .coordinator_plan_tools()
        .build()
        .expect("root runtime builds");
    root_runtime
        .begin_plan(BeginPlanInput {
            reason: "prove terminal subagents remain fail-closed".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Apply one delegated patch");
    let PlanChangeInput::DefinePlan { root, .. } = &mut update.change else {
        unreachable!("fixture defines a plan")
    };
    root.executor_policy = PlanExecutorPolicy::Delegate;
    root.harness.allowed_tools = vec![ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap()];
    root.harness.write_scope = vec!["notes".to_owned()];
    let update = root_runtime
        .update_plan(update)
        .await
        .expect("plan update succeeds");
    root_runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                allowed_tools: vec![ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap()],
                write_scope: vec!["notes".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan authorization succeeds");
    let child_session_id = session_id("runtime-plan-terminal-subagent-child");
    let started = root_runtime
        .inner
        .plan_controller
        .start_attempt(
            update.client_key_ids["root"].clone(),
            PlanAttemptActor {
                executor_session_id: child_session_id.clone(),
            },
            1_000,
        )
        .await
        .expect("delegated attempt starts")
        .output;
    let plan_id = root_runtime
        .plan_snapshot()
        .await
        .unwrap()
        .expect("plan exists")
        .plan_id;
    let control = PlanSubagentControl::new(
        root_runtime.inner.plan_controller.clone(),
        plan_id,
        update.client_key_ids["root"].clone(),
        started.attempt.attempt_id,
        started.lease.lease_id,
        child_session_id.clone(),
    );
    let executor = ProposingToolExecutor::immediate();
    let child_runtime = Runtime::builder(child_session_id)
        .register_tool(
            RegisteredTool::new(
                policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
                Arc::new(executor.clone()),
                ToolActionKind::WorkspaceWrite,
            )
            .with_action_proposal(),
        )
        .allow_low_risk_workspace_patches()
        .plan_subagent_control(control)
        .build()
        .expect("child runtime builds");

    let terminal = pending_plan_tool(
        "call-terminal-subagent-report",
        REPORT_PLAN_ATTEMPT_TOOL_NAME,
        &ReportPlanAttemptInput {
            outcome: PlanAttemptOutcome::Completed,
            result: Some(PlanNodeResult {
                conclusion: "Delegated work completed".to_owned(),
                evidence_refs: Vec::new(),
                artifact_refs: Vec::new(),
                changed_paths: Vec::new(),
                verification: vec!["delegated verification passed".to_owned()],
                open_questions: Vec::new(),
            }),
            diagnostic: None,
            decomposition: None,
            acknowledged_directive_ids: Vec::new(),
            applied_directive_ids: Vec::new(),
        },
    );
    record_pending(&child_runtime, terminal.clone()).await;
    child_runtime
        .execute_tool_call(terminal.id(), ToolExecutionContext::default())
        .await
        .expect("terminal report succeeds");

    let mutating = PendingToolCall::new(
        ToolCallId::new("call-after-terminal-subagent-report").unwrap(),
        ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap(),
        ToolCallArguments::new(Default::default()),
    );
    record_pending(&child_runtime, mutating.clone()).await;
    let error = child_runtime
        .execute_tool_call(mutating.id(), ToolExecutionContext::default())
        .await
        .expect_err("a terminal subagent must not execute another tool");

    assert!(matches!(
        error,
        RuntimeError::PlanSubagentAttemptInactive { .. }
    ));
    assert_eq!(executor.propose_count(), 0);
    assert_eq!(executor.execute_count(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn successful_workspace_effect_is_attributed_before_transient_retry_decision() {
    let executor = ProposingToolExecutor::immediate();
    let tool = RegisteredTool::new(
        policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
        Arc::new(executor.clone()),
        ToolActionKind::WorkspaceWrite,
    )
    .with_action_proposal();
    let runtime = Runtime::builder(session_id("runtime-plan-effect-attribution"))
        .coordinator_plan_tools()
        .register_tool(tool)
        .allow_low_risk_workspace_patches()
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "test runtime-owned effect attribution".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Apply one scoped patch");
    let PlanChangeInput::DefinePlan { root, .. } = &mut update.change else {
        unreachable!("fixture defines a plan")
    };
    root.executor_policy = PlanExecutorPolicy::Local;
    root.recovery_policy.max_transient_attempts = 3;
    root.harness.allowed_tools = vec![ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap()];
    root.harness.write_scope = vec!["notes".to_owned()];
    runtime
        .update_plan(update)
        .await
        .expect("plan update succeeds");
    runtime
        .authorize_plan_execution(
            PlanCapabilityEnvelopeSnapshot {
                allowed_tools: vec![ToolName::new(WORKSPACE_PATCH_TOOL_NAME).unwrap()],
                write_scope: vec!["notes".to_owned()],
                ..PlanCapabilityEnvelopeSnapshot::default()
            },
            vec!["test authorization".to_owned()],
        )
        .await
        .expect("plan authorization succeeds");
    wait_for_live_local_attempt(&runtime).await;

    let pending = PendingToolCall::new(
        ToolCallId::new("call-plan-effect-attribution").expect("valid call id"),
        ToolName::new(WORKSPACE_PATCH_TOOL_NAME).expect("valid tool name"),
        ToolCallArguments::new(Default::default()),
    );
    record_pending(&runtime, pending.clone()).await;
    runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("scoped workspace effect executes");
    let after_effect = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert_eq!(after_effect.attempt_progress[0].observable_side_effects, 1);
    assert_eq!(
        after_effect.attempt_progress[0].changed_paths,
        ["notes/proposed.txt"]
    );

    runtime
        .report_current_local_plan_attempt(ReportPlanAttemptInput {
            outcome: PlanAttemptOutcome::TransientFailure,
            result: None,
            diagnostic: Some(
                merry_core::ErrorInfo::new(
                    "provider_failed_after_patch",
                    "provider failed after the patch committed",
                )
                .expect("valid diagnostic"),
            ),
            decomposition: None,
            acknowledged_directive_ids: Vec::new(),
            applied_directive_ids: Vec::new(),
        })
        .await
        .expect("transient report commits");
    let terminal = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert_eq!(terminal.phase, merry_core::PlanPhase::Blocked);
    assert_eq!(terminal.attempts.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn update_plan_tool_returns_compact_output_and_stale_revision_does_not_mutate() {
    let runtime = runtime_with_empty_plan("runtime-plan-tool-update").await;
    let update = update_input(0, "root", "Implement recursive scheduling");
    let pending = pending_plan_tool("call-update-plan", UPDATE_PLAN_TOOL_NAME, &update);
    record_pending(&runtime, pending.clone()).await;

    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("update_plan should execute");
    assert!(matches!(
        events[0].payload,
        RuntimeJournalPayload::PlanUpdated { .. }
    ));
    let payload = resolved_json(&runtime, &events).await;
    assert_eq!(payload["revision"], 1);
    assert_eq!(payload["phase"], "planning");
    assert!(payload["client_key_ids"]["root"].is_string());
    assert!(
        payload.get("nodes").is_none(),
        "tool continuation must stay compact"
    );
    assert!(
        !payload
            .to_string()
            .contains("Implement recursive scheduling"),
        "full authored tree must not be echoed in update output"
    );

    let stale = pending_plan_tool(
        "call-update-plan-stale",
        UPDATE_PLAN_TOOL_NAME,
        &update_input(0, "replacement", "Replace stale plan"),
    );
    record_pending(&runtime, stale.clone()).await;
    let stale_events = runtime
        .execute_tool_call(stale.id(), ToolExecutionContext::default())
        .await
        .expect("stale update should resolve as failed tool output");

    let result = resolved_tool_result(&stale_events);
    assert_eq!(result.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        result.diagnostic().expect("diagnostic exists").code(),
        "plan_stale_revision"
    );
    assert_eq!(
        runtime
            .plan_snapshot()
            .await
            .expect("snapshot succeeds")
            .expect("plan exists")
            .revision,
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn local_attempt_report_binds_runtime_identity_without_model_supplied_ids() {
    let runtime = Runtime::builder(session_id("runtime-plan-tool-local-report"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime should build");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "exercise local plan reporting".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let mut update = update_input(0, "root", "Complete local work");
    update.execution_intent = PlanExecutionIntent::ExecuteIfAuthorized;
    update.change = match update.change {
        PlanChangeInput::DefinePlan {
            expected_plan_revision,
            mut root,
        } => {
            root.executor_policy = PlanExecutorPolicy::Local;
            PlanChangeInput::DefinePlan {
                expected_plan_revision,
                root,
            }
        }
        _ => unreachable!("update_input defines a plan"),
    };
    runtime
        .update_plan(update)
        .await
        .expect("local executable plan is defined");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot
                .attempts
                .iter()
                .any(|attempt| attempt.outcome.is_none())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("local attempt starts");

    let report = pending_plan_tool(
        "call-local-plan-report",
        REPORT_PLAN_ATTEMPT_TOOL_NAME,
        &serde_json::json!({
            "outcome": "completed",
            "result": {
                "conclusion": "Local work completed",
                "evidence_refs": [],
                "artifact_refs": [],
                "changed_paths": [],
                "verification": ["deterministic local check passed"],
                "open_questions": []
            },
            "diagnostic": null,
            "decomposition": null,
            "acknowledged_directive_ids": [],
            "applied_directive_ids": []
        }),
    );
    record_pending(&runtime, report.clone()).await;
    let events = runtime
        .execute_tool_call(report.id(), ToolExecutionContext::default())
        .await
        .expect("local report resolves through the plan runtime");
    let payload = resolved_json(&runtime, &events).await;

    assert_eq!(payload["phase"], "completed");
    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("terminal plan remains readable");
    assert!(snapshot.leases.is_empty());
    assert_eq!(
        snapshot.attempts[0].outcome,
        Some(PlanAttemptOutcome::Completed)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn update_plan_while_awaiting_approval_returns_user_control_recovery() {
    let runtime = runtime_with_empty_plan("runtime-plan-tool-awaiting-approval").await;
    let mut review = update_input(0, "root", "Wait for explicit user review");
    review.execution_intent = PlanExecutionIntent::RequestUserReview;
    let pending = pending_plan_tool("call-request-review", UPDATE_PLAN_TOOL_NAME, &review);
    record_pending(&runtime, pending.clone()).await;
    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("review update succeeds");
    let output = resolved_json(&runtime, &events).await;
    let root_id: merry_core::PlanNodeId =
        serde_json::from_value(output["client_key_ids"]["root"].clone()).expect("root id decodes");

    let mut subtree = node("replacement", "Do not replace during approval");
    subtree.id = Some(root_id.clone());
    subtree.client_key = None;
    let replace = UpdatePlanInput {
        reason: "incorrectly try to replace while awaiting approval".to_owned(),
        execution_intent: PlanExecutionIntent::ExecuteIfAuthorized,
        coordinator_node_id: Some(root_id.clone()),
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::ReplaceSubtree {
            target_node_id: root_id,
            expected_node_revision: 1,
            subtree,
        },
    };
    let pending = pending_plan_tool(
        "call-replace-awaiting-approval",
        UPDATE_PLAN_TOOL_NAME,
        &replace,
    );
    record_pending(&runtime, pending.clone()).await;
    let events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("wrong-phase update resolves as a failed tool outcome");
    let payload = resolved_json(&runtime, &events).await;

    assert_eq!(payload["error"]["code"], "plan_wrong_phase");
    assert_eq!(payload["recovery"]["actor"], "user");
    assert_eq!(
        payload["recovery"]["next_action"],
        "approve_or_request_revision_in_plan_ui"
    );
    assert!(
        payload["recovery"]["instruction"]
            .as_str()
            .is_some_and(|value| value.contains("Do not call update_plan"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn read_plan_tool_selects_an_exact_bounded_subtree() {
    let runtime = runtime_with_empty_plan("runtime-plan-tool-read").await;
    let mut root = node("root", "Root objective");
    let mut branch = node("branch", "Branch objective");
    branch.children.push(node("leaf", "Leaf objective"));
    root.children.push(branch);
    root.children.push(node("sibling", "Unrelated sibling"));
    let update = UpdatePlanInput {
        reason: "define nested plan".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(2),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root,
        },
    };
    let pending = pending_plan_tool("call-update-for-read", UPDATE_PLAN_TOOL_NAME, &update);
    record_pending(&runtime, pending.clone()).await;
    let update_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("update succeeds");
    let update_output = resolved_json(&runtime, &update_events).await;
    let branch_id: merry_core::PlanNodeId =
        serde_json::from_value(update_output["client_key_ids"]["branch"].clone())
            .expect("branch id decodes");

    let read = pending_plan_tool(
        "call-read-subtree",
        READ_PLAN_TOOL_NAME,
        &ReadPlanInput {
            plan_id: None,
            node_id: Some(branch_id.clone()),
            max_depth: Some(1),
            include_attempts: Some(false),
            include_leases: None,
            include_progress: Some(false),
            include_directives: Some(false),
            cursor: None,
        },
    );
    record_pending(&runtime, read.clone()).await;
    let read_events = runtime
        .execute_tool_call(read.id(), ToolExecutionContext::default())
        .await
        .expect("read_plan succeeds");
    let read_output = resolved_json(&runtime, &read_events).await;
    assert_eq!(
        read_output["selected_node_id"],
        serde_json::to_value(branch_id).unwrap()
    );
    let objectives = read_output["snapshot"]["nodes"]
        .as_array()
        .expect("nodes array")
        .iter()
        .map(|node| node["objective"].as_str().expect("objective"))
        .collect::<Vec<_>>();
    assert_eq!(objectives, ["Branch objective", "Leaf objective"]);
    assert!(
        read_output["snapshot"]["attempts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn local_attempt_control_progress_and_terminal_tools_commit_through_controller() {
    let runtime = runtime_with_empty_plan("runtime-plan-attempt-tools").await;
    let update = runtime
        .update_plan(update_input(0, "root", "Complete local work"))
        .await
        .expect("plan definition succeeds");
    runtime
        .inner
        .plan_controller
        .authorize_execution(
            PlanCapabilityEnvelopeSnapshot::default(),
            vec!["existing user authorization".to_owned()],
        )
        .await
        .expect("execution authorization succeeds");
    let actor = PlanAttemptActor {
        executor_session_id: runtime.inner.session_id.clone(),
    };
    let started = runtime
        .inner
        .plan_controller
        .start_attempt(update.client_key_ids["root"].clone(), actor, 1_000)
        .await
        .expect("local attempt starts")
        .output;

    let control = pending_plan_tool(
        "call-control-plan-attempt",
        CONTROL_PLAN_ATTEMPT_TOOL_NAME,
        &ControlPlanAttemptInput {
            attempt_id: started.attempt.attempt_id.clone(),
            kind: PlanDirectiveKind::Converge,
            reason: "The acceptance evidence is sufficient".to_owned(),
            instruction: Some("Finish the current verification".to_owned()),
            constraints: Some(PlanDirectiveConstraints::default()),
            requested_output: vec!["concise terminal result".to_owned()],
        },
    );
    record_pending(&runtime, control.clone()).await;
    let control_events = runtime
        .execute_tool_call(control.id(), ToolExecutionContext::default())
        .await
        .expect("control tool succeeds");
    let control_output = resolved_json(&runtime, &control_events).await;
    let directive_id: merry_core::PlanDirectiveId =
        serde_json::from_value(control_output["directive"]["directive_id"].clone())
            .expect("directive id decodes");

    let progress = pending_plan_tool(
        "call-report-plan-progress",
        REPORT_PLAN_PROGRESS_TOOL_NAME,
        &ReportPlanProgressInput {
            summary: "Verification completed".to_owned(),
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            next_action: Some("Return the terminal result".to_owned()),
            checkpoint_ref: Some("checkpoint-local".to_owned()),
            acknowledged_directive_ids: vec![directive_id.clone()],
            applied_directive_ids: vec![directive_id],
            request_coordinator_review: Some(false),
        },
    );
    record_pending(&runtime, progress.clone()).await;
    let progress_events = runtime
        .execute_tool_call(progress.id(), ToolExecutionContext::default())
        .await
        .expect("progress tool succeeds");
    assert!(progress_events.iter().any(|event| matches!(
        event.payload,
        RuntimeJournalPayload::PlanAttemptProgressReported { .. }
    )));

    let terminal = pending_plan_tool(
        "call-report-plan-attempt",
        REPORT_PLAN_ATTEMPT_TOOL_NAME,
        &ReportPlanAttemptInput {
            outcome: PlanAttemptOutcome::Completed,
            result: Some(PlanNodeResult {
                conclusion: "Local work completed".to_owned(),
                evidence_refs: Vec::new(),
                artifact_refs: Vec::new(),
                changed_paths: Vec::new(),
                verification: vec!["local deterministic verification".to_owned()],
                open_questions: Vec::new(),
            }),
            diagnostic: None,
            decomposition: None,
            acknowledged_directive_ids: Vec::new(),
            applied_directive_ids: Vec::new(),
        },
    );
    record_pending(&runtime, terminal.clone()).await;
    let terminal_events = runtime
        .execute_tool_call(terminal.id(), ToolExecutionContext::default())
        .await
        .expect("terminal tool succeeds");
    assert!(terminal_events.iter().any(|event| matches!(
        event.payload,
        RuntimeJournalPayload::PlanAttemptFinished { .. }
    )));
    assert_eq!(
        runtime
            .plan_snapshot()
            .await
            .unwrap()
            .expect("plan exists")
            .phase,
        merry_core::PlanPhase::Completed
    );
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_report_promotes_exact_artifacts_through_pending_root_and_resume() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let root_session_id = session_id("runtime-plan-subagent-artifact-root");
    let root = Runtime::builder(root_session_id.clone())
        .coordinator_plan_tools()
        .session_store(store.clone())
        .build()
        .expect("root runtime builds");
    root.begin_plan(BeginPlanInput {
        reason: "coordinate exact subagent evidence".to_owned(),
        governing_skill_id: None,
    })
    .await
    .expect("plan begins");
    let mut subagent_node = node("root", "Produce exact subagent evidence");
    subagent_node.executor_policy = PlanExecutorPolicy::Delegate;
    let update = root
        .update_plan(UpdatePlanInput {
            reason: "define delegated subagent proof".to_owned(),
            execution_intent: PlanExecutionIntent::ContinuePlanning,
            coordinator_node_id: None,
            max_concurrency_hint: None,
            change: PlanChangeInput::DefinePlan {
                expected_plan_revision: 0,
                root: subagent_node,
            },
        })
        .await
        .expect("plan definition succeeds");
    root.inner
        .plan_controller
        .authorize_execution(
            PlanCapabilityEnvelopeSnapshot::default(),
            vec!["existing user authorization".to_owned()],
        )
        .await
        .expect("execution authorization succeeds");

    let subagent_session_id = session_id("runtime-plan-subagent-artifact-child");
    let actor = PlanAttemptActor {
        executor_session_id: subagent_session_id.clone(),
    };
    let started = root
        .inner
        .plan_controller
        .start_attempt(update.client_key_ids["root"].clone(), actor, 1_000)
        .await
        .expect("subagent attempt starts")
        .output;
    let plan_id = root
        .plan_snapshot()
        .await
        .expect("snapshot reads")
        .expect("plan exists")
        .plan_id;
    let control = PlanSubagentControl::new(
        root.inner.plan_controller.clone(),
        plan_id,
        update.client_key_ids["root"].clone(),
        started.attempt.attempt_id.clone(),
        started.lease.lease_id.clone(),
        subagent_session_id.clone(),
    );
    let subagent = Runtime::builder(subagent_session_id)
        .plan_subagent_control(control)
        .build()
        .expect("subagent runtime builds");

    let source_artifact = ArtifactRef::new(
        ArtifactId::new("subagent-exact-proof").expect("valid artifact id"),
        ArtifactKind::Text,
    );
    let source_artifact_id = source_artifact.id().clone();
    subagent
        .record_artifact(
            source_artifact.clone(),
            ArtifactContent::text("acceptance proved exactly\n"),
        )
        .await
        .expect("subagent artifact records");
    let source_evidence = subagent
        .evidence_ref(source_artifact.id(), EvidenceLocator::whole_artifact())
        .await
        .expect("subagent evidence validates");

    let unrelated_pending = pending_plan_tool(
        "call-root-still-in-flight",
        READ_PLAN_TOOL_NAME,
        &ReadPlanInput {
            plan_id: None,
            node_id: None,
            max_depth: None,
            include_attempts: None,
            include_leases: None,
            include_progress: None,
            include_directives: None,
            cursor: None,
        },
    );
    record_pending(&root, unrelated_pending).await;

    let terminal = pending_plan_tool(
        "call-subagent-report-with-artifact",
        REPORT_PLAN_ATTEMPT_TOOL_NAME,
        &ReportPlanAttemptInput {
            outcome: PlanAttemptOutcome::Completed,
            result: Some(PlanNodeResult {
                conclusion: "Subagent evidence is exact and durable".to_owned(),
                evidence_refs: vec![source_evidence],
                artifact_refs: vec![source_artifact],
                changed_paths: Vec::new(),
                verification: vec!["exact artifact content checked".to_owned()],
                open_questions: Vec::new(),
            }),
            diagnostic: None,
            decomposition: None,
            acknowledged_directive_ids: Vec::new(),
            applied_directive_ids: Vec::new(),
        },
    );
    record_pending(&subagent, terminal.clone()).await;
    let terminal_events = subagent
        .execute_tool_call(terminal.id(), ToolExecutionContext::default())
        .await
        .expect("subagent report tool executes");
    assert_eq!(
        resolved_tool_result(&terminal_events).status(),
        ToolCallResultStatus::Succeeded
    );

    let snapshot = root
        .plan_snapshot()
        .await
        .expect("root snapshot reads")
        .expect("root plan exists");
    let result = snapshot.nodes[0]
        .result
        .as_ref()
        .expect("root node completed with result");
    let promoted = result.artifact_refs[0].clone();
    assert_ne!(promoted.id(), &source_artifact_id);
    assert_eq!(result.evidence_refs[0].artifact_id, *promoted.id());
    assert_eq!(
        root.read_artifact_content(promoted.id())
            .await
            .expect("promoted root artifact is readable")
            .as_text(),
        Some("acceptance proved exactly\n")
    );

    let resumed = Runtime::builder(root_session_id)
        .coordinator_plan_tools()
        .resume_from_store(store)
        .await
        .expect("root runtime resumes from plan overlay");
    let resumed_snapshot = resumed
        .plan_snapshot()
        .await
        .expect("resumed snapshot reads")
        .expect("resumed plan exists");
    let resumed_result = resumed_snapshot.nodes[0]
        .result
        .as_ref()
        .expect("resumed result exists");
    assert_eq!(resumed_result.artifact_refs[0], promoted);
    assert_eq!(
        resumed
            .read_artifact_content(promoted.id())
            .await
            .expect("resumed promoted artifact is readable")
            .as_text(),
        Some("acceptance proved exactly\n")
    );
}

async fn runtime_with_empty_plan(session: &str) -> Runtime {
    let runtime = Runtime::builder(session_id(session))
        .coordinator_plan_tools()
        .build()
        .expect("runtime should build");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "activate plan mode".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activation succeeds");
    runtime
}

fn update_input(expected_plan_revision: u64, client_key: &str, objective: &str) -> UpdatePlanInput {
    UpdatePlanInput {
        reason: "define the plan".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: None,
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision,
            root: node(client_key, objective),
        },
    }
}

fn node(client_key: &str, objective: &str) -> PlanNodeInput {
    PlanNodeInput {
        id: None,
        client_key: Some(client_key.to_owned()),
        objective: objective.to_owned(),
        acceptance: vec![format!("{objective} is verified")],
        executor_policy: PlanExecutorPolicy::Auto,
        harness: PlanHarnessSnapshot::default(),
        recovery_policy: PlanRecoveryPolicySnapshot::default(),
        depends_on: Vec::new(),
        children: Vec::new(),
    }
}

fn optional_path_search_spec() -> ToolSpec {
    let schema = schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "query": { "type": "string" },
            "path": { "type": "string" }
        },
        "required": ["query"]
    }))
    .expect("search schema is valid");
    ToolSpec::new(
        ToolName::new("workspace_search_text").expect("valid tool name"),
        "Search workspace text with an optional path",
        ToolInputSchema::new(schema).expect("valid tool input schema"),
    )
    .expect("valid tool spec")
}

fn pending_plan_tool<T: Serialize>(call_id: &str, name: &str, input: &T) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(call_id).expect("valid call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolCallArguments::try_from(serde_json::to_value(input).expect("input serializes"))
            .expect("tool input is an object"),
    )
}

async fn record_pending(runtime: &Runtime, pending: PendingToolCall) {
    let mut session = runtime.inner.session.lock().await;
    session.record_session_started_if_needed();
    session
        .record_test_tool_call_pending(pending)
        .expect("pending tool call records");
}

async fn wait_for_live_local_attempt(runtime: &Runtime) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .is_some_and(|snapshot| {
                    snapshot
                        .attempts
                        .iter()
                        .any(|attempt| attempt.outcome.is_none() && attempt.lease_id.is_none())
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("local attempt starts");
}

async fn resolved_json(runtime: &Runtime, events: &[RuntimeJournalEvent]) -> serde_json::Value {
    let result = resolved_tool_result(events);
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("result artifact is readable");
    serde_json::from_str(content.as_text().expect("result artifact is JSON text"))
        .expect("result artifact parses")
}
