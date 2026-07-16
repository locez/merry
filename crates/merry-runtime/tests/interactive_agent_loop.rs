use futures_util::{StreamExt, stream};
use merry_core::{
    InteractiveRunState, ModelUsage, PendingToolCall, PlanActivationSource, PlanAttemptOutcome,
    PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanHarnessSnapshot, PlanNodeResult,
    PlanNodeStatus, PlanPhase, PlanRecoveryPolicySnapshot, ProviderName, QueuedInputLane,
    RuntimeEvent, SessionId, ToolInputSchema, ToolName, ToolSpec,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelCapabilities, ModelError, ModelEvent, ModelEventStream,
    ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
    ModelResponse, ModelRetryPolicy, ModelStreamContext, ModelToolCall, ModelToolCallId,
    ReasoningEffort, ToolArguments,
};
use merry_runtime::{
    AgentLoopConfig, AutomaticCompactionConfig, BeginPlanInput, ChildRuntimeFactory,
    ChildRuntimeInput, CitationCompactionPolicy, FileSessionStore, InteractiveError,
    InteractivePrimaryModel, InteractiveSettingsUpdate, InteractiveSubagentSettings,
    InterruptReason, PlanApprovalInput, PlanChangeInput, PlanExecutionIntent, PlanNodeInput,
    ReportPlanAttemptInput, Runtime, SessionTranscriptItem, StepContext, SubagentConfig,
    SubagentManager, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture, UpdatePlanInput, subagent_registered_tools,
};
use schemars::Schema;
use serde_json::json;
use std::{
    collections::BTreeMap,
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    sync::{Barrier, Mutex as AsyncMutex, oneshot},
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("valid session id")
}

fn model_name() -> ModelName {
    ModelName::new("fake/model").expect("valid model name")
}

type ScriptedModelEvents = Vec<Result<ModelEvent, ModelError>>;
type ScriptedProviderSteps = Vec<ScriptedModelEvents>;

fn completed_text_event(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

fn completed_text_event_with_usage(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::text(text)],
            FinishReason::Stop,
            Some(ModelUsage::new(100, 10)),
        ),
    }
}

fn model_tool_call(id: &str, name: &str) -> ModelToolCall {
    ModelToolCall::new(
        ModelToolCallId::new(id).expect("valid model tool call id"),
        ToolName::new(name).expect("valid tool name"),
        ToolArguments::try_from(json!({"query": "test query"})).expect("valid model arguments"),
    )
}

fn completed_tool_call_event(call: ModelToolCall) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    }
}

fn completed_tool_call_batch_event(calls: Vec<ModelToolCall>) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(
            calls.into_iter().map(ModelOutput::tool_call).collect(),
            FinishReason::ToolCalls,
            None,
        ),
    }
}

fn tool_spec(name: &str) -> ToolSpec {
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "query": { "type": "string" }
        },
        "required": ["query"]
    }))
    .expect("test schema should be valid JSON schema");

    ToolSpec::new(
        ToolName::new(name).expect("valid tool name"),
        "Search test notes",
        ToolInputSchema::new(schema).expect("valid tool schema"),
    )
    .expect("valid tool spec")
}

#[derive(Clone)]
struct RecordingProvider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    steps: Arc<Mutex<ScriptedProviderSteps>>,
}

#[derive(Clone)]
struct NoopChildFactory;

impl ChildRuntimeFactory for NoopChildFactory {
    fn build_child(
        &self,
        input: ChildRuntimeInput,
    ) -> Result<Runtime, merry_runtime::RuntimeError> {
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .build()
    }
}

struct PlanLifecycleChildFactory {
    report_terminal_attempt: bool,
    attempts_started: Arc<AtomicUsize>,
}

impl ChildRuntimeFactory for PlanLifecycleChildFactory {
    fn build_child(
        &self,
        input: ChildRuntimeInput,
    ) -> Result<Runtime, merry_runtime::RuntimeError> {
        let control = input
            .plan_subagent_control
            .expect("plan subagent carries attempt control");
        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(PlanLifecycleChildProvider {
                    report_terminal_attempt: self.report_terminal_attempt,
                    attempts_started: Arc::clone(&self.attempts_started),
                    turns: AtomicUsize::new(0),
                }),
                model_name(),
            )
            .automatic_compaction(AutomaticCompactionConfig::disabled())
            .plan_subagent_control(control)
            .build()
    }
}

struct PlanLifecycleChildProvider {
    report_terminal_attempt: bool,
    attempts_started: Arc<AtomicUsize>,
    turns: AtomicUsize,
}

impl ModelProvider for PlanLifecycleChildProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| {
            ProviderName::new("interactive-plan-subagent-lifecycle").expect("valid provider")
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let turn = self.turns.fetch_add(1, Ordering::AcqRel);
            if turn == 0 {
                self.attempts_started.fetch_add(1, Ordering::AcqRel);
            }
            let event = if self.report_terminal_attempt && turn == 0 {
                let input = ReportPlanAttemptInput {
                    outcome: PlanAttemptOutcome::Completed,
                    result: Some(PlanNodeResult {
                        conclusion: "delegated verification completed".to_owned(),
                        evidence_refs: Vec::new(),
                        artifact_refs: Vec::new(),
                        changed_paths: Vec::new(),
                        verification: vec!["delegated lifecycle fixture".to_owned()],
                        open_questions: Vec::new(),
                    }),
                    diagnostic: None,
                    decomposition: None,
                    acknowledged_directive_ids: Vec::new(),
                    applied_directive_ids: Vec::new(),
                };
                completed_tool_call_event(ModelToolCall::new(
                    ModelToolCallId::new("delegated-plan-attempt").expect("valid call id"),
                    ToolName::new("report_plan_attempt").expect("valid tool name"),
                    ToolArguments::try_from(
                        serde_json::to_value(input).expect("attempt report serializes"),
                    )
                    .expect("attempt report arguments are an object"),
                ))
            } else {
                completed_text_event("delegated turn complete")
            };
            let event_stream: ModelEventStream = Box::pin(stream::iter(vec![Ok(event)]));
            Ok(event_stream)
        })
    }
}

impl RecordingProvider {
    fn new() -> Self {
        Self::new_with_steps(vec![vec![Ok(completed_text_event("done"))]])
    }

    fn new_with_steps(steps: ScriptedProviderSteps) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            steps: Arc::new(Mutex::new(steps.into_iter().rev().collect())),
        }
    }

    fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    fn next_step(&self) -> Vec<Result<ModelEvent, ModelError>> {
        self.steps
            .lock()
            .expect("steps lock")
            .pop()
            .unwrap_or_else(|| vec![Ok(completed_text_event("done"))])
    }
}

impl ModelProvider for RecordingProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("interactive-test-provider").expect("valid provider"))
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.requests.lock().expect("requests lock").push(request);
            let event_stream: ModelEventStream = Box::pin(stream::iter(self.next_step()));
            Ok(event_stream)
        })
    }
}

#[derive(Clone, Default)]
struct LocalPlanProvider {
    calls: Arc<AtomicUsize>,
}

impl ModelProvider for LocalPlanProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| ProviderName::new("interactive-local-plan").expect("valid provider"))
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            let event = if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
                let input = ReportPlanAttemptInput {
                    outcome: PlanAttemptOutcome::Completed,
                    result: Some(PlanNodeResult {
                        conclusion: "local coordinator verification completed".to_owned(),
                        evidence_refs: Vec::new(),
                        artifact_refs: Vec::new(),
                        changed_paths: Vec::new(),
                        verification: vec!["deterministic local verification".to_owned()],
                        open_questions: Vec::new(),
                    }),
                    diagnostic: None,
                    decomposition: None,
                    acknowledged_directive_ids: Vec::new(),
                    applied_directive_ids: Vec::new(),
                };
                completed_tool_call_event(ModelToolCall::new(
                    ModelToolCallId::new("local-plan-attempt").expect("valid call id"),
                    ToolName::new("report_plan_attempt").expect("valid tool name"),
                    ToolArguments::try_from(
                        serde_json::to_value(input).expect("attempt report serializes"),
                    )
                    .expect("attempt report arguments are an object"),
                ))
            } else {
                completed_text_event("local plan complete")
            };
            let event_stream: ModelEventStream = Box::pin(stream::iter(vec![Ok(event)]));
            Ok(event_stream)
        })
    }
}

#[derive(Clone, Default)]
struct MissingLocalPlanReportProvider {
    calls: Arc<AtomicUsize>,
}

impl ModelProvider for MissingLocalPlanReportProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| {
            ProviderName::new("interactive-missing-local-plan-report").expect("valid provider")
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        _request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let event_stream: ModelEventStream = Box::pin(stream::iter(vec![Ok(
                completed_text_event("work finished without a structured attempt report"),
            )]));
            Ok(event_stream)
        })
    }
}

#[derive(Clone)]
struct BlockingFirstProvider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    first_started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    first_release: Arc<AsyncMutex<Option<oneshot::Receiver<()>>>>,
}

impl BlockingFirstProvider {
    fn new(started: oneshot::Sender<()>, release: oneshot::Receiver<()>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            first_started: Arc::new(Mutex::new(Some(started))),
            first_release: Arc::new(AsyncMutex::new(Some(release))),
        }
    }

    fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl ModelProvider for BlockingFirstProvider {
    fn name(&self) -> &ProviderName {
        static NAME: OnceLock<ProviderName> = OnceLock::new();
        NAME.get_or_init(|| {
            ProviderName::new("interactive-blocking-provider").expect("valid provider")
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        static CAPABILITIES: OnceLock<ModelCapabilities> = OnceLock::new();
        CAPABILITIES.get_or_init(|| {
            ModelCapabilities::new(true, true, false, true, None, None).expect("valid capabilities")
        })
    }

    fn stream_model<'a>(
        &'a self,
        request: ModelRequest,
        _context: ModelStreamContext,
    ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
        Box::pin(async move {
            self.requests.lock().expect("requests lock").push(request);

            let blocks_first = self
                .first_started
                .lock()
                .expect("started lock")
                .take()
                .map(|sender| {
                    let _ = sender.send(());
                })
                .is_some();

            if blocks_first {
                let release = self
                    .first_release
                    .lock()
                    .await
                    .take()
                    .expect("first release receiver exists");
                release.await.expect("test releases first provider step");
                let event_stream: ModelEventStream =
                    Box::pin(stream::iter([Ok(completed_text_event("first done"))]));
                return Ok(event_stream);
            }

            let event_stream: ModelEventStream =
                Box::pin(stream::iter([Ok(completed_text_event("done"))]));
            Ok(event_stream)
        })
    }
}

#[derive(Clone)]
struct BlockingToolExecutor {
    started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl ToolExecutor for BlockingToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if let Some(started) = self.started.lock().expect("started lock").take() {
                let _ = started.send(());
            }
            context.cancellation_token().cancelled().await;
            Err(ToolExecutionError::Cancelled)
        })
    }
}

#[derive(Clone)]
struct InfrastructureFailingToolExecutor;

impl ToolExecutor for InfrastructureFailingToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async {
            Err(ToolExecutionError::infrastructure(
                "test executor backend is unavailable\nretry later",
            ))
        })
    }
}

#[derive(Clone)]
struct CancelFirstThenSucceedToolExecutor {
    first_started: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    calls_started: Arc<Mutex<usize>>,
}

impl ToolExecutor for CancelFirstThenSucceedToolExecutor {
    fn execute<'a>(
        &'a self,
        _call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        let first_started = Arc::clone(&self.first_started);
        let calls_started = Arc::clone(&self.calls_started);
        Box::pin(async move {
            let call_index = {
                let mut calls_started = calls_started.lock().expect("calls lock");
                let call_index = *calls_started;
                *calls_started += 1;
                call_index
            };

            if call_index == 0 {
                if let Some(started) = first_started.lock().expect("started lock").take() {
                    let _ = started.send(());
                }
                context.cancellation_token().cancelled().await;
                return Err(ToolExecutionError::Cancelled);
            }

            Ok(ToolExecutionOutcome::succeeded_text("second tool ok"))
        })
    }
}

#[derive(Clone)]
struct BarrierToolExecutor {
    barrier: Arc<Barrier>,
}

impl BarrierToolExecutor {
    fn new(parties: usize) -> Self {
        Self {
            barrier: Arc::new(Barrier::new(parties)),
        }
    }
}

impl ToolExecutor for BarrierToolExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            self.barrier.wait().await;
            Ok(ToolExecutionOutcome::succeeded_text(format!(
                "result for {}",
                call.id()
            )))
        })
    }
}

#[tokio::test]
async fn interactive_run_starts_waiting_for_input() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-waiting"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, _control) = run.split();

    let event = stream.next().await.expect("state event");
    assert!(matches!(
        event,
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));
    assert!(provider.recorded_requests().is_empty());
}

#[tokio::test]
async fn interactive_control_saves_at_a_waiting_boundary_without_closing_the_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = FileSessionStore::new(temp.path());
    let id = session_id("interactive-save-waiting");
    let runtime = Runtime::builder(id.clone())
        .model_provider(Arc::new(RecordingProvider::new()), model_name())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    assert!(matches!(
        stream.next().await.expect("waiting state"),
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));
    input
        .submit_next("persist this turn")
        .await
        .expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    control
        .save_session_to(store.clone())
        .await
        .expect("waiting save succeeds");
    let resumed = Runtime::builder(id)
        .resume_from_store_without_automatic_savepoints(store)
        .await
        .expect("saved session resumes");
    assert_eq!(
        resumed
            .session_transcript()
            .await
            .expect("transcript reads"),
        vec![
            SessionTranscriptItem::UserMessage {
                text: "persist this turn".to_owned(),
                images: Vec::new(),
            },
            SessionTranscriptItem::AssistantText {
                text: "done".to_owned(),
            },
        ]
    );

    control.close().await.expect("interactive run closes");
    stream.wait_until_closed().await;
}

#[tokio::test]
async fn interactive_control_returns_waiting_save_failures_through_the_ack() {
    let temp = tempfile::tempdir().expect("tempdir");
    let blocked_root = temp.path().join("not-a-directory");
    std::fs::write(&blocked_root, "blocked").expect("blocking file");
    let runtime = Runtime::builder(session_id("interactive-save-error"))
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    let error = control
        .save_session_to(FileSessionStore::new(blocked_root))
        .await
        .expect_err("write failure reaches the caller");
    assert!(matches!(error, InteractiveError::Runtime { .. }));

    control.close().await.expect("interactive run closes");
    stream.wait_until_closed().await;
}

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
    let _ = stream.next().await.expect("waiting state");

    control
        .enter_plan_mode("user requested explicit planning")
        .await
        .expect("plan mode control succeeds");
    let plan_event = timeout(Duration::from_secs(1), async {
        loop {
            let event = stream.next().await.expect("interactive event");
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
    let _ = stream.next().await.expect("waiting state");
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
#[ignore = "legacy local Plan attempt scheduler was removed; use explicit subagent PlanLink lifecycle"]
async fn interactive_run_automatically_executes_a_local_plan_node_without_a_lease() {
    let provider = LocalPlanProvider::default();
    let runtime = Runtime::builder(session_id("interactive-local-plan-lane"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "define local coordinator work".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(local_plan_input())
        .await
        .expect("plan definition succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan is authorized");
    timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .expect("plan exists");
            if snapshot
                .attempts
                .iter()
                .any(|attempt| attempt.outcome.is_none() && attempt.lease_id.is_none())
            {
                assert!(snapshot.leases.is_empty());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("local attempt should be reserved without a lease");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut events, _input, _control) = run.split();
    let _ = events.next().await.expect("waiting state");
    timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .is_some_and(|snapshot| snapshot.phase == PlanPhase::Completed)
            {
                break;
            }
            let _ = events.next().await;
        }
    })
    .await
    .expect("local plan should complete automatically");
    assert!(provider.calls.load(Ordering::Acquire) >= 2);
    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert_eq!(snapshot.phase, PlanPhase::Completed);
    assert_eq!(snapshot.nodes[0].status, PlanNodeStatus::Completed);
}

#[tokio::test]
#[ignore = "legacy Plan attempt replay protocol was removed"]
async fn interactive_run_recovers_a_missing_local_attempt_report_without_hanging() {
    let provider = MissingLocalPlanReportProvider::default();
    let runtime = Runtime::builder(session_id("interactive-missing-local-plan-report"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "define local work with protocol recovery".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(local_plan_input())
        .await
        .expect("plan definition succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan is authorized");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut events, _input, _control) = run.split();
    timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .is_some_and(|snapshot| snapshot.phase == PlanPhase::Blocked)
            {
                break;
            }
            let _ = events.next().await;
        }
    })
    .await
    .expect("missing reports should exhaust bounded retries without hanging");

    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert_eq!(snapshot.attempts.len(), 2, "{snapshot:#?}");
    assert!(snapshot.leases.is_empty());
    assert!(
        snapshot.attempts.iter().all(|attempt| {
            attempt.outcome == Some(PlanAttemptOutcome::TransientFailure)
                && attempt
                    .diagnostic
                    .as_ref()
                    .is_some_and(|diagnostic| diagnostic.code() == "missing_attempt_report")
        }),
        "{snapshot:#?}"
    );
    assert_eq!(
        provider.calls.load(Ordering::Acquire),
        3,
        "two bounded attempts plus one coordinator explanation turn"
    );
}

#[tokio::test]
#[ignore = "legacy automatic delegated Plan scheduler was removed"]
async fn delegated_plan_completion_wakes_the_idle_coordinator() {
    let coordinator = RecordingProvider::new();
    let attempts_started = Arc::new(AtomicUsize::new(0));
    let child_factory: Arc<dyn ChildRuntimeFactory> = Arc::new(PlanLifecycleChildFactory {
        report_terminal_attempt: true,
        attempts_started: Arc::clone(&attempts_started),
    });
    let runtime = Runtime::builder(session_id("interactive-delegated-plan-completion"))
        .model_provider(Arc::new(coordinator.clone()), model_name())
        .coordinator_plan_tools()
        .plan_subagent_factory(child_factory, 1)
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "verify delegated completion wakeup".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(delegated_plan_input())
        .await
        .expect("plan definition succeeds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut events, _input, _control) = run.split();
    let _ = events.next().await.expect("waiting state");

    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan is authorized");
    timeout(Duration::from_secs(3), async {
        loop {
            let completed = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .is_some_and(|snapshot| snapshot.phase == PlanPhase::Completed);
            if completed && !coordinator.recorded_requests().is_empty() {
                break;
            }
            let _ = events.next().await;
        }
    })
    .await
    .expect("terminal delegated plan should wake the coordinator");

    assert_eq!(attempts_started.load(Ordering::Acquire), 1);
    assert_eq!(coordinator.recorded_requests().len(), 1);
}

#[tokio::test]
#[ignore = "legacy automatic delegated Plan scheduler was removed"]
async fn exhausted_delegated_attempts_wake_the_idle_coordinator() {
    let coordinator = RecordingProvider::new();
    let attempts_started = Arc::new(AtomicUsize::new(0));
    let child_factory: Arc<dyn ChildRuntimeFactory> = Arc::new(PlanLifecycleChildFactory {
        report_terminal_attempt: false,
        attempts_started: Arc::clone(&attempts_started),
    });
    let runtime = Runtime::builder(session_id("interactive-delegated-plan-blocked"))
        .model_provider(Arc::new(coordinator.clone()), model_name())
        .coordinator_plan_tools()
        .plan_subagent_factory(child_factory, 1)
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "verify delegated retry exhaustion wakeup".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(delegated_plan_input())
        .await
        .expect("plan definition succeeds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut events, _input, _control) = run.split();
    let _ = events.next().await.expect("waiting state");

    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan is authorized");
    timeout(Duration::from_secs(3), async {
        loop {
            let blocked = runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .is_some_and(|snapshot| snapshot.phase == PlanPhase::Blocked);
            if blocked && !coordinator.recorded_requests().is_empty() {
                break;
            }
            let _ = events.next().await;
        }
    })
    .await
    .expect("blocked delegated plan should wake the coordinator");

    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert_eq!(attempts_started.load(Ordering::Acquire), 2);
    assert_eq!(coordinator.recorded_requests().len(), 1);
    assert!(snapshot.attempts.iter().all(|attempt| {
        attempt.outcome == Some(PlanAttemptOutcome::TransientFailure)
            && attempt
                .diagnostic
                .as_ref()
                .is_some_and(|diagnostic| diagnostic.code() == "missing_attempt_report")
    }));
}

#[tokio::test]
#[ignore = "legacy local Plan attempt reporting protocol was removed"]
async fn interactive_run_closes_failed_local_attempts_without_hanging() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Err(ModelError::invalid_request(
            "first local attempt failed",
        ))],
        vec![Err(ModelError::invalid_request(
            "second local attempt failed",
        ))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-failed-local-plan-attempt"))
        .model_provider(Arc::new(provider), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "exercise failed local turns".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(local_plan_input())
        .await
        .expect("plan definition succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan is authorized");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut events, _input, _control) = run.split();
    timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .is_some_and(|snapshot| snapshot.phase == PlanPhase::Blocked)
            {
                break;
            }
            let _ = events.next().await;
        }
    })
    .await
    .expect("failed local turns should exhaust bounded retries");

    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert_eq!(snapshot.attempts.len(), 2);
    assert!(snapshot.attempts.iter().all(|attempt| {
        attempt.outcome == Some(PlanAttemptOutcome::TransientFailure)
            && attempt.diagnostic.is_some()
    }));
}

#[tokio::test]
#[ignore = "legacy local Plan attempt scheduler was removed"]
async fn interactive_run_retries_a_local_stream_eof_without_hanging() {
    let provider = RecordingProvider::new_with_steps(vec![Vec::new(), Vec::new()]);
    let runtime = Runtime::builder(session_id("interactive-blocked-local-plan-attempt"))
        .model_provider(Arc::new(provider), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "exercise blocked local turns".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(local_plan_input())
        .await
        .expect("plan definition succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan is authorized");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut events, _input, _control) = run.split();
    timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .is_some_and(|snapshot| snapshot.phase == PlanPhase::Blocked)
            {
                break;
            }
            let _ = events.next().await;
        }
    })
    .await
    .expect("local stream EOF should become bounded transient retries");

    let snapshot = runtime
        .plan_snapshot()
        .await
        .expect("snapshot read succeeds")
        .expect("plan exists");
    assert!(
        snapshot.attempts.iter().all(|attempt| {
            attempt.outcome == Some(PlanAttemptOutcome::TransientFailure)
                && attempt
                    .diagnostic
                    .as_ref()
                    .is_some_and(|diagnostic| diagnostic.code() == "model_stream_eof")
        }),
        "{snapshot:#?}"
    );
}

#[tokio::test]
#[ignore = "legacy local Plan attempt reporting protocol was removed"]
async fn interactive_run_closes_cancelled_local_attempts_without_hanging() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Err(ModelError::Cancelled)],
        vec![Err(ModelError::Cancelled)],
    ]);
    let runtime = Runtime::builder(session_id("interactive-cancelled-local-plan-attempt"))
        .model_provider(Arc::new(provider), model_name())
        .coordinator_plan_tools()
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(BeginPlanInput {
            reason: "exercise cancelled local turns".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(local_plan_input())
        .await
        .expect("plan definition succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan is authorized");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut events, _input, _control) = run.split();
    timeout(Duration::from_secs(2), async {
        loop {
            if runtime
                .plan_snapshot()
                .await
                .expect("snapshot read succeeds")
                .is_some_and(|snapshot| snapshot.phase == PlanPhase::Blocked)
            {
                break;
            }
            let _ = events.next().await;
        }
    })
    .await
    .expect("cancelled local turns should settle instead of stranding attempts");
}

#[tokio::test]
#[ignore = "legacy Plan attempt wakeup protocol was removed"]
async fn plan_wakeup_received_during_a_user_turn_runs_at_the_next_safe_boundary() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-plan-wakeup-safe-boundary"))
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
    let (mut events, input, _control) = run.split();
    let _ = events.next().await.expect("waiting state");
    input
        .submit_next("Start an ordinary user turn")
        .await
        .expect("input queues");
    started_rx.await.expect("provider request starts");

    runtime
        .begin_plan(BeginPlanInput {
            reason: "activate plan while the user turn is in flight".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan begins");
    runtime
        .update_plan(local_plan_input())
        .await
        .expect("plan definition succeeds");
    runtime
        .authorize_plan_execution(Default::default(), vec!["test authorization".to_owned()])
        .await
        .expect("plan is authorized");
    release_tx.send(()).expect("release first provider turn");

    timeout(Duration::from_secs(2), async {
        loop {
            if provider.recorded_requests().len() >= 2 {
                break;
            }
            let _ = events.next().await;
        }
    })
    .await
    .expect("the consumed plan wakeup must trigger a continuation at the safe boundary");
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
    let _ = stream.next().await.expect("waiting state");

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
    let _ = stream.next().await.expect("waiting state");

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
#[ignore = "legacy local Plan attempt reporting protocol was removed"]
async fn structured_plan_approval_resumes_execution_without_chat_confirmation() {
    let mut review_plan = local_plan_input();
    review_plan.execution_intent = PlanExecutionIntent::RequestUserReview;
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(ModelToolCall::new(
            ModelToolCallId::new("call-plan-approval-resume").expect("valid call id"),
            ToolName::new("update_plan").expect("valid tool name"),
            ToolArguments::try_from(
                serde_json::to_value(review_plan).expect("review plan serializes"),
            )
            .expect("review plan arguments are an object"),
        )))],
        vec![Ok(completed_tool_call_event(ModelToolCall::new(
            ModelToolCallId::new("call-approved-local-attempt").expect("valid call id"),
            ToolName::new("report_plan_attempt").expect("valid tool name"),
            ToolArguments::try_from(
                serde_json::to_value(ReportPlanAttemptInput {
                    outcome: PlanAttemptOutcome::Completed,
                    result: Some(PlanNodeResult {
                        conclusion: "Structured approval resumed the planned execution.".to_owned(),
                        evidence_refs: Vec::new(),
                        artifact_refs: Vec::new(),
                        changed_paths: Vec::new(),
                        verification: vec!["approved local attempt completed".to_owned()],
                        open_questions: Vec::new(),
                    }),
                    diagnostic: None,
                    decomposition: None,
                    acknowledged_directive_ids: Vec::new(),
                    applied_directive_ids: Vec::new(),
                })
                .expect("attempt report serializes"),
            )
            .expect("attempt report arguments are an object"),
        )))],
        vec![Ok(completed_text_event("The approved plan is complete."))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-plan-structured-approval-resume"))
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
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");
    input
        .submit_next("Create the plan, then wait for structured approval")
        .await
        .expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;
    assert_eq!(provider.recorded_requests().len(), 1);

    control
        .approve_plan(PlanApprovalInput {
            plan_id: runtime
                .plan_snapshot()
                .await
                .expect("plan snapshot read succeeds")
                .expect("plan exists")
                .plan_id,
            expected_plan_revision: 1,
            review_resolution_ref: "user approved through the Plan UI".to_owned(),
            capability_envelope: Some(PlanCapabilityEnvelopeSnapshot::default()),
            authorization_refs: vec!["interactive Plan approval".to_owned()],
            requirement_resolution_refs: BTreeMap::new(),
        })
        .await
        .expect("structured plan approval succeeds");
    wait_for_interactive_waiting(&mut stream).await;

    assert_eq!(provider.recorded_requests().len(), 3);
    assert_eq!(
        runtime
            .plan_snapshot()
            .await
            .expect("plan snapshot read succeeds")
            .expect("plan exists")
            .phase,
        PlanPhase::Completed
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
    let _ = stream.next().await.expect("waiting state");
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
    let _ = stream.next().await.expect("waiting state");

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

fn delegated_plan_input() -> UpdatePlanInput {
    UpdatePlanInput {
        reason: "define delegated plan leaf".to_owned(),
        execution_intent: PlanExecutionIntent::ContinuePlanning,
        coordinator_node_id: None,
        max_concurrency_hint: Some(1),
        change: PlanChangeInput::DefinePlan {
            expected_plan_revision: 0,
            root: PlanNodeInput {
                id: None,
                client_key: Some("root".to_owned()),
                objective: "Complete delegated verification".to_owned(),
                acceptance: vec!["delegated result is durable".to_owned()],
                status: None,
                executor_policy: PlanExecutorPolicy::Delegate,
                harness: PlanHarnessSnapshot::default(),
                recovery_policy: PlanRecoveryPolicySnapshot::default(),
                depends_on: Vec::new(),
                children: Vec::new(),
            },
        },
    }
}

#[tokio::test]
async fn submit_next_while_waiting_starts_model_turn() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-submit-next"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    assert!(matches!(
        stream.next().await.expect("state event"),
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));

    let item = input.submit_next("hello").await.expect("input queued");
    assert_eq!(item.lane(), QueuedInputLane::Next);
    assert_eq!(item.text(), "hello");

    let mut saw_accepted = false;
    while let Some(event) = stream.next().await {
        if matches!(event, RuntimeEvent::QueuedInputAccepted { .. }) {
            saw_accepted = true;
            break;
        }
    }
    assert!(saw_accepted);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn interactive_settings_update_changes_the_next_request_generation_config() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_text_event("first"))],
        vec![Ok(completed_text_event("second"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-update-generation"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");
    let initial_generation = GenerationConfig::default().with_reasoning_effort(Some(
        ReasoningEffort::new("low").expect("valid reasoning effort"),
    ));
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()).with_generation_config(initial_generation),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    wait_for_interactive_waiting(&mut stream).await;
    let updated_generation = GenerationConfig::default().with_reasoning_effort(Some(
        ReasoningEffort::new("high").expect("valid reasoning effort"),
    ));
    control
        .update_settings(
            InteractiveSettingsUpdate::default().with_generation_config(updated_generation),
        )
        .await
        .expect("settings update accepted");
    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]
            .generation()
            .reasoning_effort()
            .map(ReasoningEffort::as_str),
        Some("low")
    );
    assert_eq!(
        requests[1]
            .generation()
            .reasoning_effort()
            .map(ReasoningEffort::as_str),
        Some("high")
    );
}

#[tokio::test]
async fn interactive_settings_update_changes_the_next_request_primary_model() {
    let first_provider =
        RecordingProvider::new_with_steps(vec![vec![Ok(completed_text_event("first"))]]);
    let second_provider =
        RecordingProvider::new_with_steps(vec![vec![Ok(completed_text_event("second"))]]);
    let first_model = ModelName::new("fake/first").expect("valid first model");
    let second_model = ModelName::new("fake/second").expect("valid second model");
    let runtime = Runtime::builder(session_id("interactive-update-primary"))
        .model_provider(Arc::new(first_provider.clone()), first_model.clone())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    wait_for_interactive_waiting(&mut stream).await;
    control
        .update_settings(InteractiveSettingsUpdate::default().with_primary_model(
            InteractivePrimaryModel::new(
                Arc::new(second_provider.clone()),
                second_model.clone(),
                ModelRetryPolicy::disabled(),
            ),
        ))
        .await
        .expect("settings update accepted");
    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let first_requests = first_provider.recorded_requests();
    let second_requests = second_provider.recorded_requests();
    assert_eq!(first_requests.len(), 1);
    assert_eq!(first_requests[0].model(), &first_model);
    assert_eq!(second_requests.len(), 1);
    assert_eq!(second_requests[0].model(), &second_model);
}

#[tokio::test]
async fn interactive_settings_update_changes_automatic_compaction_at_request_boundary() {
    let provider = RecordingProvider::new_with_steps(Vec::new());
    let runtime = Runtime::builder(session_id("interactive-update-compaction"))
        .model_provider(Arc::new(provider), model_name())
        .automatic_compaction(AutomaticCompactionConfig::disabled())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, _input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");
    let policy =
        CitationCompactionPolicy::new(Some(128), Some(6144), 1).expect("valid compact policy");
    let updated = AutomaticCompactionConfig::enabled(policy);

    control
        .update_settings(InteractiveSettingsUpdate::default().with_automatic_compaction(updated))
        .await
        .expect("settings update accepted");

    assert_eq!(runtime.automatic_compaction_config().await, updated);
}

#[tokio::test]
async fn interactive_settings_update_changes_context_window_at_request_boundary() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_text_event_with_usage("first"))],
        vec![Ok(completed_text_event_with_usage("second"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-update-context-window"))
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
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    wait_for_interactive_waiting(&mut stream).await;
    let fallback = runtime.usage().await.expect("fallback usage");
    let fallback_context = fallback.context.expect("fallback context");
    assert_eq!(fallback_context.resolved_model_window_tokens, 272_000);
    assert_eq!(
        fallback_context.source,
        merry_core::ContextWindowSource::Fallback
    );

    control
        .update_settings(
            InteractiveSettingsUpdate::default()
                .with_context_window_tokens(NonZeroU64::new(128_000)),
        )
        .await
        .expect("settings update accepted");
    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let configured = runtime.usage().await.expect("configured usage");
    let configured_context = configured.context.expect("configured context");
    assert_eq!(configured_context.resolved_model_window_tokens, 128_000);
    assert_eq!(
        configured_context.source,
        merry_core::ContextWindowSource::ExplicitConfig
    );
}

#[tokio::test]
async fn interactive_settings_update_does_not_interrupt_the_active_model_request() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let first_provider = BlockingFirstProvider::new(started_tx, release_rx);
    let second_provider =
        RecordingProvider::new_with_steps(vec![vec![Ok(completed_text_event("second"))]]);
    let first_model = ModelName::new("fake/first").expect("valid first model");
    let second_model = ModelName::new("fake/second").expect("valid second model");
    let runtime = Runtime::builder(session_id("interactive-update-running-primary"))
        .model_provider(Arc::new(first_provider.clone()), first_model.clone())
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("first request starts")
        .expect("start signal sent");
    timeout(
        Duration::from_secs(1),
        control.update_settings(InteractiveSettingsUpdate::default().with_primary_model(
            InteractivePrimaryModel::new(
                Arc::new(second_provider.clone()),
                second_model.clone(),
                ModelRetryPolicy::disabled(),
            ),
        )),
    )
    .await
    .expect("settings update is accepted while the request is active")
    .expect("settings update succeeds");

    assert_eq!(first_provider.recorded_requests().len(), 1);
    assert!(second_provider.recorded_requests().is_empty());
    release_tx.send(()).expect("release first request");
    wait_for_interactive_waiting(&mut stream).await;

    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let first_requests = first_provider.recorded_requests();
    let second_requests = second_provider.recorded_requests();
    assert_eq!(first_requests.len(), 1);
    assert_eq!(first_requests[0].model(), &first_model);
    assert_eq!(second_requests.len(), 1);
    assert_eq!(second_requests[0].model(), &second_model);
}

#[tokio::test]
async fn interactive_subagent_setting_changes_the_next_request_tool_profile() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_text_event("disabled"))],
        vec![Ok(completed_text_event("enabled"))],
    ]);
    let manager = SubagentManager::runtime_controlled(
        session_id("interactive-update-subagents"),
        SubagentConfig::default(),
        Arc::new(NoopChildFactory),
        false,
    );
    let [spawn, wait, cancel] =
        subagent_registered_tools(manager.clone()).expect("subagent tools build");
    let runtime = Runtime::builder(session_id("interactive-update-subagents"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .subagent_manager(manager)
        .register_tool(spawn)
        .register_tool(wait)
        .register_tool(cancel)
        .build()
        .expect("runtime builds");
    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    wait_for_interactive_waiting(&mut stream).await;
    control
        .update_settings(InteractiveSettingsUpdate::default().with_subagents(
            InteractiveSubagentSettings::new(true, SubagentConfig::default()),
        ))
        .await
        .expect("settings update accepted");
    input.submit_next("second").await.expect("second queued");
    wait_for_interactive_waiting(&mut stream).await;

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        !requests[0]
            .tools()
            .iter()
            .any(|tool| tool.name().as_str() == "spawn_subagents")
    );
    assert!(
        requests[1]
            .tools()
            .iter()
            .any(|tool| tool.name().as_str() == "spawn_subagents")
    );
}

async fn wait_for_interactive_waiting(stream: &mut merry_runtime::InteractiveRunEventStream) {
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            return;
        }
    }
    panic!("interactive run closed before returning to waiting");
}

#[tokio::test(flavor = "current_thread")]
async fn interactive_run_executes_parallel_safe_tool_batch_before_waiting() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-1", "search_notes"),
            model_tool_call("call-2", "search_notes"),
        ]))],
        vec![Ok(completed_text_event("done"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-parallel-tool-batch"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(
            merry_runtime::RegisteredTool::read_only(
                tool_spec("search_notes"),
                Arc::new(BarrierToolExecutor::new(2)),
            )
            .with_parallel_safe_execution(),
        )
        .max_parallel_tool_calls(NonZeroUsize::new(2).expect("non-zero limit"))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    assert!(matches!(
        stream.next().await.expect("initial waiting state"),
        RuntimeEvent::InteractiveRunStateChanged {
            state: InteractiveRunState::WaitingForInput
        }
    ));

    input
        .submit_next("search twice")
        .await
        .expect("input queued");
    let observed = timeout(Duration::from_secs(1), async {
        let mut finished = 0;
        let mut saw_answer = false;
        while let Some(event) = stream.next().await {
            match event {
                RuntimeEvent::ToolCallFinished { .. } => finished += 1,
                RuntimeEvent::AssistantMessage { .. } => saw_answer = true,
                RuntimeEvent::InteractiveRunStateChanged {
                    state: InteractiveRunState::WaitingForInput,
                } if saw_answer => return finished,
                _ => {}
            }
        }
        finished
    })
    .await
    .expect("interactive batch should complete without serial barrier deadlock");

    assert_eq!(observed, 2);
    assert!(runtime.pending_tool_calls().await.is_empty());
    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].batch_continuations()[0]
            .results()
            .iter()
            .map(|result| result.call_id().as_str())
            .collect::<Vec<_>>(),
        ["call-1", "call-2"]
    );
}

#[tokio::test]
async fn enqueue_while_waiting_starts_backlog_turn() {
    let provider = RecordingProvider::new();
    let runtime = Runtime::builder(session_id("interactive-backlog-waiting"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    let item = input.enqueue("later").await.expect("backlog queued");
    assert_eq!(item.lane(), QueuedInputLane::Backlog);
    assert_eq!(item.text(), "later");

    let mut saw_accepted = false;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            RuntimeEvent::QueuedInputAccepted {
                lane: QueuedInputLane::Backlog,
                ..
            }
        ) {
            saw_accepted = true;
            break;
        }
    }
    assert!(saw_accepted);
    assert_eq!(provider.recorded_requests().len(), 1);
}

#[tokio::test]
async fn next_burst_before_boundary_becomes_two_user_messages_in_one_request() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-next-burst"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let first = input.submit_next("first");
    let second = input.submit_next("second");
    let (first, second) = timeout(Duration::from_millis(200), async move {
        tokio::join!(first, second)
    })
    .await
    .expect("running step should keep accepting queued next input");
    let first = first.expect("first queued");
    let second = second.expect("second queued");

    release_tx.send(()).expect("first provider step released");

    let mut saw_two = false;
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Next,
        } = event
            && inputs
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>()
                == vec![first.text(), second.text()]
        {
            saw_two = true;
            break;
        }
    }
    assert!(saw_two);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let user_texts = requests[1]
        .messages()
        .iter()
        .filter(|message| message.role() == ModelMessageRole::User)
        .map(|message| message.content().as_text().to_owned())
        .collect::<Vec<_>>();
    assert!(user_texts.ends_with(&["first".to_owned(), "second".to_owned()]));
}

#[tokio::test]
async fn close_during_running_model_reaches_closed_event() {
    let (started_tx, started_rx) = oneshot::channel();
    let (_release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-close-running"))
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
    let _ = stream.next().await.expect("waiting state");
    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("provider step starts");

    control.close().await.expect("close accepted");

    timeout(Duration::from_secs(1), async {
        while let Some(event) = stream.next().await {
            if matches!(event, RuntimeEvent::Closed) {
                return;
            }
        }
        panic!("stream ended before closed event");
    })
    .await
    .expect("close while running should reach closed event");
}

#[tokio::test]
async fn next_burst_does_not_reorder_backlog() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-backlog-order"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    input.enqueue("backlog").await.expect("backlog queued");
    input.submit_next("next").await.expect("next queued");

    release_tx.send(()).expect("first provider step released");

    let mut accepted = Vec::new();
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted { inputs, .. } = event {
            accepted.extend(
                inputs
                    .into_iter()
                    .map(|item| item.text)
                    .filter(|text| text == "next" || text == "backlog"),
            );
            if accepted.contains(&"next".to_owned()) {
                break;
            }
        }
    }
    assert_eq!(accepted, vec!["next".to_owned()]);

    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Backlog,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["backlog"]
            );
            break;
        }
    }
}

#[tokio::test]
async fn input_handle_updates_removes_and_reorders_pending_items() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-edit-queue"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("first provider step starts");

    let mut first = input.enqueue("first").await.expect("first queued");
    let second = input.enqueue("second").await.expect("second queued");

    first
        .update("updated")
        .await
        .expect("pending input updates");
    let mut snapshot = input.snapshot().await.expect("snapshot");
    snapshot.backlog.swap(0, 1);
    input
        .replace_pending_order(QueuedInputLane::Backlog, &snapshot.backlog)
        .await
        .expect("pending input reorders");
    second.remove().await.expect("pending input removes");

    let snapshot = input.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.backlog.len(), 1);
    assert_eq!(snapshot.backlog[0].text(), "updated");

    release_tx.send(()).expect("first provider step released");

    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Backlog,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["updated"]
            );
            break;
        }
    }
}

#[tokio::test]
async fn interrupt_moves_existing_next_to_suspended_and_post_interrupt_next_runs_alone() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-esc-suspended"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("initial provider step starts");
    input.submit_next("x").await.expect("x queued");
    input.submit_next("y").await.expect("y queued");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let snapshot = input.snapshot().await.expect("snapshot");
    assert_eq!(
        snapshot
            .suspended
            .iter()
            .map(|item| item.text())
            .collect::<Vec<_>>(),
        vec!["x", "y"]
    );

    input.submit_next("z").await.expect("z queued");
    drop(release_tx);

    let mut saw_z = false;
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Next,
        } = event
            && inputs.iter().any(|item| item.text == "z")
        {
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].text, "z");
            saw_z = true;
            break;
        }
    }
    assert!(saw_z);
}

#[tokio::test]
async fn resume_suspended_accepts_suspended_burst_when_waiting() {
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = BlockingFirstProvider::new(started_tx, release_rx);
    let runtime = Runtime::builder(session_id("interactive-resume-suspended"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("initial").await.expect("initial queued");
    started_rx.await.expect("initial provider step starts");
    input.submit_next("suspended").await.expect("queued");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");
    drop(release_tx);

    let mut waiting = false;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            waiting = true;
            break;
        }
    }
    assert!(waiting);

    control.resume_suspended().await.expect("suspended resumes");

    let mut saw_suspended = false;
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::QueuedInputAccepted {
            inputs,
            lane: QueuedInputLane::Suspended,
        } = event
        {
            assert_eq!(
                inputs
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>(),
                vec!["suspended"]
            );
            saw_suspended = true;
            break;
        }
    }
    assert!(saw_suspended);
}

#[tokio::test]
async fn interrupt_during_tool_execution_closes_pending_tool_call() {
    let (started_tx, started_rx) = oneshot::channel();
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-blocking",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("after cancel"))],
    ]);
    let tool = BlockingToolExecutor {
        started: Arc::new(Mutex::new(Some(started_tx))),
    };
    let runtime = Runtime::builder(session_id("interactive-tool-interrupt"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(tool),
        ))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("start").await.expect("start queued");
    started_rx.await.expect("tool starts");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let mut saw_resolved = false;
    while let Some(event) = stream.next().await {
        if matches!(event, RuntimeEvent::ToolCallFinished { .. }) {
            saw_resolved = true;
            break;
        }
    }
    assert!(saw_resolved);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test]
async fn new_input_after_interrupt_still_executes_runtime_tool_calls() {
    let (started_tx, started_rx) = oneshot::channel();
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-cancelled",
            "search_notes",
        )))],
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-after-interrupt",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("after second tool"))],
    ]);
    let tool = CancelFirstThenSucceedToolExecutor {
        first_started: Arc::new(Mutex::new(Some(started_tx))),
        calls_started: Arc::new(Mutex::new(0)),
    };
    let runtime = Runtime::builder(session_id("interactive-tool-after-interrupt"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(tool),
        ))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("first").await.expect("first queued");
    started_rx.await.expect("first tool starts");
    control
        .interrupt(InterruptReason::User)
        .await
        .expect("interrupt accepted");

    let mut returned_to_waiting = false;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::WaitingForInput
            }
        ) {
            returned_to_waiting = true;
            break;
        }
    }
    assert!(returned_to_waiting);
    assert!(runtime.pending_tool_calls().await.is_empty());

    input
        .submit_next("second")
        .await
        .expect("second queued after interrupt");

    let mut saw_second_tool_result = false;
    while let Some(event) = stream.next().await {
        if let RuntimeEvent::ToolCallFinished { result, .. } = event
            && result.call_id().as_str() == "call-after-interrupt"
        {
            saw_second_tool_result = true;
            break;
        }
    }
    assert!(saw_second_tool_result);
    assert!(runtime.pending_tool_calls().await.is_empty());
}

#[tokio::test]
async fn interactive_tool_infrastructure_failure_returns_to_waiting() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_event(model_tool_call(
            "call-infrastructure-failure",
            "search_notes",
        )))],
        vec![Ok(completed_text_event("continued after tool failure"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-tool-infrastructure-failure"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(InfrastructureFailingToolExecutor),
        ))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("start").await.expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(provider.recorded_requests().len(), 2);
}

#[tokio::test]
async fn interactive_tool_batch_infrastructure_failure_resolves_all_pending_calls() {
    let provider = RecordingProvider::new_with_steps(vec![
        vec![Ok(completed_tool_call_batch_event(vec![
            model_tool_call("call-batch-infrastructure-1", "search_notes"),
            model_tool_call("call-batch-infrastructure-2", "search_notes"),
        ]))],
        vec![Ok(completed_text_event("continued after batch failure"))],
    ]);
    let runtime = Runtime::builder(session_id("interactive-tool-batch-infrastructure-failure"))
        .model_provider(Arc::new(provider.clone()), model_name())
        .register_tool(merry_runtime::RegisteredTool::read_only(
            tool_spec("search_notes"),
            Arc::new(InfrastructureFailingToolExecutor),
        ))
        .build()
        .expect("runtime builds");

    let run = runtime
        .start_interactive_agent_run(
            StepContext::new(CancellationToken::new()),
            AgentLoopConfig::default(),
        )
        .expect("interactive run starts");
    let (mut stream, input, _control) = run.split();
    let _ = stream.next().await.expect("waiting state");

    input.submit_next("start").await.expect("input queued");
    wait_for_interactive_waiting(&mut stream).await;

    assert!(runtime.pending_tool_calls().await.is_empty());
    assert_eq!(provider.recorded_requests().len(), 2);
}
