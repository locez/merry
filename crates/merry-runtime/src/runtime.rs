//! Runtime builder and step execution skeleton.
//!
//! [`Runtime`] is the MVP facade for session-owned state. Step execution and
//! direct mutation APIs admit one active operation at a time, record durable
//! session state before returning observable events where applicable, and keep
//! provider wire details behind the `merry-llm` provider boundary.

use crate::{
    AcceptedLocalWorkspaceProcessAdmission, CheckpointId, CheckpointRefExcerpt, CheckpointRefId,
    CitationCompactionInput, CitationCompactionPolicy, CompactedCheckpointSummary, CompactionError,
    CompactionOutcome, ProcessRunner, RuntimeCapabilities, RuntimeError, RuntimeEventStream,
    RuntimeModelRole,
    event_stream::ActiveStepPermit,
    judgment::{JudgmentContext, JudgmentError, JudgmentRecord, JudgmentRequest, JudgmentSource},
    memory::MemoryActivationSource,
    model_config::RuntimeModelConfigs,
    permission::{PermissionAdmissionSource, PermissionReviewMode, RuntimeTrustLevel},
    process::PermissionedProcessRunnerFactory,
    session::SessionState,
    step::{StepContext, StepInput},
    subagent::SubagentManager,
    tool::{ToolExecutionContext, ToolRegistry},
};
use merry_core::{ErrorInfo, RuntimeEvent, SessionId, ToolCallId};
use merry_llm::GenerationConfig;
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

mod builder;
mod compaction;
mod events;
mod memory_activation;
mod model_output;
mod permission_execution;
mod process_execution;
mod provider_request;
mod provider_step;
mod session_access;
mod tool_execution;

pub use self::builder::{AutomaticCompactionConfig, RuntimeBuilder};
use self::compaction::compact_context_once_inner;
use self::events::{
    send_cancelled_event, send_cancelled_if_requested, send_normal_event,
    stream_model_with_retry_policy,
};
#[cfg(test)]
use self::memory_activation::memory_activation_seed_from_step_input;
#[cfg(test)]
use self::provider_request::request_context_budget;
#[cfg(test)]
use self::tool_execution::admit_action_to_generic_executor;

const DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED: &str = "tool_call_result_required";
const DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED: &str = "action_policy_denied";
const DIAGNOSTIC_TOOL_NOT_REGISTERED: &str = "tool_not_registered";
const TOOL_ACTION_POLICY_DENIED_MESSAGE: &str = "tool action was blocked by runtime policy";
const WORKSPACE_PATCH_TOOL_NAME: &str = "workspace_patch";

/// Merry runtime handle for one session.
///
/// A cloned handle points at the same session-owned state. [`Runtime::step`]
/// and direct mutation APIs such as [`Runtime::record_artifact`],
/// [`Runtime::record_context_entry`], [`Runtime::submit_tool_result`], and
/// [`Runtime::execute_tool_call`] acquire the active-step permit.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl Runtime {
    /// Creates a runtime builder for the provided session.
    ///
    /// The session id defines the ownership boundary for artifacts, context,
    /// ledger facts, pending tool calls, and emitted runtime events.
    #[must_use]
    pub fn builder(session_id: SessionId) -> RuntimeBuilder {
        RuntimeBuilder::new(session_id)
    }

    /// Starts a runtime step and returns its event stream.
    ///
    /// Only one step or direct mutation may own the runtime at a time. The
    /// step producer owns the active-step permit. Dropping the
    /// returned [`RuntimeEventStream`] cancels and aborts the producer; the
    /// permit is released when that producer future stops and drops its state.
    ///
    /// All events emitted by the step are provider-neutral [`RuntimeEvent`]
    /// values. The runtime records session, ledger, artifact, and pending-tool
    /// state before the corresponding event becomes observable.
    ///
    /// Cancellation records a cancelled event when the producer reaches a
    /// cancellation checkpoint. Pending tool calls remain pending unless a
    /// durable result has already been recorded.
    pub fn step(
        &self,
        input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeEventStream, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        self.step_with_active_permit(input, context, active_permit)
    }

    pub(crate) fn acquire_active_step_permit(&self) -> Result<ActiveStepPermit, RuntimeError> {
        ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step)).ok_or_else(|| {
            RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            }
        })
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.inner.session_id
    }

    /// Returns a compact snapshot of managed subagent statuses when configured.
    pub async fn subagent_snapshot(&self) -> Option<Vec<crate::SubagentStatusView>> {
        match &self.inner.subagent_manager {
            Some(manager) => Some(manager.snapshot().await),
            None => None,
        }
    }

    /// Returns the low-level Merry-managed capabilities configured for this runtime.
    #[must_use]
    pub fn capabilities(&self) -> &RuntimeCapabilities {
        &self.inner.capabilities
    }

    /// Returns whether this runtime asks the model for tool-progress commentary.
    #[must_use]
    pub fn progress_commentary(&self) -> bool {
        self.inner.progress_commentary
    }

    pub(crate) fn step_with_active_permit(
        &self,
        input: StepInput,
        context: StepContext,
        active_permit: ActiveStepPermit,
    ) -> Result<RuntimeEventStream, RuntimeError> {
        let (parent_token, generation_config, final_output_contract) = context.into_parts();
        let step_token = parent_token.child_token();
        let producer_token = step_token.clone();
        let (sender, receiver) = mpsc::channel(self.inner.event_buffer_size.get());
        let inner = Arc::clone(&self.inner);
        let producer_span = tracing::debug_span!(
            "runtime.step",
            session_id = self.inner.session_id.as_str(),
            event_buffer_size = self.inner.event_buffer_size.get(),
            provider_configured = self
                .inner
                .model_configs
                .contains_role(RuntimeModelRole::Primary),
            max_output_tokens = ?generation_config.max_output_tokens(),
            allow_parallel_tool_calls = generation_config.allow_parallel_tool_calls(),
        );

        let producer_handle = tokio::spawn(
            async move {
                run_step(
                    inner,
                    sender,
                    producer_token,
                    input,
                    generation_config,
                    final_output_contract,
                    active_permit,
                )
                .await;
            }
            .instrument(producer_span),
        );

        Ok(RuntimeEventStream::new(
            ReceiverStream::new(receiver),
            step_token,
            producer_handle,
        ))
    }

    /// Executes one pending tool call through a runtime-registered executor.
    ///
    /// Runtime code owns the resulting artifact id and `ToolCallResult`.
    /// Executor infrastructure errors and cancellation leave the call pending.
    /// Tool-domain failures should be returned as failed outcomes so the
    /// runtime can still record a durable result and emit `ToolCallResolved`.
    ///
    /// This method acquires the active-step permit while the executor runs. The
    /// executor receives provider-neutral pending call data and returns
    /// provider-neutral artifact content; provider-specific tool wire formats do
    /// not enter runtime state.
    pub async fn execute_tool_call(
        &self,
        call_id: &ToolCallId,
        context: ToolExecutionContext,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        self.execute_tool_call_with_active_permit(call_id, context, &_active_permit)
            .await
    }

    pub(crate) async fn execute_tool_call_with_active_permit(
        &self,
        call_id: &ToolCallId,
        context: ToolExecutionContext,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        tool_execution::execute_tool_call_with_active_permit(&self.inner, call_id, context).await
    }

    /// Payload-free summary of the installed compacted checkpoint, if any.
    pub async fn compacted_checkpoint_summary(&self) -> Option<CompactedCheckpointSummary> {
        let session = self.inner.session.lock().await;
        session.compacted_checkpoint_summary()
    }

    /// Reads a bounded source excerpt from the installed citation-backed checkpoint.
    pub async fn read_checkpoint_ref(
        &self,
        checkpoint_id: &CheckpointId,
        ref_id: &CheckpointRefId,
    ) -> Result<CheckpointRefExcerpt, RuntimeError> {
        let session = self.inner.session.lock().await;
        session
            .read_checkpoint_ref(checkpoint_id, ref_id)
            .map_err(RuntimeError::from)
    }

    /// Builds a model-facing citation compaction input for the compressible history prefix.
    pub async fn citation_compaction_input(
        &self,
        policy: CitationCompactionPolicy,
    ) -> Result<Option<CitationCompactionInput>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;
        let session = self.inner.session.lock().await;
        session.build_citation_compaction_input(policy)
    }

    /// Installs a validated citation compaction candidate and removes the covered history prefix.
    pub async fn install_citation_compaction_candidate(
        &self,
        input: CitationCompactionInput,
        candidate_json: &str,
    ) -> Result<CompactionOutcome, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;
        let mut session = self.inner.session.lock().await;
        session.install_citation_compaction_candidate(input, candidate_json)
    }

    /// Runs one model-backed compaction pass when a compressible history prefix exists.
    pub async fn compact_context_once(
        &self,
        policy: CitationCompactionPolicy,
        context: StepContext,
    ) -> Result<Option<CompactionOutcome>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        let (token, _, _) = context.into_parts();
        if token.is_cancelled() {
            return Err(RuntimeError::Compaction {
                source: CompactionError::InvalidModelResponseShape {
                    reason: "compaction cancelled before input build",
                },
            });
        }

        compact_context_once_inner(&self.inner, policy, token).await
    }

    #[allow(dead_code)]
    pub(crate) async fn run_uncertainty_review(
        &self,
        source: &dyn JudgmentSource,
        request: JudgmentRequest,
        token: CancellationToken,
    ) -> Result<JudgmentRecord, JudgmentError> {
        if token.is_cancelled() {
            return Err(JudgmentError::Cancelled);
        }

        {
            let session = self.inner.session.lock().await;
            if token.is_cancelled() {
                return Err(JudgmentError::Cancelled);
            }
            session.preflight_judgment_request(&request)?;
        }

        if token.is_cancelled() {
            return Err(JudgmentError::Cancelled);
        }

        let context = JudgmentContext::new(token.clone());
        let outcome = tokio::select! {
            biased;
            () = token.cancelled() => {
                return Err(JudgmentError::Cancelled);
            }
            outcome = source.judge(request.clone(), context) => outcome?,
        };

        if token.is_cancelled() {
            return Err(JudgmentError::Cancelled);
        }

        let mut session = self.inner.session.lock().await;
        if token.is_cancelled() {
            return Err(JudgmentError::Cancelled);
        }
        session.record_judgment(request, outcome)
    }
}

struct RuntimeInner {
    session_id: SessionId,
    session: Mutex<SessionState>,
    active_step: Arc<AtomicBool>,
    memory_projection_epoch: AtomicU64,
    event_buffer_size: NonZeroUsize,
    model_configs: RuntimeModelConfigs,
    automatic_compaction: AutomaticCompactionConfig,
    capabilities: RuntimeCapabilities,
    progress_commentary: bool,
    tool_registry: ToolRegistry,
    memory_activation_source: Arc<dyn MemoryActivationSource>,
    allow_low_risk_workspace_patches: bool,
    low_risk_process_runner: Option<Arc<dyn ProcessRunner>>,
    read_only_shell_process_runner: Option<Arc<dyn ProcessRunner>>,
    accepted_local_workspace_process_runner: Option<AcceptedLocalWorkspaceProcessRunner>,
    runtime_trust_level: RuntimeTrustLevel,
    permission_review_mode: PermissionReviewMode,
    permission_admission_source: Option<Arc<dyn PermissionAdmissionSource>>,
    permissioned_process_runner_factory: Option<Arc<dyn PermissionedProcessRunnerFactory>>,
    subagent_manager: Option<SubagentManager>,
}

#[derive(Clone)]
struct AcceptedLocalWorkspaceProcessRunner {
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
}

async fn run_step(
    inner: Arc<RuntimeInner>,
    sender: mpsc::Sender<RuntimeEvent>,
    token: CancellationToken,
    input: StepInput,
    generation_config: GenerationConfig,
    final_output_contract: Option<crate::FinalOutputContract>,
    _active_permit: ActiveStepPermit,
) {
    tracing::debug!(category = "started", "runtime step started");

    if token.is_cancelled() {
        tracing::debug!(category = "pre_cancelled", "runtime step pre-cancelled");
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    if !send_normal_event(&inner, &sender, &token, |session| {
        session.record_session_started_if_needed()
    })
    .await
    {
        tracing::debug!(
            category = "session_start_not_sent",
            "runtime session-start event not sent"
        );
        let _ = send_cancelled_if_requested(&inner, &sender, &token).await;
        return;
    }

    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    if !send_normal_event(&inner, &sender, &token, |session| {
        Some(session.record_step_started())
    })
    .await
    {
        let _ = send_cancelled_if_requested(&inner, &sender, &token).await;
        return;
    }

    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    let Some(provider_config) = inner.model_configs.get(RuntimeModelRole::Primary) else {
        tracing::debug!(
            category = "no_provider_completion",
            "runtime step completing without provider"
        );
        if !send_normal_event(&inner, &sender, &token, |session| {
            Some(session.record_step_completed())
        })
        .await
        {
            let _ = send_cancelled_if_requested(&inner, &sender, &token).await;
        }
        return;
    };

    tracing::debug!(
        category = "provider_path_entered",
        "runtime provider path entered"
    );
    provider_step::run_provider_step(
        &inner,
        &sender,
        &token,
        input,
        generation_config,
        final_output_contract,
        provider_config,
    )
    .await;
}

fn diagnostic_from_text(code: &'static str, message: impl AsRef<str>) -> ErrorInfo {
    let message = sanitize_diagnostic_message(message.as_ref());
    ErrorInfo::new(code, &message).expect("runtime diagnostic is sanitized and uses static code")
}

fn sanitize_diagnostic_message(message: &str) -> String {
    let sanitized: String = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let trimmed = sanitized.trim();

    let source = if trimmed.is_empty() {
        "provider returned an empty error message"
    } else {
        trimmed
    };

    source.chars().take(4096).collect()
}

#[cfg(test)]
mod tests {
    mod bridge_tool_flow;
    mod event_cancellation;
    mod memory_activation_flow;
    mod model_role_flow;
    mod permission_execution;
    mod process_cancellation;
    mod process_execution;
    mod process_shell_execution;
    mod provider_step_flow;
    mod tool_execution;
    mod tool_submit_cancellation;
    mod uncertainty_review;

    use super::{
        AutomaticCompactionConfig, DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
        DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED, Runtime, RuntimeBuilder, RuntimeInner,
        TOOL_ACTION_POLICY_DENIED_MESSAGE, WORKSPACE_PATCH_TOOL_NAME,
        admit_action_to_generic_executor, memory_activation_seed_from_step_input,
        request_context_budget, send_cancelled_event,
    };
    use crate::action_audit::ActionAuditStatus;
    use crate::action_policy::{
        ActionPolicyDecision, ActionPolicyDisposition, ActionRiskTier, DefaultActionPolicy,
    };
    use crate::artifact::ArtifactContent;
    use crate::judgment::{
        JudgmentConfidence, JudgmentContext, JudgmentError, JudgmentEvidence, JudgmentFuture,
        JudgmentOutcome, JudgmentProvenance, JudgmentPurpose, JudgmentRecommendation,
        JudgmentRecord, JudgmentRiskLevel, JudgmentSource, JudgmentSourceKind,
        ModelBackedJudgmentSource,
    };
    use crate::ledger::{LedgerFactKind, LedgerProjection, LedgerScope};
    use crate::memory::{
        ActivatedMemory, MemoryActivationContext, MemoryActivationFuture, MemoryActivationReason,
        MemoryActivationScore, MemoryActivationSource, MemoryActivationSourceKind, MemoryError,
        MemoryEvidence, MemoryId, MemoryItem, MemoryItemSelection, MemoryScope,
    };
    use crate::model_config::RuntimeModelConfigs;
    use crate::process::{
        AcceptedLocalWorkspaceProcessAdmission, PermissionedProcessRunnerFactory,
        ProcessActionIntent, ProcessEnvPolicy, ProcessExecutionEvidence, ProcessExitStatus,
        ProcessPermissionProfileId, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
        ProcessRunnerFuture, ProcessRunnerOutput, stable_process_input_fingerprint,
    };
    use crate::session::SessionState;
    use crate::tool::{
        ActionExecutionEvidence, ActionProposal, ActionProposalEvidence, RegisteredTool,
        ToolActionKind, ToolActionPreflight, ToolActionProposalFuture, ToolExecutionContext,
        ToolExecutionError, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture, ToolRegistry,
        WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
    };
    use crate::{
        ArtifactError, CheckpointDecision, CitationCompactionPolicy, ContextBudgetPolicy,
        PermissionReviewMode, RuntimeError, RuntimeModelRole, RuntimeTrustLevel, StepContext,
        request_permissions_tool,
    };
    use futures_util::StreamExt;
    use merry_core::{
        ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, PendingToolCall,
        RuntimeEvent, RuntimeEventKind, SessionId, ToolCallArguments, ToolCallId,
        ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
    };
    use merry_llm::{
        FinishReason, GenerationConfig, ModelCapabilities, ModelContent, ModelError, ModelEvent,
        ModelEventStream, ModelMessage, ModelMessageRole, ModelName, ModelOutput, ModelProvider,
        ModelProviderFuture, ModelRequest, ModelResponse, ModelRetryPolicy, ModelStreamContext,
        ModelToolCall, ModelToolCallId, ProviderErrorKind, ToolArguments,
    };
    use schemars::Schema;
    use serde_json::json;
    use std::{
        future::Future,
        num::NonZeroUsize,
        sync::{
            Arc, Mutex as StdMutex, OnceLock,
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        },
    };
    use tokio::sync::{Mutex, mpsc, oneshot};
    use tokio_util::sync::CancellationToken;

    fn trace_output_buffer() -> &'static Arc<StdMutex<Vec<u8>>> {
        #[derive(Clone)]
        struct Buffer(Arc<StdMutex<Vec<u8>>>);

        impl std::io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("buffer mutex should not be poisoned")
                    .extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        static TRACE_OUTPUT: OnceLock<Arc<StdMutex<Vec<u8>>>> = OnceLock::new();
        TRACE_OUTPUT.get_or_init(|| {
            use tracing_subscriber::{fmt, prelude::*};

            let bytes = Arc::new(StdMutex::new(Vec::new()));
            let writer_bytes = Arc::clone(&bytes);
            let subscriber = tracing_subscriber::registry().with(
                fmt::layer()
                    .json()
                    .with_writer(move || Buffer(Arc::clone(&writer_bytes))),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("test tracing subscriber should install once");
            bytes
        })
    }

    async fn capture_traces_for<F, R>(trace_marker: &str, future: F) -> (R, String)
    where
        F: Future<Output = R>,
    {
        let bytes = Arc::clone(trace_output_buffer());
        let start = bytes
            .lock()
            .expect("buffer mutex should not be poisoned")
            .len();
        let result = future.await;
        let text = {
            let guard = bytes.lock().expect("buffer mutex should not be poisoned");
            String::from_utf8(guard[start..].to_vec()).expect("trace output should be UTF-8")
        };
        let text = text
            .lines()
            .filter(|line| line.contains(trace_marker))
            .collect::<Vec<_>>()
            .join("\n");
        (result, text)
    }

    fn model_configs_with_primary(provider: RecordingModelProvider) -> RuntimeModelConfigs {
        let mut configs = RuntimeModelConfigs::default();
        configs.insert(
            RuntimeModelRole::Primary,
            Arc::new(provider),
            model_name(),
            ModelRetryPolicy::default(),
        );
        configs
    }

    fn runtime_inner() -> RuntimeInner {
        let session_id = SessionId::new("runtime-send-test").expect("valid session id");
        RuntimeInner {
            session_id: session_id.clone(),
            session: Mutex::new(SessionState::new(session_id)),
            active_step: Arc::new(AtomicBool::new(false)),
            memory_projection_epoch: AtomicU64::new(0),
            event_buffer_size: NonZeroUsize::new(1).expect("non-zero buffer"),
            model_configs: RuntimeModelConfigs::default(),
            automatic_compaction: AutomaticCompactionConfig::default(),
            capabilities: crate::RuntimeCapabilities::default(),
            progress_commentary: false,
            tool_registry: ToolRegistry::default(),
            memory_activation_source: Arc::new(crate::memory::StoredMemoryActivationSource),
            allow_low_risk_workspace_patches: false,
            low_risk_process_runner: None,
            read_only_shell_process_runner: None,
            accepted_local_workspace_process_runner: None,
            runtime_trust_level: RuntimeTrustLevel::Agent,
            permission_review_mode: PermissionReviewMode::DefaultForTrust,
            permission_admission_source: None,
            permissioned_process_runner_factory: None,
            subagent_manager: None,
        }
    }

    fn artifact_id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("valid artifact id")
    }

    fn session_id(value: &str) -> SessionId {
        SessionId::new(value).expect("valid session id")
    }

    fn model_name() -> ModelName {
        ModelName::new("fake/model").expect("valid model name")
    }

    fn named_model(value: &str) -> ModelName {
        ModelName::new(value).expect("valid model name")
    }

    fn accepted_local_workspace_process_admission() -> AcceptedLocalWorkspaceProcessAdmission {
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1()
    }

    fn completed_event() -> ModelEvent {
        ModelEvent::Completed {
            response: ModelResponse::new(
                vec![ModelOutput::text("model result")],
                FinishReason::Stop,
                None,
            ),
        }
    }

    fn completed_event_with(outputs: Vec<ModelOutput>, finish_reason: FinishReason) -> ModelEvent {
        ModelEvent::Completed {
            response: ModelResponse::new(outputs, finish_reason, None),
        }
    }

    fn permission_review_completed_event(decision: &str, rationale: &str) -> ModelEvent {
        let output = format!(
            r#"{{"schema_version":"permission_review.v1","decision":"{decision}","risk":"low","user_authorization":"high","rationale":"{rationale}"}}"#
        );
        completed_event_with(vec![ModelOutput::text(&output)], FinishReason::Stop)
    }

    fn event_kind_names(events: &[RuntimeEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event.kind {
                RuntimeEventKind::SessionStarted => "SessionStarted",
                RuntimeEventKind::StepStarted => "StepStarted",
                RuntimeEventKind::ModelRetryAttemptStarted { .. } => "ModelRetryAttemptStarted",
                RuntimeEventKind::ModelRetryScheduled { .. } => "ModelRetryScheduled",
                RuntimeEventKind::ModelRetryExhausted { .. } => "ModelRetryExhausted",
                RuntimeEventKind::StepCompleted => "StepCompleted",
                RuntimeEventKind::Cancelled { .. } => "Cancelled",
                RuntimeEventKind::Failed { .. } => "Failed",
                RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeEventKind::EvidenceReferenced { .. } => "EvidenceReferenced",
                RuntimeEventKind::ToolCallPending { .. } => "ToolCallPending",
                RuntimeEventKind::BridgeToolCallRequested { .. } => "BridgeToolCallRequested",
                RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
                RuntimeEventKind::SkillUsed { .. } => "SkillUsed",
                _ => "Unknown",
            })
            .collect()
    }

    fn failed_code(events: &[RuntimeEvent]) -> Option<&str> {
        events.iter().find_map(|event| match &event.kind {
            RuntimeEventKind::Failed { diagnostic } => Some(diagnostic.code()),
            _ => None,
        })
    }

    async fn collect_step(
        runtime: &Runtime,
        text: &str,
        context: crate::StepContext,
    ) -> Vec<RuntimeEvent> {
        runtime
            .step(
                crate::StepInput::user_text(text).expect("valid step input"),
                context,
            )
            .expect("step should start")
            .collect()
            .await
    }

    fn pending_tool_call(id: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new(id).expect("valid tool call id"),
            ToolName::new("lookup").expect("valid tool name"),
            ToolCallArguments::new(Default::default()),
        )
    }

    fn model_tool_call(id: &str) -> ModelToolCall {
        ModelToolCall::new(
            ModelToolCallId::new(id).expect("valid model tool call id"),
            ToolName::new("lookup").expect("valid tool name"),
            ToolArguments::new(Default::default()),
        )
    }

    fn judgment_evidence(label: &str, id: &str, locator: EvidenceLocator) -> JudgmentEvidence {
        JudgmentEvidence::new(label, EvidenceRef::new(artifact_id(id), locator))
            .expect("valid judgment evidence")
    }

    fn judgment_constraints() -> Vec<String> {
        vec!["advisory semantic signal only".to_owned()]
    }

    fn judgment_confidence(value: f32) -> JudgmentConfidence {
        JudgmentConfidence::new(value).expect("valid judgment confidence")
    }

    fn judgment_provenance() -> JudgmentProvenance {
        JudgmentProvenance::new(JudgmentSourceKind::Test, "runtime scripted source")
            .expect("valid judgment provenance")
    }

    fn tool_risk_review_request(
        evidence: Vec<JudgmentEvidence>,
    ) -> crate::judgment::JudgmentRequest {
        crate::judgment::JudgmentRequest::new(
            JudgmentPurpose::ToolRiskReview,
            "pending lookup tool",
            "Review whether the lookup input has semantic risk.",
            evidence,
            judgment_constraints(),
            "runtime uncertainty review test",
        )
        .expect("valid tool risk request")
    }

    fn high_tool_risk_outcome(evidence: Vec<JudgmentEvidence>) -> JudgmentOutcome {
        JudgmentOutcome::new(
            JudgmentPurpose::ToolRiskReview,
            JudgmentRecommendation::ToolRiskReview {
                risk: JudgmentRiskLevel::High,
                concerns: vec!["Input references credential-like material.".to_owned()],
            },
            judgment_confidence(0.95),
            evidence,
            "Credential-like input is semantically risky.",
            "This advisory review cannot authorize or block tool execution.",
            judgment_provenance(),
        )
        .expect("valid high risk outcome")
    }

    fn unknown_tool_risk_outcome(evidence: Vec<JudgmentEvidence>) -> JudgmentOutcome {
        JudgmentOutcome::new(
            JudgmentPurpose::ToolRiskReview,
            JudgmentRecommendation::ToolRiskReview {
                risk: JudgmentRiskLevel::Unknown,
                concerns: vec!["Available semantic evidence is insufficient.".to_owned()],
            },
            judgment_confidence(0.35),
            evidence,
            "The source could not determine the risk from available input.",
            "The result is advisory and non-authoritative.",
            judgment_provenance(),
        )
        .expect("valid unknown risk outcome")
    }

    fn model_backed_judgment_source(
        provider: RecordingModelProvider,
        source_label: &str,
    ) -> ModelBackedJudgmentSource {
        let provider: Arc<dyn ModelProvider> = Arc::new(provider);
        ModelBackedJudgmentSource::new(provider, model_name(), source_label)
            .expect("model-backed judgment source is valid")
    }

    fn model_tool_risk_judgment_json(
        risk: &str,
        concern: &str,
        evidence_index: usize,
        evidence_label: &str,
        confidence: f32,
        rationale: &str,
        uncertainty: &str,
    ) -> String {
        json!({
            "schema_version": "merry.model_judgment_output.v1",
            "purpose": "tool_risk_review",
            "recommendation": {
                "kind": "tool_risk_review",
                "risk": risk,
                "concerns": [concern],
            },
            "confidence": confidence,
            "evidence": [
                {
                    "index": evidence_index,
                    "label": evidence_label,
                },
            ],
            "rationale": rationale,
            "uncertainty": uncertainty,
        })
        .to_string()
    }

    #[derive(Debug)]
    enum ScriptedJudgmentResponse {
        Outcome(JudgmentOutcome),
        Error(JudgmentError),
        Cancelled,
        PendingUntilReleasedOrCancelled {
            started: oneshot::Sender<()>,
            release: oneshot::Receiver<()>,
            outcome: JudgmentOutcome,
        },
    }

    #[derive(Debug, Clone)]
    struct ScriptedJudgmentSource {
        responses: Arc<StdMutex<Vec<ScriptedJudgmentResponse>>>,
        calls: Arc<AtomicUsize>,
    }

    impl ScriptedJudgmentSource {
        fn new(responses: Vec<ScriptedJudgmentResponse>) -> Self {
            Self {
                responses: Arc::new(StdMutex::new(responses.into_iter().rev().collect())),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn with_outcome(outcome: JudgmentOutcome) -> Self {
            Self::new(vec![ScriptedJudgmentResponse::Outcome(outcome)])
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl JudgmentSource for ScriptedJudgmentSource {
        fn judge<'a>(
            &'a self,
            _request: crate::judgment::JudgmentRequest,
            context: JudgmentContext,
        ) -> JudgmentFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let response = self
                .responses
                .lock()
                .expect("judgment response mutex should not be poisoned")
                .pop();
            Box::pin(async move {
                if context.cancellation_token().is_cancelled() {
                    return Err(JudgmentError::Cancelled);
                }

                match response.expect("scripted judgment response should exist") {
                    ScriptedJudgmentResponse::Outcome(outcome) => Ok(outcome),
                    ScriptedJudgmentResponse::Error(error) => Err(error),
                    ScriptedJudgmentResponse::Cancelled => Err(JudgmentError::Cancelled),
                    ScriptedJudgmentResponse::PendingUntilReleasedOrCancelled {
                        started,
                        release,
                        outcome,
                    } => {
                        let _ = started.send(());
                        tokio::select! {
                            biased;
                            () = context.cancellation_token().cancelled() => {
                                Err(JudgmentError::Cancelled)
                            }
                            signal = release => {
                                signal.map_err(|_| JudgmentError::Cancelled)?;
                                Ok(outcome)
                            }
                        }
                    }
                }
            })
        }
    }

    fn activated_memory(id: &str, text: &str, evidence_artifact: &str) -> ActivatedMemory {
        let item = memory_item(id, text, evidence_artifact, &["topic"]);
        let score = MemoryActivationScore::new(1, 1, 0.8).expect("valid memory score");
        ActivatedMemory::new(
            item,
            score,
            vec![
                MemoryActivationReason::ScopeAllowed,
                MemoryActivationReason::trigger_matched("topic").expect("valid trigger"),
                MemoryActivationReason::ranked(score),
            ],
            crate::memory::MemoryActivationProvenance::new(
                "topic",
                vec![MemoryScope::Session, MemoryScope::Task, MemoryScope::Step],
                MemoryActivationSourceKind::UserQuery,
                "test source",
            )
            .expect("valid provenance"),
        )
        .expect("valid activated memory")
    }

    fn memory_item(id: &str, text: &str, evidence_artifact: &str, triggers: &[&str]) -> MemoryItem {
        MemoryItem::new(
            MemoryId::new(id).expect("valid memory id"),
            MemoryScope::Session,
            text,
            vec![
                MemoryEvidence::new(
                    "primary source",
                    EvidenceRef::new(
                        artifact_id(evidence_artifact),
                        EvidenceLocator::whole_artifact(),
                    ),
                )
                .expect("valid memory evidence"),
            ],
            MemoryItemSelection::new(
                triggers
                    .iter()
                    .map(|trigger| (*trigger).to_owned())
                    .collect(),
                0.8,
                1,
                None,
            )
            .expect("valid memory selection"),
        )
        .expect("valid memory item")
    }

    fn activated_memory_with_unreadable_evidence(id: &str) -> ActivatedMemory {
        activated_memory(id, "Unreadable evidence memory.", &format!("{id}-missing"))
    }

    fn record_memory_artifact(runtime: &Runtime, artifact_id_value: &str, content: &str) {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_artifact_state(
                ArtifactRef::new(artifact_id(artifact_id_value), ArtifactKind::Text),
                ArtifactContent::text(content),
            )
            .expect("memory artifact records");
    }

    fn record_prior_failed_tool_result(runtime: &Runtime, content: &str) {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        let call = PendingToolCall::new(
            ToolCallId::new("call-prior-process").expect("valid tool call id"),
            ToolName::new("process_command").expect("valid tool name"),
            ToolCallArguments::try_from(serde_json::json!({
                "argv": ["cargo", "test"],
                "cwd": "."
            }))
            .expect("valid arguments"),
        );
        session
            .record_tool_call_pending(call.clone())
            .expect("prior pending call records");
        session
            .submit_tool_execution_outcome(
                call.id(),
                ToolCallResultStatus::Failed,
                ArtifactContent::json(content.to_owned()),
                Some(
                    merry_core::ErrorInfo::new("process_action_failed", "process action failed")
                        .expect("valid diagnostic"),
                ),
                None,
            )
            .expect("prior failed tool result records");
    }

    fn record_memory_item(runtime: &Runtime, item: MemoryItem) {
        let mut session = runtime
            .inner
            .session
            .try_lock()
            .expect("session lock is free");
        session
            .record_memory_item(item)
            .expect("memory item records");
    }

    fn runtime_with_provider_and_single_memory(
        session: &str,
        provider: RecordingModelProvider,
        memory_id: &str,
        memory_text: &str,
        memory_artifact_id: &str,
    ) -> (Runtime, ScriptedMemoryActivationSource) {
        let memory = activated_memory(memory_id, memory_text, memory_artifact_id);
        let source = ScriptedMemoryActivationSource::new(vec![vec![memory.clone()]]);
        let runtime = runtime_with_provider_and_memory_source(session, provider, source.clone());
        record_memory_artifact(
            &runtime,
            memory_artifact_id,
            "exact evidence for lifecycle memory",
        );
        record_memory_item(&runtime, memory.item().clone());
        (runtime, source)
    }

    async fn compiled_context_snapshot(runtime: &Runtime) -> String {
        crate::ContextCompiler::new()
            .compile(&runtime.context_snapshot().await)
            .expect("context compiles")
            .to_snapshot()
    }

    #[derive(Debug, PartialEq)]
    struct JudgmentHarnessState {
        context: String,
        ledger: crate::LedgerProjectionSnapshot,
        pending_tool_calls: Vec<PendingToolCall>,
        judgment_records: Vec<JudgmentRecord>,
    }

    async fn judgment_harness_state(runtime: &Runtime) -> JudgmentHarnessState {
        let session = runtime.inner.session.lock().await;
        JudgmentHarnessState {
            context: crate::ContextCompiler::new()
                .compile(&session.context_snapshot())
                .expect("context compiles")
                .to_snapshot(),
            ledger: session.ledger_projection(),
            pending_tool_calls: session.pending_tool_calls(),
            judgment_records: session.judgment_records(),
        }
    }

    async fn assert_activated_memory_projection_cleared(runtime: &Runtime) {
        assert_eq!(compiled_context_snapshot(runtime).await, "");
    }

    async fn assert_activated_memory_projection_retained(
        runtime: &Runtime,
        memory_id: &str,
        memory_text: &str,
    ) {
        let snapshot = compiled_context_snapshot(runtime).await;
        assert!(
            snapshot.contains(&format!("memory:{memory_id}")),
            "compiled context should retain memory id {memory_id}; snapshot:\n{snapshot}"
        );
        assert!(
            snapshot.contains(&format!("memory-text:{memory_text}")),
            "compiled context should retain memory text for {memory_id}; snapshot:\n{snapshot}"
        );
    }

    #[derive(Debug)]
    enum ScriptedMemoryActivationResponse {
        Memories(Vec<ActivatedMemory>),
        Error(MemoryError),
        CancelThenMemories {
            token: CancellationToken,
            memories: Vec<ActivatedMemory>,
        },
        PendingUntilDropped {
            started: oneshot::Sender<()>,
            dropped: oneshot::Sender<()>,
        },
    }

    impl ScriptedMemoryActivationResponse {
        async fn into_result(self) -> Result<Vec<ActivatedMemory>, MemoryError> {
            match self {
                Self::Memories(memories) => Ok(memories),
                Self::Error(error) => Err(error),
                Self::CancelThenMemories { token, memories } => {
                    token.cancel();
                    Ok(memories)
                }
                Self::PendingUntilDropped { started, dropped } => {
                    let _notify_on_drop = NotifyOnDrop::new(dropped);
                    let _ = started.send(());
                    std::future::pending::<Result<Vec<ActivatedMemory>, MemoryError>>().await
                }
            }
        }
    }

    #[derive(Debug, Clone)]
    struct ScriptedMemoryActivationSource {
        responses: Arc<StdMutex<Vec<ScriptedMemoryActivationResponse>>>,
        calls: Arc<AtomicUsize>,
        observed_queries: Arc<StdMutex<Vec<String>>>,
    }

    impl ScriptedMemoryActivationSource {
        fn new(responses: Vec<Vec<ActivatedMemory>>) -> Self {
            Self::with_script(
                responses
                    .into_iter()
                    .map(ScriptedMemoryActivationResponse::Memories)
                    .collect(),
            )
        }

        fn with_script(responses: Vec<ScriptedMemoryActivationResponse>) -> Self {
            Self {
                responses: Arc::new(StdMutex::new(responses.into_iter().rev().collect())),
                calls: Arc::new(AtomicUsize::new(0)),
                observed_queries: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl MemoryActivationSource for ScriptedMemoryActivationSource {
        fn activate<'a>(
            &'a self,
            seed: crate::memory::MemoryActivationSeed,
            _candidates: Vec<MemoryItem>,
            context: MemoryActivationContext,
        ) -> MemoryActivationFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.observed_queries
                .lock()
                .expect("observed query mutex should not be poisoned")
                .push(seed.query().to_owned());
            let response = self
                .responses
                .lock()
                .expect("memory response mutex should not be poisoned")
                .pop();
            Box::pin(async move {
                if context.cancellation_token().is_cancelled() {
                    return Ok(Vec::new());
                }

                match response {
                    Some(response) => response.into_result().await,
                    None => Ok(Vec::new()),
                }
            })
        }
    }

    fn pending_memory_activation_source() -> (
        ScriptedMemoryActivationSource,
        oneshot::Receiver<()>,
        oneshot::Receiver<()>,
    ) {
        let (started_tx, started_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let source = ScriptedMemoryActivationSource::with_script(vec![
            ScriptedMemoryActivationResponse::PendingUntilDropped {
                started: started_tx,
                dropped: dropped_tx,
            },
        ]);

        (source, started_rx, dropped_rx)
    }

    #[derive(Debug)]
    enum ScriptedModelProviderResponse {
        SetupError(ModelError),
        PendingSetup(oneshot::Sender<()>),
        PendingSetupWithDrop {
            started: oneshot::Sender<()>,
            dropped: oneshot::Sender<()>,
        },
        Stream(Vec<Result<ModelEvent, ModelError>>),
    }

    struct NotifyOnDrop(Option<oneshot::Sender<()>>);

    impl NotifyOnDrop {
        fn new(sender: oneshot::Sender<()>) -> Self {
            Self(Some(sender))
        }
    }

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[derive(Debug, Clone)]
    struct RecordingModelProvider {
        requests: Arc<StdMutex<Vec<ModelRequest>>>,
        calls: Arc<AtomicUsize>,
        responses: Arc<StdMutex<Vec<ScriptedModelProviderResponse>>>,
    }

    impl RecordingModelProvider {
        fn new() -> Self {
            Self::with_script(Vec::new())
        }

        fn with_script(responses: Vec<ScriptedModelProviderResponse>) -> Self {
            Self {
                requests: Arc::new(StdMutex::new(Vec::new())),
                calls: Arc::new(AtomicUsize::new(0)),
                responses: Arc::new(StdMutex::new(responses.into_iter().rev().collect())),
            }
        }

        fn recorded_requests(&self) -> Vec<ModelRequest> {
            self.requests
                .lock()
                .expect("recorded requests mutex should not be poisoned")
                .clone()
        }

        fn next_response(&self) -> ScriptedModelProviderResponse {
            self.responses
                .lock()
                .expect("model response mutex should not be poisoned")
                .pop()
                .unwrap_or_else(|| {
                    ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())])
                })
        }
    }

    impl ModelProvider for RecordingModelProvider {
        fn name(&self) -> &merry_core::ProviderName {
            static PROVIDER_NAME: std::sync::OnceLock<merry_core::ProviderName> =
                std::sync::OnceLock::new();
            PROVIDER_NAME.get_or_init(|| {
                merry_core::ProviderName::new("runtime-test-provider").expect("valid provider name")
            })
        }

        fn capabilities(&self) -> &ModelCapabilities {
            static CAPABILITIES: std::sync::OnceLock<ModelCapabilities> =
                std::sync::OnceLock::new();
            CAPABILITIES.get_or_init(|| {
                ModelCapabilities::new(true, true, false, true, None, None)
                    .expect("valid capabilities")
            })
        }

        fn stream_model<'a>(
            &'a self,
            request: ModelRequest,
            context: ModelStreamContext,
        ) -> ModelProviderFuture<'a, Result<ModelEventStream, ModelError>> {
            Box::pin(async move {
                if context.cancellation_token().is_cancelled() {
                    return Err(ModelError::Cancelled);
                }

                self.calls.fetch_add(1, Ordering::SeqCst);
                self.requests
                    .lock()
                    .expect("recorded requests mutex should not be poisoned")
                    .push(request);
                match self.next_response() {
                    ScriptedModelProviderResponse::SetupError(error) => Err(error),
                    ScriptedModelProviderResponse::PendingSetup(started) => {
                        let _ = started.send(());
                        std::future::pending::<Result<ModelEventStream, ModelError>>().await
                    }
                    ScriptedModelProviderResponse::PendingSetupWithDrop { started, dropped } => {
                        let _notify_on_drop = NotifyOnDrop::new(dropped);
                        let _ = started.send(());
                        std::future::pending::<Result<ModelEventStream, ModelError>>().await
                    }
                    ScriptedModelProviderResponse::Stream(events) => {
                        let stream: ModelEventStream = Box::pin(futures_util::stream::iter(events));
                        Ok(stream)
                    }
                }
            })
        }
    }

    fn runtime_with_provider_and_memory_source<S>(
        session: &str,
        provider: RecordingModelProvider,
        source: S,
    ) -> Runtime
    where
        S: MemoryActivationSource + 'static,
    {
        Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: session_id(session),
                session: Mutex::new(SessionState::new(session_id(session))),
                active_step: Arc::new(AtomicBool::new(false)),
                memory_projection_epoch: AtomicU64::new(0),
                event_buffer_size: NonZeroUsize::new(16).expect("non-zero buffer"),
                model_configs: model_configs_with_primary(provider),
                automatic_compaction: AutomaticCompactionConfig::default(),
                capabilities: crate::RuntimeCapabilities::default(),
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(source),
                allow_low_risk_workspace_patches: false,
                low_risk_process_runner: None,
                read_only_shell_process_runner: None,
                accepted_local_workspace_process_runner: None,
                progress_commentary: false,
                runtime_trust_level: RuntimeTrustLevel::Agent,
                permission_review_mode: PermissionReviewMode::DefaultForTrust,
                permission_admission_source: None,
                permissioned_process_runner_factory: None,
                subagent_manager: None,
            }),
        }
    }

    fn runtime_with_provider(session: &str, provider: RecordingModelProvider) -> Runtime {
        Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: session_id(session),
                session: Mutex::new(SessionState::new(session_id(session))),
                active_step: Arc::new(AtomicBool::new(false)),
                memory_projection_epoch: AtomicU64::new(0),
                event_buffer_size: NonZeroUsize::new(16).expect("non-zero buffer"),
                model_configs: model_configs_with_primary(provider),
                automatic_compaction: AutomaticCompactionConfig::default(),
                capabilities: crate::RuntimeCapabilities::default(),
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(crate::memory::StoredMemoryActivationSource),
                allow_low_risk_workspace_patches: false,
                low_risk_process_runner: None,
                read_only_shell_process_runner: None,
                accepted_local_workspace_process_runner: None,
                progress_commentary: false,
                runtime_trust_level: RuntimeTrustLevel::Agent,
                permission_review_mode: PermissionReviewMode::DefaultForTrust,
                permission_admission_source: None,
                permissioned_process_runner_factory: None,
                subagent_manager: None,
            }),
        }
    }

    fn runtime_without_provider_with_memory_source<S>(session: &str, source: S) -> Runtime
    where
        S: MemoryActivationSource + 'static,
    {
        Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: session_id(session),
                session: Mutex::new(SessionState::new(session_id(session))),
                active_step: Arc::new(AtomicBool::new(false)),
                memory_projection_epoch: AtomicU64::new(0),
                event_buffer_size: NonZeroUsize::new(16).expect("non-zero buffer"),
                model_configs: RuntimeModelConfigs::default(),
                automatic_compaction: AutomaticCompactionConfig::default(),
                capabilities: crate::RuntimeCapabilities::default(),
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(source),
                allow_low_risk_workspace_patches: false,
                low_risk_process_runner: None,
                read_only_shell_process_runner: None,
                accepted_local_workspace_process_runner: None,
                progress_commentary: false,
                runtime_trust_level: RuntimeTrustLevel::Agent,
                permission_review_mode: PermissionReviewMode::DefaultForTrust,
                permission_admission_source: None,
                permissioned_process_runner_factory: None,
                subagent_manager: None,
            }),
        }
    }

    fn registered_tool_spec() -> ToolSpec {
        let schema = Schema::try_from(json!({ "type": "object" }))
            .expect("test schema should be a JSON schema");
        ToolSpec::new(
            ToolName::new("registered_tool").expect("valid tool name"),
            "Registered test tool",
            ToolInputSchema::new(schema).expect("valid tool schema"),
        )
        .expect("valid tool spec")
    }

    fn policy_tool_spec(name: &str) -> ToolSpec {
        let schema = Schema::try_from(json!({ "type": "object" }))
            .expect("test schema should be a JSON schema");
        ToolSpec::new(
            ToolName::new(name).expect("valid tool name"),
            "Policy test tool",
            ToolInputSchema::new(schema).expect("valid tool schema"),
        )
        .expect("valid tool spec")
    }

    fn policy_pending_tool_call(id: &str, name: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new(id).expect("valid tool call id"),
            ToolName::new(name).expect("valid tool name"),
            ToolCallArguments::new(Default::default()),
        )
    }

    fn permission_pending_tool_call(id: &str, reason: &str, argv: &[&str]) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new(id).expect("valid tool call id"),
            ToolName::new("request_permissions").expect("valid tool name"),
            ToolCallArguments::try_from(serde_json::json!({
                "reason": reason,
                "requested": { "network": true },
                "for_action": {
                    "kind": "process",
                    "argv": argv,
                    "cwd": ".",
                }
            }))
            .expect("valid permission arguments"),
        )
    }

    fn invalid_permission_pending_tool_call(id: &str) -> PendingToolCall {
        PendingToolCall::new(
            ToolCallId::new(id).expect("valid tool call id"),
            ToolName::new("request_permissions").expect("valid tool name"),
            ToolCallArguments::try_from(serde_json::json!({
                "requested": {},
                "for_action": {
                    "kind": "process",
                    "argv": ["cargo", "test"],
                    "cwd": ".",
                }
            }))
            .expect("valid JSON arguments"),
        )
    }

    fn resolved_tool_result(events: &[RuntimeEvent]) -> &merry_core::ToolCallResult {
        events
            .iter()
            .find_map(|event| match &event.kind {
                RuntimeEventKind::ToolCallResolved { result } => Some(result),
                _ => None,
            })
            .expect("tool call should resolve")
    }

    async fn register_policy_pending_tool(
        session: &str,
        tool_name: &str,
        call_id: &str,
        action_kind: ToolActionKind,
        executor: impl ToolExecutor + 'static,
    ) -> (Runtime, PendingToolCall) {
        register_policy_pending_registered_tool(
            session,
            tool_name,
            call_id,
            RegisteredTool::new(policy_tool_spec(tool_name), Arc::new(executor), action_kind),
        )
        .await
    }

    async fn register_policy_pending_registered_tool(
        session: &str,
        tool_name: &str,
        call_id: &str,
        tool: RegisteredTool,
    ) -> (Runtime, PendingToolCall) {
        register_policy_pending_registered_tool_with_builder(
            session,
            tool_name,
            call_id,
            tool,
            RuntimeBuilder::build,
        )
        .await
    }

    async fn register_policy_pending_registered_tool_with_builder(
        session: &str,
        tool_name: &str,
        call_id: &str,
        tool: RegisteredTool,
        configure: impl FnOnce(RuntimeBuilder) -> Result<Runtime, RuntimeError>,
    ) -> (Runtime, PendingToolCall) {
        let spec = policy_tool_spec(tool_name);
        let pending = policy_pending_tool_call(call_id, spec.name().as_str());
        let runtime = configure(Runtime::builder(session_id(session)).register_tool(tool))
            .expect("runtime should build");
        {
            let mut session = runtime.inner.session.lock().await;
            session.record_session_started_if_needed();
            session
                .record_tool_call_pending(pending.clone())
                .expect("pending call should record");
        }
        (runtime, pending)
    }

    async fn register_permission_pending_tool_with_builder(
        session_id_value: &str,
        call_id: &str,
        configure: impl FnOnce(RuntimeBuilder) -> Result<Runtime, RuntimeError>,
    ) -> (Runtime, PendingToolCall) {
        let pending = permission_pending_tool_call(
            call_id,
            "Need network for this exact command.",
            &["cargo", "test"],
        );
        let runtime = configure(
            Runtime::builder(session_id(session_id_value))
                .register_tool(request_permissions_tool().expect("permission tool builds")),
        )
        .expect("runtime should build");
        {
            let mut session = runtime.inner.session.lock().await;
            session.record_session_started_if_needed();
            session.record_user_message_body(
                "Please run cargo test; if network is blocked, request network for that command.",
            );
            session
                .record_tool_call_pending(pending.clone())
                .expect("pending call should record");
        }
        (runtime, pending)
    }

    async fn denied_action_content(
        runtime: &Runtime,
        events: &[RuntimeEvent],
    ) -> serde_json::Value {
        let result = resolved_tool_result(events);
        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("denial artifact should be readable");
        let text = content
            .as_text()
            .expect("denial artifact should be textual JSON");
        serde_json::from_str(text).expect("denial artifact should parse as JSON")
    }

    async fn action_audit_records(
        runtime: &Runtime,
    ) -> Vec<crate::action_audit::ActionAuditRecord> {
        let session = runtime.inner.session.lock().await;
        session.action_audit_snapshot().records().to_vec()
    }

    fn lifecycle_kinds(
        runtime_projection: &crate::LedgerProjectionSnapshot,
    ) -> Vec<LedgerFactKind> {
        runtime_projection
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                LedgerProjection::Lifecycle { kind, .. } => Some(*kind),
                LedgerProjection::Fact { .. } => None,
            })
            .collect()
    }

    fn assert_lifecycle_order(
        lifecycle_kinds: &[LedgerFactKind],
        before: LedgerFactKind,
        after: LedgerFactKind,
    ) {
        let before_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == before)
            .expect("before lifecycle kind should exist");
        let after_index = lifecycle_kinds
            .iter()
            .position(|kind| *kind == after)
            .expect("after lifecycle kind should exist");
        assert!(
            before_index < after_index,
            "{before:?} should be recorded before {after:?}"
        );
    }

    fn assert_sanitized_policy_denial_content(content: &serde_json::Value, tool_name: &str) {
        assert_eq!(
            content,
            &json!({
                "ok": false,
                "tool": tool_name,
                "error": {
                    "code": DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED,
                    "message": TOOL_ACTION_POLICY_DENIED_MESSAGE
                }
            })
        );
        assert!(content.get("call_id").is_none());
        assert!(content.get("action_kind").is_none());
        assert!(content.get("policy").is_none());
        assert!(content.get("reason").is_none());
        assert!(content.get("provider").is_none());
        assert!(content.get("provider_response").is_none());
        assert!(content.get("wire").is_none());
        assert!(content.get("previous_response_id").is_none());
    }

    fn event_kind_names_for_tool_execution(events: &[RuntimeEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event.kind {
                RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
                RuntimeEventKind::SessionStarted => "SessionStarted",
                RuntimeEventKind::StepStarted => "StepStarted",
                RuntimeEventKind::StepCompleted => "StepCompleted",
                RuntimeEventKind::Cancelled { .. } => "Cancelled",
                RuntimeEventKind::Failed { .. } => "Failed",
                RuntimeEventKind::ToolCallPending { .. } => "ToolCallPending",
                RuntimeEventKind::EvidenceReferenced { .. } => "EvidenceReferenced",
                RuntimeEventKind::SkillUsed { .. } => "SkillUsed",
                _ => "Other",
            })
            .collect()
    }

    #[derive(Clone)]
    struct SuccessfulToolExecutor {
        calls: Arc<AtomicUsize>,
    }

    impl SuccessfulToolExecutor {
        fn new() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ToolExecutor for SuccessfulToolExecutor {
        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            _context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(ToolExecutionOutcome::succeeded_text("ok\n"))
            })
        }
    }

    #[derive(Clone)]
    struct CancelDuringRuntimeControlExecutor {
        calls: Arc<AtomicUsize>,
        token_seen: Arc<StdMutex<Option<CancellationToken>>>,
    }

    impl CancelDuringRuntimeControlExecutor {
        fn new() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                token_seen: Arc::new(StdMutex::new(None)),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn token_seen(&self) -> CancellationToken {
            self.token_seen
                .lock()
                .expect("token mutex is not poisoned")
                .clone()
                .expect("executor should capture token")
        }
    }

    impl ToolExecutor for CancelDuringRuntimeControlExecutor {
        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                *self.token_seen.lock().expect("token mutex is not poisoned") =
                    Some(context.cancellation_token().clone());
                context.cancellation_token().cancel();
                Ok(ToolExecutionOutcome::succeeded_text(
                    "control state committed\n",
                ))
            })
        }
    }

    #[derive(Clone)]
    struct ProposingToolExecutor {
        execute_calls: Arc<AtomicUsize>,
        propose_calls: Arc<AtomicUsize>,
        wait_for_cancel: bool,
        record_approved_proposal: Arc<StdMutex<Vec<bool>>>,
        attach_execution_evidence: bool,
        preflight_outcome: Option<ToolExecutionOutcome>,
    }

    impl ProposingToolExecutor {
        fn immediate() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: false,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: true,
                preflight_outcome: None,
            }
        }

        fn cancelling() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: true,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: true,
                preflight_outcome: None,
            }
        }

        fn missing_execution_evidence() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: false,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: false,
                preflight_outcome: None,
            }
        }

        fn with_preflight_outcome(outcome: ToolExecutionOutcome) -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                wait_for_cancel: false,
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
                attach_execution_evidence: true,
                preflight_outcome: Some(outcome),
            }
        }

        fn execute_count(&self) -> usize {
            self.execute_calls.load(Ordering::SeqCst)
        }

        fn propose_count(&self) -> usize {
            self.propose_calls.load(Ordering::SeqCst)
        }

        fn approved_proposal_seen(&self) -> Vec<bool> {
            self.record_approved_proposal
                .lock()
                .expect("approved proposal records mutex should not be poisoned")
                .clone()
        }
    }

    impl ToolExecutor for ProposingToolExecutor {
        fn propose<'a>(
            &'a self,
            call: PendingToolCall,
            context: ToolExecutionContext,
        ) -> ToolActionProposalFuture<'a> {
            Box::pin(async move {
                self.propose_calls.fetch_add(1, Ordering::SeqCst);
                if self.wait_for_cancel {
                    context.cancellation_token().cancelled().await;
                    return Err(ToolExecutionError::Cancelled);
                }
                if let Some(outcome) = self.preflight_outcome.clone() {
                    return Ok(ToolActionPreflight::Outcome(outcome));
                }

                let patch = WorkspacePatchProposal::new(
                    "notes/proposed.txt",
                    3,
                    7,
                    20,
                    24,
                    "fnv1a64:0000000000000001",
                    "fnv1a64:0000000000000002",
                )
                .expect("test proposal metadata is valid");
                Ok(ToolActionPreflight::Proposal(
                    ActionProposal::new(
                        &call,
                        ToolActionKind::WorkspaceWrite,
                        "workspace patch",
                        "notes/proposed.txt",
                        "Replace one matched preimage in notes/proposed.txt",
                        ActionProposalEvidence::WorkspacePatch(patch),
                    )
                    .expect("test action proposal is valid"),
                ))
            })
        }

        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move {
                self.execute_calls.fetch_add(1, Ordering::SeqCst);
                self.record_approved_proposal
                    .lock()
                    .expect("approved proposal records mutex should not be poisoned")
                    .push(context.approved_workspace_patch().is_some());
                if !self.attach_execution_evidence {
                    return Ok(ToolExecutionOutcome::succeeded_text(
                        "patched without evidence\n",
                    ));
                }
                let evidence = WorkspacePatchExecutionEvidence::new(
                    "notes/proposed.txt",
                    3,
                    7,
                    20,
                    24,
                    "fnv1a64:0000000000000001",
                    "fnv1a64:0000000000000002",
                )
                .expect("test execution evidence is valid");
                Ok(ToolExecutionOutcome::succeeded_text("patched\n")
                    .with_execution_evidence(ActionExecutionEvidence::WorkspacePatch(evidence)))
            })
        }
    }

    #[derive(Clone)]
    struct ProcessProposingToolExecutor {
        execute_calls: Arc<AtomicUsize>,
        propose_calls: Arc<AtomicUsize>,
        argv: Vec<String>,
        stdin_text: Option<String>,
    }

    impl ProcessProposingToolExecutor {
        fn new() -> Self {
            Self::with_argv(["rustc", "--version"])
        }

        fn with_argv<const N: usize>(argv: [&str; N]) -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                argv: argv.into_iter().map(str::to_owned).collect(),
                stdin_text: None,
            }
        }

        fn with_stdin_text(stdin_text: impl Into<String>) -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                argv: ["cargo", "test", "-p", "merry-runtime"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                stdin_text: Some(stdin_text.into()),
            }
        }

        fn execute_count(&self) -> usize {
            self.execute_calls.load(Ordering::SeqCst)
        }

        fn propose_count(&self) -> usize {
            self.propose_calls.load(Ordering::SeqCst)
        }
    }

    impl ToolExecutor for ProcessProposingToolExecutor {
        fn propose<'a>(
            &'a self,
            call: PendingToolCall,
            _context: ToolExecutionContext,
        ) -> ToolActionProposalFuture<'a> {
            Box::pin(async move {
                self.propose_calls.fetch_add(1, Ordering::SeqCst);
                let intent = ProcessActionIntent::new(
                    self.argv.clone(),
                    Some(".".to_owned()),
                    ProcessEnvPolicy::empty(),
                    self.stdin_text.clone(),
                    16 * 1024,
                    16 * 1024,
                )
                .expect("test process intent is valid");
                Ok(ToolActionPreflight::Proposal(
                    ActionProposal::new(
                        &call,
                        ToolActionKind::CommandExec,
                        "process action",
                        self.argv.join(" "),
                        "Run proposed process action.",
                        ActionProposalEvidence::ProcessAction(intent),
                    )
                    .expect("test process action proposal is valid"),
                ))
            })
        }

        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            _context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move {
                self.execute_calls.fetch_add(1, Ordering::SeqCst);
                Ok(ToolExecutionOutcome::succeeded_text(
                    "process execution must not be reached in SP1\n",
                ))
            })
        }
    }

    #[derive(Clone)]
    struct FakeProcessRunner {
        calls: Arc<AtomicUsize>,
        observed_intents: Arc<StdMutex<Vec<ProcessActionIntent>>>,
        response: Arc<StdMutex<Option<FakeProcessRunnerResponse>>>,
    }

    impl FakeProcessRunner {
        fn succeeding() -> Self {
            Self::with_response(FakeProcessRunnerResponse::Success {
                stdout_text: "runtime tests passed\n".to_owned(),
                stdout_truncated: false,
                stderr_text: String::new(),
                stderr_truncated: false,
            })
        }

        fn succeeding_with_truncated_stdout() -> Self {
            Self::with_response(FakeProcessRunnerResponse::Success {
                stdout_text: "partial runtime output\n".to_owned(),
                stdout_truncated: true,
                stderr_text: String::new(),
                stderr_truncated: false,
            })
        }

        fn cancelling() -> Self {
            Self::with_response(FakeProcessRunnerResponse::Error(
                ProcessRunnerError::Cancelled,
            ))
        }

        fn infrastructure_failure(message: &str) -> Self {
            Self::with_response(FakeProcessRunnerResponse::Error(
                ProcessRunnerError::infrastructure(message),
            ))
        }

        fn succeeding_then_cancelling_token() -> Self {
            Self::with_response(FakeProcessRunnerResponse::SuccessThenCancel {
                stdout_text: "runtime tests passed after token cancellation\n".to_owned(),
                stdout_truncated: false,
                stderr_text: String::new(),
                stderr_truncated: false,
            })
        }

        fn with_response(response: FakeProcessRunnerResponse) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                observed_intents: Arc::new(StdMutex::new(Vec::new())),
                response: Arc::new(StdMutex::new(Some(response))),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn observed_intents(&self) -> Vec<ProcessActionIntent> {
            self.observed_intents
                .lock()
                .expect("observed intents mutex should not be poisoned")
                .clone()
        }
    }

    enum FakeProcessRunnerResponse {
        Success {
            stdout_text: String,
            stdout_truncated: bool,
            stderr_text: String,
            stderr_truncated: bool,
        },
        SuccessThenCancel {
            stdout_text: String,
            stdout_truncated: bool,
            stderr_text: String,
            stderr_truncated: bool,
        },
        Error(ProcessRunnerError),
    }

    impl ProcessRunner for FakeProcessRunner {
        fn run<'a>(
            &'a self,
            intent: ProcessActionIntent,
            context: ProcessRunnerContext,
        ) -> ProcessRunnerFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.observed_intents
                    .lock()
                    .expect("observed intents mutex should not be poisoned")
                    .push(intent.clone());
                if context.cancellation_token().is_cancelled() {
                    return Err(ProcessRunnerError::Cancelled);
                }

                let response = self
                    .response
                    .lock()
                    .expect("process response mutex should not be poisoned")
                    .take()
                    .expect("scripted process response should exist");
                match response {
                    FakeProcessRunnerResponse::Success {
                        stdout_text,
                        stdout_truncated,
                        stderr_text,
                        stderr_truncated,
                    } => ProcessRunnerOutput::new(
                        &intent,
                        ProcessExitStatus::Exited(0),
                        stdout_text,
                        stdout_truncated,
                        stderr_text,
                        stderr_truncated,
                    )
                    .map_err(|source| ProcessRunnerError::infrastructure(source.to_string())),
                    FakeProcessRunnerResponse::SuccessThenCancel {
                        stdout_text,
                        stdout_truncated,
                        stderr_text,
                        stderr_truncated,
                    } => {
                        let output = ProcessRunnerOutput::new(
                            &intent,
                            ProcessExitStatus::Exited(0),
                            stdout_text,
                            stdout_truncated,
                            stderr_text,
                            stderr_truncated,
                        )
                        .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))?;
                        context.cancellation_token().cancel();
                        Ok(output)
                    }
                    FakeProcessRunnerResponse::Error(error) => Err(error),
                }
            })
        }
    }

    #[derive(Clone)]
    struct RecordingPermissionedProcessRunnerFactory {
        runner: Arc<dyn ProcessRunner>,
        calls: Arc<AtomicUsize>,
        observed_network_requests: Arc<StdMutex<Vec<bool>>>,
    }

    impl RecordingPermissionedProcessRunnerFactory {
        fn new(runner: Arc<dyn ProcessRunner>) -> Self {
            Self {
                runner,
                calls: Arc::new(AtomicUsize::new(0)),
                observed_network_requests: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn observed_network_requests(&self) -> Vec<bool> {
            self.observed_network_requests
                .lock()
                .expect("observed permission requests mutex should not be poisoned")
                .clone()
        }
    }

    impl PermissionedProcessRunnerFactory for RecordingPermissionedProcessRunnerFactory {
        fn runner_for(&self, request: &crate::PermissionRequest) -> Arc<dyn ProcessRunner> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.observed_network_requests
                .lock()
                .expect("observed permission requests mutex should not be poisoned")
                .push(request.requests_network());
            Arc::clone(&self.runner)
        }
    }

    #[derive(Clone)]
    struct StaticPermissionAdmissionSource {
        decision: crate::PermissionAdmissionDecision,
        calls: Arc<AtomicUsize>,
    }

    impl StaticPermissionAdmissionSource {
        fn approving() -> Self {
            Self {
                decision: crate::PermissionAdmissionDecision::approved("host approved"),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl crate::PermissionAdmissionSource for StaticPermissionAdmissionSource {
        fn review<'a>(
            &'a self,
            _request: crate::PermissionRequest,
            _context: crate::PermissionAdmissionContext,
        ) -> crate::PermissionAdmissionFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(self.decision.clone())
            })
        }
    }

    #[derive(Clone)]
    struct CancellingOptInPatchExecutor {
        execute_calls: Arc<AtomicUsize>,
        propose_calls: Arc<AtomicUsize>,
        side_effect: Arc<AtomicBool>,
        record_approved_proposal: Arc<StdMutex<Vec<bool>>>,
    }

    impl CancellingOptInPatchExecutor {
        fn new() -> Self {
            Self {
                execute_calls: Arc::new(AtomicUsize::new(0)),
                propose_calls: Arc::new(AtomicUsize::new(0)),
                side_effect: Arc::new(AtomicBool::new(false)),
                record_approved_proposal: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn execute_count(&self) -> usize {
            self.execute_calls.load(Ordering::SeqCst)
        }

        fn propose_count(&self) -> usize {
            self.propose_calls.load(Ordering::SeqCst)
        }

        fn side_effect_happened(&self) -> bool {
            self.side_effect.load(Ordering::SeqCst)
        }

        fn approved_proposal_seen(&self) -> Vec<bool> {
            self.record_approved_proposal
                .lock()
                .expect("approved proposal records mutex should not be poisoned")
                .clone()
        }
    }

    impl ToolExecutor for CancellingOptInPatchExecutor {
        fn propose<'a>(
            &'a self,
            call: PendingToolCall,
            _context: ToolExecutionContext,
        ) -> ToolActionProposalFuture<'a> {
            Box::pin(async move {
                self.propose_calls.fetch_add(1, Ordering::SeqCst);
                let patch = WorkspacePatchProposal::new(
                    "notes/proposed.txt",
                    3,
                    7,
                    20,
                    24,
                    "fnv1a64:0000000000000003",
                    "fnv1a64:0000000000000004",
                )
                .expect("test proposal metadata is valid");
                Ok(ToolActionPreflight::Proposal(
                    ActionProposal::new(
                        &call,
                        ToolActionKind::WorkspaceWrite,
                        "workspace patch",
                        "notes/proposed.txt",
                        "Replace one matched preimage in notes/proposed.txt",
                        ActionProposalEvidence::WorkspacePatch(patch),
                    )
                    .expect("test action proposal is valid"),
                ))
            })
        }

        fn execute<'a>(
            &'a self,
            _call: PendingToolCall,
            context: ToolExecutionContext,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async move {
                self.execute_calls.fetch_add(1, Ordering::SeqCst);
                self.record_approved_proposal
                    .lock()
                    .expect("approved proposal records mutex should not be poisoned")
                    .push(context.approved_workspace_patch().is_some());
                self.side_effect.store(true, Ordering::SeqCst);
                context.cancellation_token().cancel();
                let evidence = WorkspacePatchExecutionEvidence::new(
                    "notes/proposed.txt",
                    3,
                    7,
                    20,
                    24,
                    "fnv1a64:0000000000000003",
                    "fnv1a64:0000000000000004",
                )
                .expect("test execution evidence is valid");
                Ok(ToolExecutionOutcome::succeeded_text("patched\n")
                    .with_execution_evidence(ActionExecutionEvidence::WorkspacePatch(evidence)))
            })
        }
    }
}
