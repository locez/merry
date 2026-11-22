pub(super) use super::test_support::*;
use crate::{
    RegisteredTool, Runtime, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use merry_core::{PendingToolCall, SessionId, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{
    FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream, ModelName,
    ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest, ModelResponse,
    ModelStreamContext, ModelToolCall, ModelToolCallId, ReasoningEffort, ToolArguments,
    testing::FakeModelProvider,
};
use schemars::Schema;
use serde_json::{Map, json};
use std::{
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct FakeChildFactory {
    started: Arc<AtomicUsize>,
}

impl FakeChildFactory {
    fn new() -> Self {
        Self {
            started: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ChildRuntimeFactory for FakeChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .build()
    }
}

#[derive(Clone)]
struct FailsFirstChildFactory {
    calls: Arc<AtomicUsize>,
}

impl FailsFirstChildFactory {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ChildRuntimeFactory for FailsFirstChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            return Err(crate::RuntimeError::InvalidStepInput {
                reason: "test child factory failure",
            });
        }

        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .build()
    }
}

#[derive(Clone)]
struct PendingChildFactory {
    started: Arc<AtomicUsize>,
}

impl PendingChildFactory {
    fn new() -> Self {
        Self {
            started: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ChildRuntimeFactory for PendingChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        let started = self.started.fetch_add(1, Ordering::SeqCst);
        if started == 0 {
            return Runtime::builder(input.session_id)
                .task_anchor(input.task_anchor)
                .build();
        }

        let provider = PendingModelProvider::new();
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(provider),
                merry_llm::ModelName::new("fake/pending").expect("valid model name"),
            )
            .build()
    }
}

#[derive(Clone)]
struct AlwaysPendingChildFactory {
    started: Arc<AtomicUsize>,
}

impl AlwaysPendingChildFactory {
    fn new() -> Self {
        Self {
            started: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ChildRuntimeFactory for AlwaysPendingChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(PendingModelProvider::new()),
                ModelName::new("fake/pending").expect("valid model name"),
            )
            .build()
    }
}

struct PendingModelProvider {
    name: merry_core::ProviderName,
    capabilities: merry_llm::ModelCapabilities,
}

impl PendingModelProvider {
    fn new() -> Self {
        Self {
            name: merry_core::ProviderName::new("pending-model-provider")
                .expect("valid provider name"),
            capabilities: merry_llm::ModelCapabilities::new(true, true, false, false, None, None)
                .expect("valid capabilities"),
        }
    }
}

impl merry_llm::ModelProvider for PendingModelProvider {
    fn name(&self) -> &merry_core::ProviderName {
        &self.name
    }

    fn capabilities(&self) -> &merry_llm::ModelCapabilities {
        &self.capabilities
    }

    fn stream_model<'a>(
        &'a self,
        _request: merry_llm::ModelRequest,
        _context: merry_llm::ModelStreamContext,
    ) -> merry_llm::ModelProviderFuture<
        'a,
        Result<merry_llm::ModelEventStream, merry_llm::ModelError>,
    > {
        Box::pin(async move {
            let stream = futures_util::stream::pending::<
                Result<merry_llm::ModelEvent, merry_llm::ModelError>,
            >();
            Ok(Box::pin(stream) as merry_llm::ModelEventStream)
        })
    }
}

#[derive(Clone)]
struct RecordingModelChildFactory {
    provider: FakeModelProvider,
}

impl RecordingModelChildFactory {
    fn new() -> Self {
        Self {
            provider: FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("child done")],
                    FinishReason::Stop,
                    None,
                ),
            })]),
        }
    }

    fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.provider.recorded_requests()
    }
}

impl ChildRuntimeFactory for RecordingModelChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(self.provider.clone()),
                ModelName::new("fake/recording-child").expect("valid model name"),
            )
            .build()
    }
}

struct ReportingChildFactory;

impl ChildRuntimeFactory for ReportingChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        let provider = ScriptedStepProvider::new(vec![
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::tool_call(ModelToolCall::new(
                        ModelToolCallId::new("call-child-patch").expect("valid call id"),
                        ToolName::new("apply_patch").expect("valid tool name"),
                        ToolArguments::new(Map::new()),
                    ))],
                    FinishReason::ToolCalls,
                    None,
                ),
            })],
            vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text(
                        "Patched subagent-output.txt to status: done.",
                    )],
                    FinishReason::Stop,
                    None,
                ),
            })],
        ]);

        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(provider),
                ModelName::new("fake/reporting-child").expect("valid model name"),
            )
            .register_tool(RegisteredTool::read_only(
                apply_patch_tool_spec(),
                Arc::new(FakeWorkspacePatchExecutor),
            ))
            .build()
    }
}

type ScriptedStepEvents = Vec<Result<ModelEvent, ModelError>>;
type ScriptedStepResponses = Vec<ScriptedStepEvents>;

struct ScriptedStepProvider {
    name: merry_core::ProviderName,
    capabilities: ModelCapabilities,
    responses: Arc<StdMutex<ScriptedStepResponses>>,
    release: Option<CancellationToken>,
}

impl ScriptedStepProvider {
    fn new(responses: ScriptedStepResponses) -> Self {
        Self {
            name: merry_core::ProviderName::new("scripted-step-provider")
                .expect("valid provider name"),
            capabilities: ModelCapabilities::new(true, true, false, true, None, None)
                .expect("valid capabilities"),
            responses: Arc::new(StdMutex::new(responses.into_iter().rev().collect())),
            release: None,
        }
    }

    fn with_release(responses: ScriptedStepResponses, release: CancellationToken) -> Self {
        Self {
            release: Some(release),
            ..Self::new(responses)
        }
    }
}

impl ModelProvider for ScriptedStepProvider {
    fn name(&self) -> &merry_core::ProviderName {
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
        let release = self.release.clone();
        Box::pin(async move {
            if let Some(release) = release {
                release.cancelled().await;
            }
            let _ = request;
            let events = self
                .responses
                .lock()
                .expect("scripted provider response mutex should not be poisoned")
                .pop()
                .expect("scripted child provider should have a response for each step");
            Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
        })
    }
}

struct FakeWorkspacePatchExecutor;

impl ToolExecutor for FakeWorkspacePatchExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async {
            Ok(ToolExecutionOutcome::succeeded_json(
                json!({
                    "ok": true,
                    "tool": "apply_patch",
                    "changes": [{
                        "path": "subagent-output.txt",
                        "hunks": 1
                    }]
                })
                .to_string(),
            ))
        })
    }
}

fn apply_patch_tool_spec() -> ToolSpec {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("test schema should be a JSON schema");
    ToolSpec::new(
        ToolName::new("apply_patch").expect("valid tool name"),
        "Apply a workspace patch.",
        ToolInputSchema::new(schema).expect("valid schema"),
    )
    .expect("valid tool spec")
}

mod policy;
mod results;
mod scheduler;

struct GateChildFactory {
    release: CancellationToken,
}

impl ChildRuntimeFactory for GateChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        let provider = ScriptedStepProvider::with_release(
            vec![vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("gated child finished")],
                    FinishReason::Stop,
                    None,
                ),
            })]],
            self.release.clone(),
        );
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(provider),
                ModelName::new("fake/gated-child").expect("valid model"),
            )
            .build()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_reports_partial_rejections_alongside_spawned_tasks() {
    let release = CancellationToken::new();
    release.cancel();
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(GateChildFactory { release });
    let config = SubagentConfig::new(2, 1)
        .expect("valid config")
        .with_model_turn_bounds(2, 10)
        .expect("valid bounds");
    let manager = SubagentManager::new(
        SessionId::new("partial-reject-parent").expect("valid session id"),
        config,
        factory,
    );

    let valid = SubagentTaskSpec::new("Valid child task.", 4)
        .expect("valid task")
        .with_write_scope(Vec::<&str>::new())
        .expect("valid empty write scope");
    let invalid_budget = SubagentTaskSpec::new("Out-of-bounds budget task.", 1)
        .expect("valid spec")
        .with_write_scope(Vec::<&str>::new())
        .expect("valid empty write scope");

    let output = manager
        .spawn(vec![valid, invalid_budget], None, CancellationToken::new())
        .await
        .expect("spawn should succeed with partial rejections");
    assert_eq!(output.spawned.len(), 1);
    assert_eq!(output.rejected.len(), 1);
    assert_eq!(output.rejected[0].task_index, 1);
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_tool_decouples_child_cancellation_from_tool_phase_token() {
    let release = CancellationToken::new();
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(GateChildFactory {
        release: release.clone(),
    });
    let manager = SubagentManager::new(
        SessionId::new("decoupled-phase-parent").expect("valid session id"),
        SubagentConfig::new(2, 1).expect("valid config"),
        factory,
    );
    let [spawn_tool, _, _, _] =
        crate::subagent_registered_tools(manager.clone()).expect("subagent tools build");
    let phase_token = CancellationToken::new();
    let call = PendingToolCall::new(
        merry_core::ToolCallId::new("spawn-call-1").expect("valid call id"),
        ToolName::new("spawn_subagents").expect("valid tool name"),
        merry_core::ToolCallArguments::try_from(json!({
            "tasks": [{
                "task": "Wait on gate.",
                "write_scope": []
            }]
        }))
        .expect("valid arguments"),
    );

    let outcome = spawn_tool
        .executor()
        .execute(call, ToolExecutionContext::new(phase_token.clone()))
        .await
        .expect("spawn tool should succeed");
    assert_eq!(
        outcome.status(),
        merry_core::ToolCallResultStatus::Succeeded
    );

    // Cancelling the tool phase token after spawn must NOT cancel the child.
    phase_token.cancel();
    tokio::task::yield_now().await;
    let snapshot = manager.snapshot().await;
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].status, SubagentStatusLabel::Running);

    release.cancel();
    let waited = manager
        .wait(&[snapshot[0].agent_id.clone()], WaitMode::All, None)
        .await
        .expect("wait should complete");
    assert_eq!(waited.agents[0].status, SubagentStatusLabel::Completed);
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_cancels_active_children_and_reclaims_handles() {
    let release = CancellationToken::new();
    let factory: Arc<dyn ChildRuntimeFactory> = Arc::new(GateChildFactory { release });
    let manager = SubagentManager::new(
        SessionId::new("shutdown-parent").expect("valid session id"),
        SubagentConfig::new(2, 1).expect("valid config"),
        factory,
    );
    let task = SubagentTaskSpec::new("Long running task.", 4)
        .expect("valid task")
        .with_write_scope(Vec::<&str>::new())
        .expect("valid scope");
    let spawned = manager
        .spawn(vec![task], None, CancellationToken::new())
        .await
        .expect("spawn should succeed");
    assert_eq!(spawned.spawned.len(), 1);
    assert!(manager.has_active_agents().await);

    manager.shutdown().await;
    assert!(!manager.has_active_agents().await);
    let snapshot = manager.snapshot().await;
    assert_eq!(snapshot[0].status, SubagentStatusLabel::Cancelled);
}

#[tokio::test(flavor = "current_thread")]
async fn non_interactive_agent_loop_waits_for_active_subagents_before_completing() {
    let release = CancellationToken::new();
    let child_factory: Arc<dyn ChildRuntimeFactory> = Arc::new(GateChildFactory {
        release: release.clone(),
    });
    let parent_provider = ScriptedStepProvider::new(vec![
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-spawn").expect("valid call id"),
                    ToolName::new("spawn_subagents").expect("valid tool name"),
                    ToolArguments::try_from(json!({
                        "tasks": [{
                            "task": "Slow child task.",
                            "write_scope": []
                        }]
                    }))
                    .expect("valid arguments"),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("Spawned child, waiting for it.")],
                FinishReason::Stop,
                None,
            ),
        })],
        vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("Child finished; parent done.")],
                FinishReason::Stop,
                None,
            ),
        })],
    ]);
    let session_id = SessionId::new("non-interactive-wait-parent").expect("valid session id");
    let manager = SubagentManager::new(
        session_id.clone(),
        SubagentConfig::new(2, 1).expect("valid config"),
        child_factory,
    );
    let [spawn_tool, send_tool, wait_tool, cancel_tool] =
        crate::subagent_registered_tools(manager.clone()).expect("subagent tools build");
    let runtime = Runtime::builder(session_id)
        .model_provider(
            Arc::new(parent_provider),
            ModelName::new("fake/parent").expect("valid model"),
        )
        .subagent_manager(manager)
        .register_tool(spawn_tool)
        .register_tool(send_tool)
        .register_tool(wait_tool)
        .register_tool(cancel_tool)
        .build()
        .expect("parent runtime should build");

    let releaser = tokio::spawn({
        let release = release.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            release.cancel();
        }
    });

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runtime.run_agent_loop(
            crate::StepInput::user_text("Delegate and wait.").expect("valid input"),
            crate::StepContext::default(),
            crate::AgentLoopConfig::new(6).expect("valid loop config"),
        ),
    )
    .await
    .expect("parent loop should not hang")
    .expect("parent loop should succeed");
    releaser.await.expect("releaser should finish");

    assert_eq!(result.status(), &crate::AgentLoopStatus::Completed);
    assert_eq!(result.model_turns_run(), 3);
    assert_eq!(
        result.final_output().map(str::trim),
        Some("Child finished; parent done.")
    );
}
