use futures_util::StreamExt;
use merry_core::{
    InteractiveRunState, PendingToolCall, PlanDocumentStatus, PlanModeState, PlanReviewDecision,
    ProviderName, RuntimeEvent, RuntimeJournalPayload, SessionId, TodoStatus, ToolCallResultStatus,
    ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ToolArguments,
};
use merry_runtime::{
    AgentLoopConfig, FileSessionStore, LoadedSession, PermissionReviewMode, RegisteredTool,
    Runtime, RuntimeTrustLevel, StepContext, StepInput, ToolActionKind, ToolExecutionContext,
    ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture, UserMessageInput,
};
use schemars::Schema;
use serde_json::{Map, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::tempdir;

struct ScriptedProvider {
    name: ProviderName,
    capabilities: ModelCapabilities,
    responses: Mutex<VecDeque<ModelResponse>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            name: ProviderName::new("plan-test-provider").expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            responses: Mutex::new(VecDeque::from(responses)),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .expect("requests mutex not poisoned")
            .clone()
    }
}

impl ModelProvider for ScriptedProvider {
    fn name(&self) -> &ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("requests mutex not poisoned")
                .push(request);
            let response = self
                .responses
                .lock()
                .expect("responses mutex not poisoned")
                .pop_front()
                .expect("scripted provider has response");
            let events = vec![Ok(ModelEvent::Completed { response })];
            Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
        })
    }
}

struct CountingWriteExecutor {
    calls: Arc<AtomicUsize>,
}

impl ToolExecutor for CountingWriteExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolExecutionOutcome::succeeded_json(
                json!({ "ok": true }).to_string(),
            ))
        })
    }
}

fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ModelOutput {
    let map: Map<String, serde_json::Value> = args
        .as_object()
        .cloned()
        .expect("tool call arguments must be an object");
    ModelOutput::tool_call(ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::new(map),
    ))
}

fn workspace_write_tool(calls: Arc<AtomicUsize>) -> RegisteredTool {
    let schema = Schema::try_from(json!({ "type": "object" })).expect("valid schema");
    let spec = ToolSpec::new(
        ToolName::new("apply_patch").expect("valid tool name"),
        "Apply a patch to the workspace.",
        ToolInputSchema::new(schema).expect("valid input schema"),
    )
    .expect("valid tool spec");
    RegisteredTool::new(
        spec,
        Arc::new(CountingWriteExecutor { calls }),
        ToolActionKind::WorkspaceWrite,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn plan_mode_blocks_workspace_writes_even_in_fully_trusted_mode_until_approved() {
    let write_calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(ScriptedProvider::new(vec![
        // Turn 1: model tries apply_patch while Plan Mode is active -> blocked by gate
        ModelResponse::new(
            vec![tool_call("call-write-blocked", "apply_patch", json!({}))],
            FinishReason::ToolCalls,
            None,
        ),
        // Turn 2: model writes a plan and requests approval
        ModelResponse::new(
            vec![
                tool_call(
                    "call-write-plan",
                    "write_plan",
                    json!({
                        "title": "Auth Cleanup",
                        "content": "# Auth Cleanup\n\n- Step 1: refactor handler\n"
                    }),
                ),
                tool_call("call-exit-plan", "exit_plan_mode", json!({})),
            ],
            FinishReason::ToolCalls,
            None,
        ),
        // Turn 3: after user approves plan, apply_patch succeeds
        ModelResponse::new(
            vec![tool_call("call-write-allowed", "apply_patch", json!({}))],
            FinishReason::ToolCalls,
            None,
        ),
    ]));

    let runtime = Runtime::builder(SessionId::new("plan-gate-test").expect("valid id"))
        .model_provider(
            provider.clone(),
            ModelName::new("test/model").expect("valid model"),
        )
        .runtime_trust_level(RuntimeTrustLevel::TrustedSdk)
        .permission_review_mode(PermissionReviewMode::FullyTrusted)
        .allow_low_risk_apply_patches()
        .coordinator_plan_tools()
        .register_tool(workspace_write_tool(Arc::clone(&write_calls)))
        .build()
        .expect("runtime builds");

    runtime
        .set_plan_mode(true, true)
        .await
        .expect("plan mode enables");
    assert_eq!(
        runtime.plan_session_snapshot().await.mode,
        PlanModeState::Active
    );

    // Step 1: model requests apply_patch
    let _ = runtime
        .step(
            StepInput::user_text("Refactor auth").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts")
        .collect::<Vec<_>>()
        .await;
    let pending = runtime.pending_tool_calls().await;
    assert_eq!(pending.len(), 1);
    let events = runtime
        .execute_tool_call(pending[0].id(), ToolExecutionContext::default())
        .await
        .expect("blocked tool call still resolves");
    assert_eq!(
        write_calls.load(Ordering::SeqCst),
        0,
        "executor must not run"
    );
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        RuntimeJournalPayload::ToolCallResolved { result, .. }
            if result.status() == ToolCallResultStatus::Failed
                && result.diagnostic().map(|d| d.code()) == Some("plan_mode_read_only")
    )));

    // Step 2: model writes plan and exits plan mode for approval
    let _ = runtime
        .step(
            StepInput::user_text("draft plan").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts")
        .collect::<Vec<_>>()
        .await;
    for call in runtime.pending_tool_calls().await {
        let _ = runtime
            .execute_tool_call(call.id(), ToolExecutionContext::default())
            .await
            .expect("plan tool executes");
    }
    assert!(runtime.awaiting_plan_approval().await);
    let plan_id = runtime
        .plan_session_snapshot()
        .await
        .active_plan_id
        .expect("active plan exists");

    // User approves plan -> Plan Mode becomes Inactive
    runtime
        .approve_plan(plan_id, None)
        .await
        .expect("approve succeeds");
    assert_eq!(
        runtime.plan_session_snapshot().await.mode,
        PlanModeState::Inactive
    );

    // Step 3: apply_patch now runs cleanly
    let _ = runtime
        .step(
            StepInput::user_text("apply now").expect("valid input"),
            StepContext::default(),
        )
        .expect("step starts")
        .collect::<Vec<_>>()
        .await;
    let pending = runtime.pending_tool_calls().await;
    assert_eq!(pending.len(), 1);
    let events = runtime
        .execute_tool_call(pending[0].id(), ToolExecutionContext::default())
        .await
        .expect("apply_patch executes");
    assert_eq!(write_calls.load(Ordering::SeqCst), 1);
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        RuntimeJournalPayload::ToolCallResolved { result, .. }
            if result.status() == ToolCallResultStatus::Succeeded
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn multi_plan_files_and_todos_persist_and_resume() {
    let dir = tempdir().expect("tempdir");
    let store = FileSessionStore::new(dir.path());
    let session_id = SessionId::new("multi-plan-resume").expect("valid session id");

    let provider = Arc::new(ScriptedProvider::new(vec![
        ModelResponse::new(
            vec![
                tool_call(
                    "call-plan-1",
                    "write_plan",
                    json!({
                        "title": "First Plan",
                        "content": "# First Plan\n\nDetails one."
                    }),
                ),
                tool_call(
                    "call-todos",
                    "update_todos",
                    json!({
                        "todos": [
                            { "id": "t1", "content": "Inspect code", "status": "completed" },
                            { "id": "t2", "content": "Implement fix", "status": "in_progress" }
                        ]
                    }),
                ),
            ],
            FinishReason::ToolCalls,
            None,
        ),
        ModelResponse::new(
            vec![tool_call(
                "call-plan-2",
                "write_plan",
                json!({
                    "title": "Second Plan",
                    "content": "# Second Plan\n\nDetails two.",
                    "create_new": true
                }),
            )],
            FinishReason::ToolCalls,
            None,
        ),
    ]));

    let runtime = Runtime::builder(session_id.clone())
        .session_store(store.clone())
        .model_provider(provider, ModelName::new("test/model").expect("valid model"))
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");

    let _ = runtime
        .step(
            StepInput::user_text("Create two plans and todos").expect("valid input"),
            StepContext::default(),
        )
        .expect("step 1")
        .collect::<Vec<_>>()
        .await;
    for call in runtime.pending_tool_calls().await {
        runtime
            .execute_tool_call(call.id(), ToolExecutionContext::default())
            .await
            .expect("tool executes");
    }

    let _ = runtime
        .step(
            StepInput::user_text("Create second plan").expect("valid input"),
            StepContext::default(),
        )
        .expect("step 2")
        .collect::<Vec<_>>()
        .await;
    for call in runtime.pending_tool_calls().await {
        runtime
            .execute_tool_call(call.id(), ToolExecutionContext::default())
            .await
            .expect("tool executes");
    }

    // Verify Markdown files written under <session_dir>/plans/
    let docs = runtime.plan_documents().await;
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0].summary.file_name, "001-first-plan.md");
    assert_eq!(docs[1].summary.file_name, "002-second-plan.md");
    let plans_dir = dir.path().join(session_id.as_str()).join("plans");
    assert_eq!(
        tokio::fs::read_to_string(plans_dir.join("001-first-plan.md"))
            .await
            .expect("read plan 1"),
        "# First Plan\n\nDetails one."
    );
    assert_eq!(
        tokio::fs::read_to_string(plans_dir.join("002-second-plan.md"))
            .await
            .expect("read plan 2"),
        "# Second Plan\n\nDetails two."
    );

    // Resume from store directly without any legacy compatibility shims
    let loaded = LoadedSession::load(&store, &session_id)
        .await
        .expect("session resumes cleanly");
    let resumed = Runtime::builder(session_id)
        .session_store(store)
        .with_loaded_session(loaded)
        .coordinator_plan_tools()
        .build()
        .expect("resumed runtime builds");

    let resumed_docs = resumed.plan_documents().await;
    assert_eq!(resumed_docs.len(), 2);
    assert_eq!(resumed_docs[0].content, "# First Plan\n\nDetails one.");
    assert_eq!(resumed_docs[1].content, "# Second Plan\n\nDetails two.");

    let resumed_todos = resumed.todo_items().await;
    assert_eq!(resumed_todos.len(), 2);
    assert_eq!(resumed_todos[0].status, TodoStatus::Completed);
    assert_eq!(resumed_todos[1].status, TodoStatus::InProgress);
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_loop_pauses_on_exit_plan_mode_supports_revise_and_approve() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        // 1. Model drafts plan v1 and calls exit_plan_mode
        ModelResponse::new(
            vec![
                tool_call(
                    "c1",
                    "write_plan",
                    json!({ "title": "API Migration", "content": "# API Migration\n\nv1" }),
                ),
                tool_call("c2", "exit_plan_mode", json!({})),
            ],
            FinishReason::ToolCalls,
            None,
        ),
        // 2. After request_plan_changes, model revises plan to v2 and calls exit_plan_mode
        ModelResponse::new(
            vec![
                tool_call(
                    "c3",
                    "write_plan",
                    json!({ "title": "API Migration", "content": "# API Migration\n\nv2 with tests" }),
                ),
                tool_call("c4", "exit_plan_mode", json!({})),
            ],
            FinishReason::ToolCalls,
            None,
        ),
        // 3. After approve_plan, model executes and completes
        ModelResponse::new(
            vec![ModelOutput::text("Implemented API migration with tests.")],
            FinishReason::Stop,
            None,
        ),
    ]));

    let runtime = Runtime::builder(SessionId::new("interactive-plan-review").expect("valid id"))
        .model_provider(
            provider.clone(),
            ModelName::new("test/model").expect("valid model"),
        )
        .coordinator_plan_tools()
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::default(),
            AgentLoopConfig::new(16).expect("valid config"),
        )
        .expect("interactive loop starts");
    let (mut stream, input, control) = run.split();

    // Drain initial WaitingForInput
    let initial = stream
        .next_event()
        .await
        .expect("stream ok")
        .expect("initial state");
    assert!(matches!(
        initial,
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));

    // Toggle Plan Mode on before first prompt
    control
        .set_plan_mode(true)
        .await
        .expect("set_plan_mode queued");
    input
        .submit_message(
            UserMessageInput::new("Plan the API migration", Vec::new()).expect("valid text"),
        )
        .await
        .expect("input queued");

    // Wait for PlanApprovalRequested and WaitingForInput
    let mut plan_id_v1 = None;
    let mut saw_waiting = false;
    while let Some(event) = stream.next_event().await.expect("next event") {
        match event {
            RuntimeEvent::PlanApprovalRequested { plan, .. } => {
                assert_eq!(plan.summary.revision, 1);
                plan_id_v1 = Some(plan.summary.id);
            }
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if plan_id_v1.is_some() => {
                saw_waiting = true;
                break;
            }
            _ => {}
        }
    }
    let plan_id = plan_id_v1.expect("plan v1 requested approval");
    assert!(saw_waiting);
    assert!(runtime.awaiting_plan_approval().await);

    // Request changes -> model revises to v2 and pauses again
    control
        .request_plan_changes(plan_id.clone(), "Add integration tests".to_owned())
        .await
        .expect("request_plan_changes sent");

    let mut saw_v2 = false;
    let mut saw_waiting_v2 = false;
    while let Some(event) = stream.next_event().await.expect("next event") {
        match event {
            RuntimeEvent::PlanApprovalRequested { plan, .. } if plan.summary.revision == 2 => {
                saw_v2 = true;
            }
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if saw_v2 => {
                saw_waiting_v2 = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_v2 && saw_waiting_v2);

    // Approve with optional inline comment note -> exits Plan Mode and runs implementation
    control
        .approve_plan(plan_id.clone(), Some("Keep PR small.".to_owned()))
        .await
        .expect("approve_plan sent");

    let mut saw_approved = false;
    let mut saw_final_text = false;
    while let Some(event) = stream.next_event().await.expect("next event") {
        match event {
            RuntimeEvent::PlanReviewed {
                decision: PlanReviewDecision::Approved { .. },
                ..
            } => {
                saw_approved = true;
            }
            RuntimeEvent::AssistantMessage { text, .. }
                if text.contains("Implemented API migration") =>
            {
                saw_final_text = true;
            }
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput,
            } if saw_final_text => {
                break;
            }
            _ => {}
        }
    }
    assert!(saw_approved && saw_final_text);
    assert_eq!(
        runtime.plan_session_snapshot().await.mode,
        PlanModeState::Inactive
    );
    let docs = runtime.plan_documents().await;
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].summary.status, PlanDocumentStatus::Approved);

    // Verify tool specs stayed identical across all 3 turns (KV cache stability)
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].tools(), requests[1].tools());
    assert_eq!(requests[1].tools(), requests[2].tools());

    control.close().await.expect("session closes");
}
