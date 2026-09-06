pub(super) use super::test_support::*;
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
        atomic::{AtomicBool, AtomicUsize, Ordering},
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

struct GatedRecordingModelChildFactory {
    release: CancellationToken,
}

impl ChildRuntimeFactory for GatedRecordingModelChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        let provider = ScriptedStepProvider::with_release(
            vec![vec![Ok(ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("gated child done")],
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
                ModelName::new("fake/gated-recording-child").expect("valid model name"),
            )
            .build()
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

struct BridgeRequestChildFactory;

impl ChildRuntimeFactory for BridgeRequestChildFactory {
    fn build_child(&self, input: ChildRuntimeInput) -> Result<Runtime, crate::RuntimeError> {
        let provider = ScriptedStepProvider::new(vec![vec![Ok(ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::tool_call(ModelToolCall::new(
                    ModelToolCallId::new("call-child-bridge").expect("valid call id"),
                    ToolName::new("child_bridge").expect("valid tool name"),
                    ToolArguments::new(Map::new()),
                ))],
                FinishReason::ToolCalls,
                None,
            ),
        })]]);

        Runtime::builder(input.session_id)
            .task_anchor(input.task_anchor)
            .model_provider(
                Arc::new(provider),
                ModelName::new("fake/bridge-child").expect("valid model name"),
            )
            .tool_admission(crate::ToolAdmission::allow_only(input.allowed_tools))
            .allow_bridge_tools()
            .register_tool(RegisteredTool::bridge(bridge_tool_spec()))
            .build()
    }
}

struct DefaultScopePlanLinkRuntime;

impl PlanLinkRuntime for DefaultScopePlanLinkRuntime {
    fn bind_subagent<'a>(
        &'a self,
        _client_key: String,
        _agent_id: SubagentId,
        _task_id: SubagentTaskId,
        _now_ms: u64,
    ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
        Box::pin(async { Err("unused test binding".to_owned()) })
    }

    fn update_subagent_link<'a>(
        &'a self,
        _binding_id: PlanBindingId,
        _status: PlanLinkStatus,
        _now_ms: u64,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

fn synthetic_plan_link(
    agent_id: SubagentId,
    task_id: SubagentTaskId,
    binding_id: PlanBindingId,
) -> PlanLinkSnapshot {
    PlanLinkSnapshot {
        plan_id: merry_core::PlanId::new("synthetic-plan").expect("valid plan id"),
        node_id: merry_core::PlanNodeId::new("synthetic-node").expect("valid node id"),
        binding_id,
        subagent_id: agent_id,
        task_id,
        status: PlanLinkStatus::Active,
        linked_at_ms: 1,
        terminal_at_ms: None,
        superseded_by: None,
    }
}

struct FailingScopePlanLinkRuntime {
    updates: Arc<StdMutex<Vec<PlanLinkStatus>>>,
}

impl PlanLinkRuntime for FailingScopePlanLinkRuntime {
    fn bind_subagent<'a>(
        &'a self,
        _client_key: String,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        _now_ms: u64,
    ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
        Box::pin(async move {
            Ok(synthetic_plan_link(
                agent_id,
                task_id,
                PlanBindingId::new("synthetic-binding").expect("valid binding id"),
            ))
        })
    }

    fn update_subagent_link<'a>(
        &'a self,
        _binding_id: PlanBindingId,
        status: PlanLinkStatus,
        _now_ms: u64,
    ) -> BoxFuture<'a, Result<(), String>> {
        let updates = Arc::clone(&self.updates);
        Box::pin(async move {
            updates
                .lock()
                .expect("link updates mutex is not poisoned")
                .push(status);
            Ok(())
        })
    }

    fn scope_for_link<'a>(
        &'a self,
        _link: &'a PlanLinkSnapshot,
    ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
        Box::pin(async { Err("synthetic scope lookup failed".to_owned()) })
    }
}

#[derive(Clone)]
struct OrderingPlanLinkRuntime {
    hub: Arc<SubagentActivityHub>,
    terminal_seen_during_update: Arc<StdMutex<Vec<bool>>>,
    phases_during_update: Arc<StdMutex<Vec<Option<merry_core::SubagentActivityPhase>>>>,
    update_started: Arc<Notify>,
    release: CancellationToken,
    update_completed: Arc<AtomicBool>,
    update_completed_notify: Arc<Notify>,
}

impl PlanLinkRuntime for OrderingPlanLinkRuntime {
    fn bind_subagent<'a>(
        &'a self,
        _client_key: String,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        _now_ms: u64,
    ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
        Box::pin(async move {
            let binding_id = PlanBindingId::new("ordering-binding").expect("valid binding id");
            Ok(synthetic_plan_link(agent_id, task_id, binding_id))
        })
    }

    fn update_subagent_link<'a>(
        &'a self,
        _binding_id: PlanBindingId,
        _status: PlanLinkStatus,
        _now_ms: u64,
    ) -> BoxFuture<'a, Result<(), String>> {
        let hub = Arc::clone(&self.hub);
        let observations = Arc::clone(&self.terminal_seen_during_update);
        let phases = Arc::clone(&self.phases_during_update);
        let update_started = Arc::clone(&self.update_started);
        let release = self.release.clone();
        let update_completed = Arc::clone(&self.update_completed);
        let update_completed_notify = Arc::clone(&self.update_completed_notify);
        Box::pin(async move {
            let activity = hub.current();
            let terminal_seen = activity.iter().any(|snapshot| {
                matches!(
                    snapshot.phase,
                    merry_core::SubagentActivityPhase::Completed
                        | merry_core::SubagentActivityPhase::Failed
                        | merry_core::SubagentActivityPhase::Cancelled
                )
            });
            phases
                .lock()
                .expect("ordering phases mutex is not poisoned")
                .push(activity.first().map(|snapshot| snapshot.phase));
            observations
                .lock()
                .expect("ordering observations mutex is not poisoned")
                .push(terminal_seen);
            update_started.notify_one();
            release.cancelled().await;
            update_completed.store(true, Ordering::SeqCst);
            update_completed_notify.notify_one();
            Ok(())
        })
    }

    fn scope_for_link<'a>(
        &'a self,
        _link: &'a PlanLinkSnapshot,
    ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Clone)]
struct BlockingScopePlanLinkRuntime {
    scope_calls: Arc<AtomicUsize>,
    lookup_started: Arc<Notify>,
    lookup_dropped: Arc<Notify>,
    release: CancellationToken,
    updates: Arc<StdMutex<Vec<PlanLinkStatus>>>,
}

struct LookupDropGuard(Arc<Notify>);

impl Drop for LookupDropGuard {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

impl PlanLinkRuntime for BlockingScopePlanLinkRuntime {
    fn bind_subagent<'a>(
        &'a self,
        _client_key: String,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        _now_ms: u64,
    ) -> BoxFuture<'a, Result<PlanLinkSnapshot, String>> {
        Box::pin(async move {
            let binding_id = PlanBindingId::new(&format!("synthetic-binding-{}", task_id.as_str()))
                .expect("valid binding id");
            Ok(synthetic_plan_link(agent_id, task_id, binding_id))
        })
    }

    fn update_subagent_link<'a>(
        &'a self,
        _binding_id: PlanBindingId,
        status: PlanLinkStatus,
        _now_ms: u64,
    ) -> BoxFuture<'a, Result<(), String>> {
        let updates = Arc::clone(&self.updates);
        Box::pin(async move {
            updates
                .lock()
                .expect("link updates mutex is not poisoned")
                .push(status);
            Ok(())
        })
    }

    fn scope_for_link<'a>(
        &'a self,
        _link: &'a PlanLinkSnapshot,
    ) -> BoxFuture<'a, Result<Option<PlanSubagentScope>, String>> {
        let call = self.scope_calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Box::pin(async { Ok(None) });
        }
        let lookup_started = Arc::clone(&self.lookup_started);
        let lookup_dropped = Arc::clone(&self.lookup_dropped);
        let release = self.release.clone();
        Box::pin(async move {
            let _guard = LookupDropGuard(lookup_dropped);
            lookup_started.notify_one();
            release.cancelled().await;
            Ok(None)
        })
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

fn bridge_tool_spec() -> ToolSpec {
    let schema =
        Schema::try_from(json!({ "type": "object" })).expect("test schema should be a JSON schema");
    ToolSpec::new(
        ToolName::new("child_bridge").expect("valid tool name"),
        "Request a host bridge operation.",
        ToolInputSchema::new(schema).expect("valid schema"),
    )
    .expect("valid tool spec")
}


mod plan_lifecycle;
mod plan_runtime;
mod policy;
mod results;
mod scheduler;
