use super::*;
use crate::plan::{
    BeginPlanInput, ControlPlanAttemptInput, PlanChangeInput, PlanExecutionIntent, PlanNodeInput,
    ReadPlanInput, ReportPlanAttemptInput, ReportPlanProgressInput, UpdatePlanInput,
    execution::PlanAttemptActor,
    tools::{
        BEGIN_PLAN_TOOL_NAME, CONTROL_PLAN_ATTEMPT_TOOL_NAME, READ_PLAN_TOOL_NAME,
        REPORT_PLAN_ATTEMPT_TOOL_NAME, REPORT_PLAN_PROGRESS_TOOL_NAME, UPDATE_PLAN_TOOL_NAME,
    },
};
use merry_core::{
    PlanAttemptOutcome, PlanCapabilityEnvelopeSnapshot, PlanDirectiveConstraints,
    PlanDirectiveKind, PlanExecutorPolicy, PlanHarnessSnapshot, PlanNodeResult,
    PlanRecoveryPolicySnapshot, ToolCallArguments,
};
use serde::Serialize;

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
            expected_lease_id: started.lease.lease_id.clone(),
            expected_node_revision: started.lease.node_revision,
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
            lease_id: started.lease.lease_id.clone(),
            expected_node_revision: started.lease.node_revision,
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
            lease_id: started.lease.lease_id,
            expected_node_revision: started.lease.node_revision,
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

async fn resolved_json(runtime: &Runtime, events: &[RuntimeJournalEvent]) -> serde_json::Value {
    let result = resolved_tool_result(events);
    let content = runtime
        .read_artifact_content(result.artifact().id())
        .await
        .expect("result artifact is readable");
    serde_json::from_str(content.as_text().expect("result artifact is JSON text"))
        .expect("result artifact parses")
}
