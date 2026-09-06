use crate::support::{
    models::{
        BlockingFirstProvider, RecordingProvider, completed_text_event, completed_tool_call_event,
        model_name,
    },
    runtime::{session_id, wait_for_interactive_waiting},
};
use merry_core::{
    PlanActivationSource, PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanHarnessSnapshot,
    PlanPhase, PlanRecoveryPolicySnapshot, RuntimeEvent, ToolName,
};
use merry_llm::{ModelMessageRole, ModelToolCall, ModelToolCallId, ToolArguments};
use merry_runtime::{
    AgentLoopConfig, AutomaticCompactionConfig, BeginPlanInput, FileSessionStore, InteractiveError,
    PlanApprovalInput, PlanChangeInput, PlanExecutionIntent, PlanNodeInput, Runtime, StepContext,
    UpdatePlanInput,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::{
    sync::oneshot,
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn interactive_plan_mode_control_commits_and_streams_the_plan_snapshot() {
    let runtime = Runtime::builder(session_id("interactive-enter-plan"))
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    control
        .enter_plan_mode("user requested explicit planning")
        .await
        .expect("plan mode control succeeds");
    let plan_event = timeout(Duration::from_secs(1), async {
        loop {
            let event = stream
                .next_event()
                .await
                .expect("stream error")
                .expect("interactive event");
            if matches!(event, RuntimeEvent::PlanUpdated { .. }) {
                break event;
            }
        }
    })
    .await
    .expect("plan event should be streamed");
    let RuntimeEvent::PlanUpdated { snapshot, .. } = plan_event else {
        unreachable!("matched plan event")
    };
    assert_eq!(snapshot.activation_source, PlanActivationSource::User);
    assert_eq!(
        runtime
            .plan_snapshot()
            .await
            .expect("snapshot read succeeds")
            .expect("plan exists"),
        snapshot
    );
}

#[tokio::test]
async fn interactive_plan_controls_reject_while_the_main_model_phase_is_running() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-plan-control-running"))
        .model_provider(Arc::new(provider), model_name())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");
    input
        .submit_next("start blocking work")
        .await
        .expect("queued");
    started_rx.await.expect("provider request starts");

    let error = control
        .enter_plan_mode("must wait for the safe boundary")
        .await
        .expect_err("running model phase rejects plan control");
    assert!(matches!(error, InteractiveError::PlanControlRequiresIdle));
    let error = control
        .retry_interrupted_plan_node(
            merry_core::PlanNodeId::new("interrupted-node").expect("valid node id"),
            "must also wait for the safe boundary",
        )
        .await
        .expect_err("running model phase rejects retry control");
    assert!(matches!(error, InteractiveError::PlanControlRequiresIdle));
    let temp = tempfile::tempdir().expect("tempdir");
    let error = control
        .save_session_to(FileSessionStore::new(temp.path()))
        .await
        .expect_err("running model phase rejects session save");
    assert!(matches!(error, InteractiveError::SessionSaveRequiresIdle));
    release_tx.send(()).expect("provider release succeeds");
}

#[tokio::test]
async fn interactive_run_stops_before_another_model_turn_when_plan_awaits_approval() {
    let mut review_plan = local_plan_input();
    review_plan.execution_intent = PlanExecutionIntent::RequestUserReview;
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(ModelToolCall::new(
            ModelToolCallId::new("call-plan-review").expect("valid call id"),
            ToolName::new("update_plan").expect("valid tool name"),
            ToolArguments::try_from(
                serde_json::to_value(review_plan).expect("review plan serializes"),
            )
            .expect("review plan arguments are an object"),
        )))],
        vec![Ok(completed_text_event(
            "this turn must not run before structured approval",
        ))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-plan-awaiting-approval-boundary"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "prepare an explicitly reviewed plan".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input
        .submit_next("Create the plan, then wait for my approval")
        .await
        .expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    assert_eq!(
        provider.recorded_requests().len(),
        1,
        "the tool continuation must stop at the user approval boundary"
    );
    assert_eq!(
        runtime
            .plan_snapshot()
            .await
            .expect("plan snapshot read succeeds")
            .expect("plan exists")
            .phase,
        PlanPhase::AwaitingApproval
    );
}

#[tokio::test]
async fn interactive_run_stops_before_another_model_turn_for_a_non_empty_planning_draft() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(ModelToolCall::new(
            ModelToolCallId::new("call-planning-draft").expect("valid call id"),
            ToolName::new("update_plan").expect("valid tool name"),
            ToolArguments::try_from(
                serde_json::to_value(local_plan_input()).expect("planning draft serializes"),
            )
            .expect("planning draft arguments are an object"),
        )))],
        vec![Ok(completed_text_event(
            "this turn must wait for structured draft approval",
        ))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-planning-draft-boundary"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "prepare a planning draft".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input
        .submit_next("Create a draft and let me approve it")
        .await
        .expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    assert_eq!(provider.recorded_requests().len(), 1);
    assert_eq!(
        runtime
            .plan_snapshot()
            .await
            .expect("plan snapshot read succeeds")
            .expect("plan exists")
            .phase,
        PlanPhase::Planning
    );
}

#[tokio::test]
async fn plan_approval_triggers_a_model_continuation_with_explicit_approval() {
    let mut review_plan = local_plan_input();
    review_plan.execution_intent = PlanExecutionIntent::RequestUserReview;
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(ModelToolCall::new(
            ModelToolCallId::new("call-plan-approval-continuation").expect("valid call id"),
            ToolName::new("update_plan").expect("valid tool name"),
            ToolArguments::try_from(
                serde_json::to_value(review_plan).expect("review plan serializes"),
            )
            .expect("review plan arguments are an object"),
        )))],
        vec![Ok(completed_text_event("The approved plan can proceed."))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-plan-approval-continuation"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");
    input
        .submit_next("Create the plan and wait for approval")
        .await
        .expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("plan exists");
    control
        .approve_plan(PlanApprovalInput {
            plan_id: snapshot.plan_id,
            expected_plan_revision: snapshot.revision,
            review_resolution_ref: "user approved through the Plan UI".to_owned(),
            capability_envelope: Some(PlanCapabilityEnvelopeSnapshot::default()),
            authorization_refs: vec!["interactive Plan approval".to_owned()],
            requirement_resolution_refs: BTreeMap::new(),
        })
        .await
        .expect("structured plan approval succeeds");

    tokio::time::timeout(
        Duration::from_secs(2),
        wait_for_interactive_waiting(&mut stream),
    )
    .await
    .expect("approval should trigger a continuation model turn");

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let approval_text = requests[1]
        .messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::User)
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(approval_text.contains("approved"));
}

#[tokio::test]
async fn interactive_run_continues_when_user_already_authorized_plan_execution() {
    let mut executable_plan = local_plan_input();
    executable_plan.execution_intent = PlanExecutionIntent::ExecuteIfAuthorized;
    let PlanChangeInput::DefinePlan { root, .. } = &mut executable_plan.change else {
        panic!("local plan fixture must define a plan");
    };
    root.executor_policy = PlanExecutorPolicy::Delegate;
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(ModelToolCall::new(
            ModelToolCallId::new("call-plan-execute").expect("valid call id"),
            ToolName::new("update_plan").expect("valid tool name"),
            ToolArguments::try_from(
                serde_json::to_value(executable_plan).expect("executable plan serializes"),
            )
            .expect("executable plan arguments are an object"),
        )))],
        vec![Ok(completed_text_event(
            "The authorized plan has entered execution.",
        ))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-plan-preauthorized-execution"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "the user asked to plan and execute".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream
        .next_event()
        .await
        .expect("stream error")
        .expect("waiting state");

    input
        .submit_next("Use this plan and execute it")
        .await
        .expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    assert_eq!(
        provider.recorded_requests().len(),
        2,
        "pre-authorized execution must continue after update_plan"
    );
    assert_eq!(
        runtime
            .plan_snapshot()
            .await
            .expect("plan snapshot read succeeds")
            .expect("plan exists")
            .phase,
        PlanPhase::Executing
    );
}

fn local_plan_input() -> UpdatePlanInput {
    UpdatePlanInput {
        reason: "define local coordinator leaf".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Complete local coordinator verification".to_owned(),
                acceptance: vec!["local verification is durable".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Local,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    }
}
