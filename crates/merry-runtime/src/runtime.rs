//! Runtime builder and step execution skeleton.
//!
//! [`Runtime`] is the MVP facade for session-owned state. Step execution and
//! direct mutation APIs admit one active operation at a time, record durable
//! session state before returning observable events where applicable, and keep
//! provider wire details behind the `merry-llm` provider boundary.

use crate::{
    AcceptedLocalWorkspaceProcessAdmission, ArtifactContent, CheckpointId, CheckpointRefExcerpt,
    CheckpointRefId, CitationCompactionInput, CitationCompactionPolicy, CompactedCheckpoint,
    CompactedCheckpointSummary, CompactionError, CompactionOutcome, ContextEntry, ContextSummary,
    LedgerProjectionSnapshot, ProcessRunner, ProjectRules, RuntimeCapabilities, RuntimeError,
    RuntimeEventStream, RuntimeModelRole, RuntimeProfile, SessionContextSnapshot, SkillCatalog,
    TaskAnchor,
    event_stream::ActiveStepPermit,
    judgment::{JudgmentContext, JudgmentError, JudgmentRecord, JudgmentRequest, JudgmentSource},
    memory::{MemoryActivationSource, StoredMemoryActivationSource},
    model_config::RuntimeModelConfigs,
    permission::{PermissionAdmissionSource, PermissionReviewMode, RuntimeTrustLevel},
    process::{PermissionedProcessRunnerFactory, StaticPermissionedProcessRunnerFactory},
    session::{SessionState, is_runtime_reserved_artifact_id},
    step::{StepContext, StepInput},
    subagent::SubagentManager,
    tool::{RegisteredTool, ToolExecutionContext, ToolRegistry},
};
use merry_core::{
    ArtifactId, ArtifactRef, ErrorInfo, EvidenceLocator, EvidenceRef, PendingToolCall,
    RuntimeEvent, SessionId, ToolCallId, ToolCallResult,
};
use merry_llm::{GenerationConfig, ModelName, ModelProvider, ModelRetryPolicy};
use std::{
    collections::BTreeMap,
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

mod compaction;
mod events;
mod memory_activation;
mod model_output;
mod process_execution;
mod provider_request;
mod provider_step;
mod tool_execution;

use self::compaction::{compact_context_once_inner, default_automatic_compaction_policy};
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

const DEFAULT_EVENT_BUFFER_SIZE: usize = 16;
const DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED: &str = "tool_call_result_required";
const DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED: &str = "action_policy_denied";
const DIAGNOSTIC_TOOL_NOT_REGISTERED: &str = "tool_not_registered";
const TOOL_ACTION_POLICY_DENIED_MESSAGE: &str = "tool action was blocked by runtime policy";
const WORKSPACE_PATCH_TOOL_NAME: &str = "workspace_patch";
// MVP automatic compaction policy. Retaining two history items usually keeps
// the latest completed user/assistant pair raw; it is a policy default, not a
// semantic invariant.
const DEFAULT_AUTO_COMPACTION_TARGET_OUTPUT_TOKENS: u64 = 192;
const DEFAULT_AUTO_COMPACTION_MAX_OUTPUT_BYTES: usize = 8192;
const DEFAULT_AUTO_COMPACTION_RETAINED_RAW_TAIL_ITEMS: usize = 2;
const DEFAULT_AUTO_COMPACTION_MAX_REF_EXCERPT_BYTES: usize = 1200;
const DEFAULT_AUTO_COMPACTION_MAX_CARRIED_PRIOR_REFS: usize = 16;

/// Runtime-owned policy for automatic checkpoint compaction.
///
/// This controls the pre-provider hard-watermark compaction path. Manual
/// [`Runtime::compact_context_once`] calls still take an explicit
/// [`CitationCompactionPolicy`] so tests and callers can run one-off compaction
/// passes without mutating runtime construction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomaticCompactionConfig {
    enabled: bool,
    policy: CitationCompactionPolicy,
}

impl AutomaticCompactionConfig {
    /// Enables automatic hard-watermark compaction with the provided policy.
    #[must_use]
    pub fn enabled(policy: CitationCompactionPolicy) -> Self {
        Self {
            enabled: true,
            policy,
        }
    }

    /// Disables automatic hard-watermark compaction.
    ///
    /// The policy remains populated with defaults so disabled configs can be
    /// inspected or re-enabled by callers without constructing a dummy policy.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            policy: default_automatic_compaction_policy(),
        }
    }

    #[must_use]
    pub fn is_enabled(self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn policy(self) -> CitationCompactionPolicy {
        self.policy
    }
}

impl Default for AutomaticCompactionConfig {
    fn default() -> Self {
        Self::enabled(default_automatic_compaction_policy())
    }
}

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

    /// Records exact artifact state into the owning session and returns observable events.
    ///
    /// When this is the first observable action in the session, `SessionStarted`
    /// is returned before `ArtifactRecorded`.
    ///
    /// This direct mutation path acquires the active-step permit and therefore
    /// cannot run concurrently with [`Runtime::step`],
    /// [`Runtime::submit_tool_result`], or [`Runtime::execute_tool_call`]. State
    /// is written before returned events are handed to the caller.
    ///
    /// Artifact ids with runtime-reserved prefixes are rejected. Runtime-owned
    /// ids are used for internally generated artifacts such as assistant output
    /// and registered tool execution results.
    pub async fn record_artifact(
        &self,
        artifact: ArtifactRef,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        if is_runtime_reserved_artifact_id(artifact.id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: artifact.id().clone(),
            });
        }

        let mut session = self.inner.session.lock().await;
        session
            .record_artifact_events(artifact, content)
            .map_err(Into::into)
    }

    /// Resolves one pending tool call with an artifact-backed result.
    ///
    /// The artifact content is durably recorded before `ToolCallResolved` is
    /// emitted. The event carries only the artifact reference, not the payload.
    ///
    /// This is the manual result path for external tool runners. Callers choose
    /// the artifact id and must not use runtime-reserved artifact ids. The
    /// registered executor path is [`Runtime::execute_tool_call`], where runtime
    /// code owns the generated artifact id and result envelope.
    ///
    /// Cancellation or executor infrastructure failures do not resolve the call;
    /// a pending tool call remains pending until this method or
    /// [`Runtime::execute_tool_call`] records a durable result.
    pub async fn submit_tool_result(
        &self,
        result: ToolCallResult,
        content: ArtifactContent,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        self.submit_tool_result_with_active_permit(result, content, &_active_permit)
            .await
    }

    pub(crate) async fn submit_tool_result_with_active_permit(
        &self,
        result: ToolCallResult,
        content: ArtifactContent,
        _active_permit: &ActiveStepPermit,
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        if is_runtime_reserved_artifact_id(result.artifact().id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: result.artifact().id().clone(),
            });
        }

        let mut session = self.inner.session.lock().await;
        session.submit_tool_result(result, content)
    }

    pub(crate) async fn record_final_output_tool_call(
        &self,
        call: PendingToolCall,
    ) -> Result<(crate::FinalOutput, Vec<RuntimeEvent>), RuntimeError> {
        let json = serde_json::to_string(call.arguments().as_object())
            .expect("tool call arguments are JSON object values and must serialize");
        let mut session = self.inner.session.lock().await;
        session.record_final_output(call.id().clone(), json)
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

    /// Creates an exact evidence reference from artifact state owned by this session.
    ///
    /// Prefer this facade over reading [`crate::ArtifactRegistry`] directly. The
    /// returned reference is valid only for artifact content already owned by
    /// this runtime session.
    pub async fn evidence_ref(
        &self,
        artifact_id: &ArtifactId,
        locator: EvidenceLocator,
    ) -> Result<EvidenceRef, RuntimeError> {
        let session = self.inner.session.lock().await;
        session
            .evidence_ref(artifact_id, locator)
            .map_err(Into::into)
    }

    /// Reads exact artifact content already owned by this runtime session.
    ///
    /// This is an inspection facade over session-owned artifact state. It does
    /// not mutate runtime state, advance event sequence, or expose provider
    /// wire formats.
    pub async fn read_artifact_content(
        &self,
        artifact_id: &ArtifactId,
    ) -> Result<ArtifactContent, RuntimeError> {
        let session = self.inner.session.lock().await;
        session
            .read_artifact_content(artifact_id)
            .map_err(Into::into)
    }

    /// Records a structured context entry into the owning session.
    ///
    /// This is the raw/manual MVP direct context mutation surface. It appends
    /// summary-only context entries today after acquiring the active-step
    /// permit. It does not validate evidence readability, reject duplicate
    /// summary ids, emit runtime events, or write ledger facts.
    ///
    /// Direct writes are validated later when a session snapshot is compiled by
    /// [`ContextCompiler`]. They are not summary-draft promotion, do not record
    /// promotion lifecycle state, and are not governed by the internal
    /// summary-draft promotion acceptance/replay rules.
    pub async fn record_context_entry(&self, entry: ContextEntry) -> Result<(), RuntimeError> {
        let _active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        let mut session = self.inner.session.lock().await;
        session.record_context_entry(entry);
        Ok(())
    }

    /// Records a summary context entry into the owning session.
    ///
    /// Summaries are navigation only; exact supporting evidence must remain
    /// readable through session-owned artifacts before the summary can enter
    /// compiled context. This helper is the raw/manual MVP direct write path:
    /// it delegates to [`Runtime::record_context_entry`], so it records with
    /// the same active-step admission guard and without immediate evidence
    /// readability validation, duplicate-id rejection, runtime events, or
    /// ledger facts.
    ///
    /// This API is independent of the internal summary-draft promotion
    /// lifecycle. Calling it does not create promotion records, perform
    /// acceptance/replay checks, or authorize context mutation from judgment
    /// output.
    pub async fn record_context_summary(
        &self,
        summary: ContextSummary,
    ) -> Result<(), RuntimeError> {
        self.record_context_entry(ContextEntry::summary(summary))
            .await
    }

    /// Builds a sealed context snapshot from session-owned context and artifacts.
    ///
    /// The snapshot is opaque and session-owned. It exists so
    /// [`ContextCompiler`] can validate summaries against the matching artifact
    /// view without accepting arbitrary caller-assembled state.
    pub async fn context_snapshot(&self) -> SessionContextSnapshot {
        let session = self.inner.session.lock().await;
        session.context_snapshot()
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

    /// Builds a read-only deterministic projection of the task ledger.
    ///
    /// This is the preferred public read path for lifecycle and compact ledger
    /// facts. Direct [`crate::TaskLedger`] access is a low-level in-memory MVP
    /// primitive and should not be treated as the stable application-facing
    /// ledger API.
    pub async fn ledger_projection(&self) -> LedgerProjectionSnapshot {
        let session = self.inner.session.lock().await;
        session.ledger_projection()
    }

    /// Returns a snapshot of provider-neutral tool calls currently awaiting results.
    ///
    /// The returned calls are normalized Merry runtime state, not provider wire
    /// payloads. A call remains listed until a durable result is submitted or
    /// executed through a registered executor.
    pub async fn pending_tool_calls(&self) -> Vec<PendingToolCall> {
        let session = self.inner.session.lock().await;
        session.pending_tool_calls()
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

/// Builder for a Merry runtime.
///
/// The builder wires provider-neutral runtime configuration: event buffering,
/// one optional model provider, and zero or more runtime-owned tool executors.
/// Provider integrations stay outside this crate behind [`ModelProvider`].
pub struct RuntimeBuilder {
    session_id: SessionId,
    event_buffer_size: NonZeroUsize,
    model_configs: RuntimeModelConfigs,
    model_retry_policy: ModelRetryPolicy,
    automatic_compaction: AutomaticCompactionConfig,
    capabilities: RuntimeCapabilities,
    progress_commentary: bool,
    registered_tools: Vec<RegisteredTool>,
    initial_context_summaries: BTreeMap<String, String>,
    skill_catalog: Option<SkillCatalog>,
    project_rules: Option<ProjectRules>,
    task_anchor: Option<TaskAnchor>,
    compacted_checkpoint: Option<CompactedCheckpoint>,
    memory_activation_source: Arc<dyn MemoryActivationSource>,
    allow_bridge_tools: bool,
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

impl RuntimeBuilder {
    fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            event_buffer_size: NonZeroUsize::new(DEFAULT_EVENT_BUFFER_SIZE)
                .expect("default event buffer size is non-zero"),
            model_configs: RuntimeModelConfigs::default(),
            model_retry_policy: ModelRetryPolicy::default(),
            automatic_compaction: AutomaticCompactionConfig::default(),
            capabilities: crate::RuntimeCapabilities::default(),
            progress_commentary: false,
            registered_tools: Vec::new(),
            initial_context_summaries: BTreeMap::new(),
            skill_catalog: None,
            project_rules: None,
            task_anchor: None,
            compacted_checkpoint: None,
            memory_activation_source: Arc::new(StoredMemoryActivationSource),
            allow_bridge_tools: false,
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

    /// Sets the bounded event channel buffer size.
    ///
    /// Runtime event production uses a bounded channel. Backpressure is part of
    /// the state-before-event contract: producers reserve an event slot before
    /// mutating durable session state for the corresponding event.
    #[must_use]
    pub fn event_buffer_size(mut self, event_buffer_size: NonZeroUsize) -> Self {
        self.event_buffer_size = event_buffer_size;
        self
    }

    /// Sets the provider and model used by runtime steps.
    ///
    /// The provider receives normalized model requests and returns normalized
    /// model events from `merry-llm`. Provider response formats are not stored
    /// in runtime state.
    #[must_use]
    pub fn model_provider(mut self, provider: Arc<dyn ModelProvider>, model: ModelName) -> Self {
        self.model_configs.insert(
            RuntimeModelRole::Primary,
            provider,
            model,
            self.model_retry_policy,
        );
        self
    }

    /// Sets the provider and model for a runtime model role.
    ///
    /// Only [`RuntimeModelRole::Primary`] is used by normal runtime steps today.
    /// Non-primary roles are stored as runtime-owned configuration for future
    /// review gates and do not alter provider request compilation.
    #[must_use]
    pub fn model_provider_for_role(
        mut self,
        role: RuntimeModelRole,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
    ) -> Self {
        self.model_configs
            .insert(role, provider, model, self.model_retry_policy);
        self
    }

    /// Sets the retry policy applied to configured and subsequently configured
    /// model providers.
    #[must_use]
    pub fn model_retry_policy(mut self, policy: ModelRetryPolicy) -> Self {
        self.model_retry_policy = policy;
        self.model_configs.set_retry_policy(policy);
        self
    }

    /// Sets the runtime policy for automatic hard-watermark compaction.
    ///
    /// Automatic compaction runs only when a compiled provider request crosses
    /// the hard context watermark. The current step input is still outside the
    /// compaction input and is projected raw after any installed checkpoint.
    #[must_use]
    pub fn automatic_compaction(mut self, config: AutomaticCompactionConfig) -> Self {
        self.automatic_compaction = config;
        self
    }

    /// Registers a runtime-owned tool executor.
    ///
    /// Registering a tool makes its spec available to provider requests and
    /// lets [`Runtime::execute_tool_call`] resolve matching pending calls. It
    /// does not start an automatic tool loop.
    #[must_use]
    pub fn register_tool(mut self, tool: RegisteredTool) -> Self {
        self.registered_tools.push(tool);
        self
    }

    /// Applies a complete runtime profile while keeping this builder as the
    /// construction owner.
    pub fn with_profile(mut self, profile: RuntimeProfile) -> Result<Self, RuntimeError> {
        let parts = profile.into_parts();
        self.capabilities = parts.capabilities;
        if let Some(policy) = parts.model_retry_policy {
            self = self.model_retry_policy(policy);
        }
        self.progress_commentary = parts.progress_commentary;
        for (id, text) in parts.initial_context_summaries {
            self = self.initial_context_summary(&id, &text);
        }
        for tool in parts.registered_tools {
            self = self.register_tool(tool);
        }
        if parts.allow_bridge_tools {
            self = self.allow_bridge_tools();
        }
        if parts.allow_low_risk_workspace_patches {
            self = self.allow_low_risk_workspace_patches();
        }
        if let Some(runner) = parts.low_risk_process_runner {
            self = self.allow_low_risk_process_actions(runner);
        }
        if let Some(runner) = parts.read_only_shell_process_runner {
            self = self.allow_read_only_shell_process_actions(runner);
        }
        if let Some(accepted) = parts.accepted_local_workspace_process_runner {
            self = self.allow_accepted_local_workspace_process_actions(
                accepted.admission(),
                accepted.runner(),
            );
        }
        if let Some(trust_level) = parts.runtime_trust_level {
            self = self.runtime_trust_level(trust_level);
        }
        if let Some(mode) = parts.permission_review_mode {
            self = self.permission_review_mode(mode);
        }
        if let Some(source) = parts.permission_admission_source {
            self = self.permission_admission_source(source);
        }
        if let Some(factory) = parts.permissioned_process_runner_factory {
            self = self.permissioned_process_runner_factory(factory);
        }
        if let Some(catalog) = parts.skill_catalog {
            self = self.skill_catalog(catalog);
        }
        if let Some(project_rules) = parts.project_rules {
            self = self.project_rules(project_rules);
        }
        if let Some(task_anchor) = parts.task_anchor {
            self = self.task_anchor(task_anchor);
        }
        if let Some(manager) = parts.subagent_manager {
            self = self.subagent_manager(manager);
        }
        Ok(self)
    }

    /// Allows bridge tools to be registered for this runtime.
    ///
    /// Bridge handlers run in host code outside Merry-managed sandboxing. This
    /// opt-in is separate from [`RuntimeCapabilities`] policy.
    #[must_use]
    pub fn allow_bridge_tools(mut self) -> Self {
        self.allow_bridge_tools = true;
        self
    }

    /// Seeds deterministic runtime context without emitting observable events.
    ///
    /// This is for startup-owned facts such as a compact project capability
    /// summary. It is not a substitute for runtime artifacts produced during a
    /// step, and repeated ids are replaced before build-time validation.
    #[must_use]
    pub fn initial_context_summary(mut self, id: &str, text: &str) -> Self {
        self.initial_context_summaries
            .insert(id.to_owned(), text.to_owned());
        self
    }

    /// Adds explicit project rules to the cacheable stable request prefix.
    ///
    /// This is a construction-time projection for durable project instructions
    /// such as `AGENTS.md`. It does not scan the filesystem and is separate
    /// from context summaries, ledger facts, and artifact payloads.
    #[must_use]
    pub fn project_rules(mut self, project_rules: ProjectRules) -> Self {
        self.project_rules = Some(project_rules);
        self
    }

    /// Adds available skill metadata to the cacheable stable request prefix.
    ///
    /// This projects only `SKILL.md` frontmatter metadata. Full skill bodies
    /// remain on disk and must be read through registered workspace file tools.
    #[must_use]
    pub fn skill_catalog(mut self, skill_catalog: SkillCatalog) -> Self {
        self.skill_catalog = Some(skill_catalog);
        self
    }

    /// Sets the current task objective control-plane anchor.
    ///
    /// This reserves the runtime context slot for future `/task` commands. It
    /// is rendered as dynamic request context, not as project rules, ledger
    /// projection, or append-only chat history.
    #[must_use]
    pub fn task_anchor(mut self, task_anchor: TaskAnchor) -> Self {
        self.task_anchor = Some(task_anchor);
        self
    }

    /// Sets compacted checkpoint context for dynamic provider requests.
    ///
    /// This is selected by a checkpoint/compaction boundary. It does not
    /// project ordinary ledger facts, artifact payloads, or tool-result
    /// observations.
    #[must_use]
    pub fn compacted_checkpoint(mut self, checkpoint: CompactedCheckpoint) -> Self {
        self.compacted_checkpoint = Some(checkpoint);
        self
    }

    /// Opts in to executing validated low-risk workspace patch proposals.
    ///
    /// This keeps the default policy conservative: workspace writes remain
    /// denied unless the tool provides valid workspace patch proposal evidence
    /// and runtime construction explicitly enables this lane.
    #[must_use]
    pub fn allow_low_risk_workspace_patches(mut self) -> Self {
        self.allow_low_risk_workspace_patches = true;
        self
    }

    /// Opts in to executing validated low-risk process action proposals.
    ///
    /// The default policy remains deny. This lane is available only for command
    /// execution proposals with provider-neutral process evidence, an injected
    /// runtime runner, and the narrow SP2 low-risk predicate.
    #[must_use]
    pub fn allow_low_risk_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.low_risk_process_runner = Some(runner);
        self
    }

    /// Opts in to executing validated read-only shell wrapper proposals.
    ///
    /// This lane is intentionally separate from the structured low-risk argv
    /// lane. It accepts only a narrow `bash`/`sh`/`zsh -c|-lc` plain command
    /// sequence classifier and requires an injected runner selected for the
    /// shell read-only profile. It does not authorize arbitrary shell syntax.
    #[must_use]
    pub fn allow_read_only_shell_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.read_only_shell_process_runner = Some(runner);
        self
    }

    /// Opts in to executing validated local workspace effect process proposals.
    ///
    /// Runner injection alone is not a sandbox or an authorization source. This
    /// lane requires explicit runtime construction-time admission that declares
    /// the sandbox profile and accepted local workspace process risk for the
    /// narrow classified process intent.
    #[must_use]
    pub fn allow_accepted_local_workspace_process_actions(
        mut self,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        self.accepted_local_workspace_process_runner =
            Some(AcceptedLocalWorkspaceProcessRunner { admission, runner });
        self
    }

    /// Sets the trust level used by permission request admission defaults.
    ///
    /// Agentic/coding-agent runtimes require review by default. Trusted SDK
    /// hosts may explicitly choose host-only admission for business workflows
    /// where the host already owns the action policy.
    #[must_use]
    pub fn runtime_trust_level(mut self, trust_level: RuntimeTrustLevel) -> Self {
        self.runtime_trust_level = trust_level;
        self
    }

    /// Sets how permission requests are reviewed before execution.
    #[must_use]
    pub fn permission_review_mode(mut self, mode: PermissionReviewMode) -> Self {
        self.permission_review_mode = mode;
        self
    }

    /// Installs a host-owned permission admission source.
    ///
    /// This is used for trusted SDK hosts or tests. Agentic runtimes should
    /// normally use the model-backed review selected from
    /// [`RuntimeModelRole::ApprovalReview`] with primary fallback.
    #[must_use]
    pub fn permission_admission_source(
        mut self,
        source: Arc<dyn PermissionAdmissionSource>,
    ) -> Self {
        self.permission_admission_source = Some(source);
        self
    }

    /// Opts in to executing process actions approved by `request_permissions`.
    ///
    /// The runner should represent the backend/profile used for approved
    /// permission requests. Runtime admission approves only the exact planned
    /// action; it does not grant a reusable id back to the model.
    #[must_use]
    pub fn allow_permissioned_process_actions(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.permissioned_process_runner_factory = Some(Arc::new(
            StaticPermissionedProcessRunnerFactory::new(runner),
        ));
        self
    }

    /// Opts in to constructing a process runner per approved permission request.
    ///
    /// This is the preferred path for sandbox backends such as bubblewrap where
    /// approved capabilities, currently network, should be materialized only
    /// for the exact action being executed.
    #[must_use]
    pub fn permissioned_process_runner_factory(
        mut self,
        factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        self.permissioned_process_runner_factory = Some(factory);
        self
    }

    /// Installs a runtime-owned subagent manager for future subagent tools.
    #[must_use]
    pub fn subagent_manager(mut self, manager: SubagentManager) -> Self {
        self.subagent_manager = Some(manager);
        self
    }

    /// Builds the runtime.
    ///
    /// Duplicate tool names are rejected before the runtime is constructed.
    pub fn build(self) -> Result<Runtime, RuntimeError> {
        let tool_registry =
            ToolRegistry::from_registered(self.registered_tools).map_err(|duplicate| {
                RuntimeError::DuplicateToolRegistration {
                    name: duplicate.name,
                }
            })?;
        if !self.allow_bridge_tools
            && let Some(name) = tool_registry.first_bridge_tool_name()
        {
            return Err(RuntimeError::BridgeToolsNotAllowed { name: name.clone() });
        }

        let mut session = SessionState::new(self.session_id.clone());
        for (id, text) in self.initial_context_summaries {
            session.seed_context_summary(&id, &text)?;
        }
        if let Some(project_rules) = self.project_rules {
            session.set_project_rules(project_rules);
        }
        if let Some(skill_catalog) = self.skill_catalog {
            session.set_skill_catalog(skill_catalog);
        }
        if let Some(task_anchor) = self.task_anchor {
            session.set_task_anchor(task_anchor);
        }
        if let Some(checkpoint) = self.compacted_checkpoint {
            session.set_compacted_checkpoint(checkpoint);
        }

        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: self.session_id.clone(),
                session: Mutex::new(session),
                active_step: Arc::new(AtomicBool::new(false)),
                memory_projection_epoch: AtomicU64::new(0),
                event_buffer_size: self.event_buffer_size,
                model_configs: self.model_configs,
                automatic_compaction: self.automatic_compaction,
                capabilities: self.capabilities,
                progress_commentary: self.progress_commentary,
                tool_registry,
                memory_activation_source: self.memory_activation_source,
                allow_low_risk_workspace_patches: self.allow_low_risk_workspace_patches,
                low_risk_process_runner: self.low_risk_process_runner,
                read_only_shell_process_runner: self.read_only_shell_process_runner,
                accepted_local_workspace_process_runner: self
                    .accepted_local_workspace_process_runner,
                runtime_trust_level: self.runtime_trust_level,
                permission_review_mode: self.permission_review_mode,
                permission_admission_source: self.permission_admission_source,
                permissioned_process_runner_factory: self.permissioned_process_runner_factory,
                subagent_manager: self.subagent_manager,
            }),
        })
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

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_preflight_rejects_unreadable_evidence() {
        let runtime = Runtime::builder(session_id("uncertainty-preflight"))
            .build()
            .expect("runtime builds");
        {
            let mut session = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            session
                .record_tool_call_pending(pending_tool_call("review-preflight-call"))
                .expect("pending tool call records");
        }
        let before = judgment_harness_state(&runtime).await;
        let request = tool_risk_review_request(vec![judgment_evidence(
            "missing request evidence",
            "missing-review-source",
            EvidenceLocator::whole_artifact(),
        )]);
        let source = ScriptedJudgmentSource::with_outcome(high_tool_risk_outcome(Vec::new()));

        let error = runtime
            .run_uncertainty_review(&source, request, CancellationToken::new())
            .await
            .expect_err("missing request evidence rejects before source invocation");

        assert!(matches!(
            error,
            JudgmentError::UnreadableEvidence {
                artifact_id,
                source: ArtifactError::MissingArtifact { .. },
            } if artifact_id.as_str() == "missing-review-source"
        ));
        assert_eq!(source.call_count(), 0);
        assert_eq!(judgment_harness_state(&runtime).await, before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_records_one_internal_payload_and_no_public_state() {
        let runtime = Runtime::builder(session_id("uncertainty-success"))
            .build()
            .expect("runtime builds");
        record_memory_artifact(
            &runtime,
            "review-source",
            "lookup input may include credential-like material\n",
        );
        {
            let mut session = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            session
                .record_tool_call_pending(pending_tool_call("review-success-call"))
                .expect("pending tool call records");
        }
        let evidence = judgment_evidence(
            "lookup input",
            "review-source",
            EvidenceLocator::whole_artifact(),
        );
        let request = tool_risk_review_request(vec![evidence.clone()]);
        let source = ScriptedJudgmentSource::with_outcome(high_tool_risk_outcome(vec![evidence]));
        let public_before = {
            let mut state = judgment_harness_state(&runtime).await;
            state.judgment_records.clear();
            state
        };

        let record = runtime
            .run_uncertainty_review(&source, request, CancellationToken::new())
            .await
            .expect("valid uncertainty review records");

        assert_eq!(source.call_count(), 1);
        assert_eq!(record.id().as_str(), "judgment-record-00000000000000000000");
        assert_eq!(record.request().purpose(), JudgmentPurpose::ToolRiskReview);
        assert_eq!(record.outcome().purpose(), JudgmentPurpose::ToolRiskReview);
        assert_eq!(record.outcome().confidence().as_f32(), 0.95);
        assert_eq!(
            record.outcome().uncertainty(),
            "This advisory review cannot authorize or block tool execution."
        );
        assert_eq!(
            record.outcome().provenance().source_kind(),
            JudgmentSourceKind::Test
        );
        assert_eq!(
            record.outcome().provenance().source_label(),
            "runtime scripted source"
        );
        match record.outcome().recommendation() {
            JudgmentRecommendation::ToolRiskReview { risk, concerns } => {
                assert_eq!(*risk, JudgmentRiskLevel::High);
                assert_eq!(
                    concerns,
                    &["Input references credential-like material.".to_owned()]
                );
            }
            other => panic!("expected tool risk review recommendation, got {other:?}"),
        }
        assert!(
            record
                .artifacts()
                .request()
                .content()
                .contains("purpose=tool_risk_review\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("recommendation.risk=high\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("confidence=0.950000\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("provenance.kind=test\n")
        );

        let after = judgment_harness_state(&runtime).await;
        assert_eq!(after.judgment_records, vec![record]);
        let public_after = JudgmentHarnessState {
            judgment_records: Vec::new(),
            ..after
        };
        assert_eq!(public_after, public_before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_model_backed_source_records_llm_judgment_and_no_public_state() {
        let runtime = Runtime::builder(session_id("uncertainty-model-backed-success"))
            .build()
            .expect("runtime builds");
        record_memory_artifact(
            &runtime,
            "model-backed-review-source",
            "lookup input includes customer token material\n",
        );
        {
            let mut session = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            session
                .record_tool_call_pending(pending_tool_call("model-backed-review-call"))
                .expect("pending tool call records");
        }
        let evidence = judgment_evidence(
            "lookup input",
            "model-backed-review-source",
            EvidenceLocator::whole_artifact(),
        );
        let request = tool_risk_review_request(vec![evidence.clone()]);
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                vec![ModelOutput::text(
                    model_tool_risk_judgment_json(
                        "high",
                        "The lookup input may expose credential-like customer material.",
                        0,
                        "lookup input",
                        0.82,
                        "The cited input contains material that should be treated as sensitive before tool policy decides.",
                        "This model judgment is advisory only and cannot authorize or block tool execution.",
                    )
                    .as_str(),
                )],
                FinishReason::Stop,
            ))]),
        ]);
        let source = model_backed_judgment_source(provider.clone(), "runtime model-backed source");
        let public_before = {
            let mut state = judgment_harness_state(&runtime).await;
            state.judgment_records.clear();
            state
        };

        let record = runtime
            .run_uncertainty_review(&source, request, CancellationToken::new())
            .await
            .expect("valid model-backed uncertainty review records");

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        let after = judgment_harness_state(&runtime).await;
        assert_eq!(after.judgment_records, vec![record.clone()]);
        assert_eq!(after.judgment_records.len(), 1);
        assert_eq!(
            record.outcome().provenance().source_kind(),
            JudgmentSourceKind::Llm
        );
        assert_eq!(
            record.outcome().provenance().source_label(),
            "runtime model-backed source"
        );
        assert_eq!(record.outcome().evidence(), std::slice::from_ref(&evidence));
        assert_eq!(record.outcome().confidence().as_f32(), 0.82);
        assert_eq!(
            record.outcome().rationale(),
            "The cited input contains material that should be treated as sensitive before tool policy decides."
        );
        assert_eq!(
            record.outcome().uncertainty(),
            "This model judgment is advisory only and cannot authorize or block tool execution."
        );
        match record.outcome().recommendation() {
            JudgmentRecommendation::ToolRiskReview { risk, concerns } => {
                assert_eq!(*risk, JudgmentRiskLevel::High);
                assert_eq!(
                    concerns,
                    &["The lookup input may expose credential-like customer material.".to_owned()]
                );
            }
            other => panic!("expected tool risk review recommendation, got {other:?}"),
        }
        assert!(
            record
                .artifacts()
                .request()
                .content()
                .contains("purpose=tool_risk_review\n")
        );
        assert!(
            record
                .artifacts()
                .request()
                .content()
                .contains("evidence.0.label=lookup input\n")
        );
        assert!(
            record
                .artifacts()
                .request()
                .content()
                .contains("evidence.0.artifact_id=model-backed-review-source\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("recommendation.risk=high\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("recommendation.concerns.0=The lookup input may expose credential-like customer material.\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("confidence=0.820000\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("evidence.0.label=lookup input\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("evidence.0.artifact_id=model-backed-review-source\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("provenance.kind=llm\n")
        );
        assert!(
            record
                .artifacts()
                .outcome()
                .content()
                .contains("provenance.label=runtime model-backed source\n")
        );

        let public_after = JudgmentHarnessState {
            judgment_records: Vec::new(),
            ..after
        };
        assert_eq!(public_after, public_before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_model_backed_source_preflight_rejects_unreadable_evidence_before_provider_call()
     {
        let runtime = Runtime::builder(session_id("uncertainty-model-backed-preflight"))
            .build()
            .expect("runtime builds");
        {
            let mut session = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            session
                .record_tool_call_pending(pending_tool_call("model-backed-preflight-call"))
                .expect("pending tool call records");
        }
        let request = tool_risk_review_request(vec![judgment_evidence(
            "missing lookup input",
            "missing-model-backed-review-source",
            EvidenceLocator::whole_artifact(),
        )]);
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                vec![ModelOutput::text(
                    model_tool_risk_judgment_json(
                        "low",
                        "No model call should be made for unreadable evidence.",
                        0,
                        "missing lookup input",
                        0.2,
                        "Unreadable evidence should fail preflight before semantic judgment.",
                        "No uncertainty should be recorded because the source must not run.",
                    )
                    .as_str(),
                )],
                FinishReason::Stop,
            ))]),
        ]);
        let source = model_backed_judgment_source(provider.clone(), "runtime model-backed source");
        let before = judgment_harness_state(&runtime).await;

        let error = runtime
            .run_uncertainty_review(&source, request, CancellationToken::new())
            .await
            .expect_err("missing request evidence rejects before provider invocation");

        assert!(matches!(
            error,
            JudgmentError::UnreadableEvidence {
                artifact_id,
                source: ArtifactError::MissingArtifact { .. },
            } if artifact_id.as_str() == "missing-model-backed-review-source"
        ));
        assert!(provider.recorded_requests().is_empty());
        assert_eq!(judgment_harness_state(&runtime).await, before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_model_backed_source_invalid_model_output_records_nothing() {
        let runtime = Runtime::builder(session_id("uncertainty-model-backed-invalid-output"))
            .build()
            .expect("runtime builds");
        record_memory_artifact(
            &runtime,
            "model-backed-invalid-source",
            "lookup input is readable for invalid model output test\n",
        );
        {
            let mut session = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            session
                .record_tool_call_pending(pending_tool_call("model-backed-invalid-call"))
                .expect("pending tool call records");
        }
        let request = tool_risk_review_request(vec![judgment_evidence(
            "lookup input",
            "model-backed-invalid-source",
            EvidenceLocator::whole_artifact(),
        )]);
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                vec![ModelOutput::text("not strict judgment json")],
                FinishReason::Stop,
            ))]),
        ]);
        let source = model_backed_judgment_source(provider.clone(), "runtime model-backed source");
        let before = judgment_harness_state(&runtime).await;

        let error = runtime
            .run_uncertainty_review(&source, request, CancellationToken::new())
            .await
            .expect_err("invalid model output rejects before registry write");

        assert_eq!(error, JudgmentError::InvalidModelJudgmentOutput);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_eq!(judgment_harness_state(&runtime).await, before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_rejects_bad_outcome_evidence_without_registry_write() {
        let runtime = Runtime::builder(session_id("uncertainty-bad-outcome"))
            .build()
            .expect("runtime builds");
        record_memory_artifact(&runtime, "review-request-source", "request evidence\n");
        {
            let mut session = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            session
                .record_tool_call_pending(pending_tool_call("review-bad-outcome-call"))
                .expect("pending tool call records");
        }
        let request = tool_risk_review_request(vec![judgment_evidence(
            "request source",
            "review-request-source",
            EvidenceLocator::whole_artifact(),
        )]);
        let source =
            ScriptedJudgmentSource::with_outcome(high_tool_risk_outcome(vec![judgment_evidence(
                "missing outcome source",
                "missing-outcome-source",
                EvidenceLocator::whole_artifact(),
            )]));
        let before = judgment_harness_state(&runtime).await;

        let error = runtime
            .run_uncertainty_review(&source, request, CancellationToken::new())
            .await
            .expect_err("missing outcome evidence rejects before registry write");

        assert!(matches!(
            error,
            JudgmentError::UnreadableEvidence {
                artifact_id,
                source: ArtifactError::MissingArtifact { .. },
            } if artifact_id.as_str() == "missing-outcome-source"
        ));
        assert_eq!(source.call_count(), 1);
        assert_eq!(judgment_harness_state(&runtime).await, before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_pre_cancelled_token_skips_source_and_state_change() {
        let runtime = Runtime::builder(session_id("uncertainty-pre-cancelled"))
            .build()
            .expect("runtime builds");
        let source = ScriptedJudgmentSource::with_outcome(high_tool_risk_outcome(Vec::new()));
        let before = judgment_harness_state(&runtime).await;
        let token = CancellationToken::new();
        token.cancel();

        let error = runtime
            .run_uncertainty_review(&source, tool_risk_review_request(Vec::new()), token)
            .await
            .expect_err("pre-cancelled token rejects");

        assert_eq!(error, JudgmentError::Cancelled);
        assert_eq!(source.call_count(), 0);
        assert_eq!(judgment_harness_state(&runtime).await, before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_cancelled_while_source_future_in_flight_records_nothing() {
        let runtime = Runtime::builder(session_id("uncertainty-in-flight-cancel"))
            .build()
            .expect("runtime builds");
        {
            let mut session = runtime
                .inner
                .session
                .try_lock()
                .expect("session lock is free");
            session
                .record_tool_call_pending(pending_tool_call("review-in-flight-call"))
                .expect("pending tool call records");
        }
        let before = judgment_harness_state(&runtime).await;
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let source = ScriptedJudgmentSource::new(vec![
            ScriptedJudgmentResponse::PendingUntilReleasedOrCancelled {
                started: started_tx,
                release: release_rx,
                outcome: high_tool_risk_outcome(Vec::new()),
            },
        ]);
        let token = CancellationToken::new();
        let review = {
            let runtime = runtime.clone();
            let source = source.clone();
            let token = token.clone();
            tokio::spawn(async move {
                runtime
                    .run_uncertainty_review(&source, tool_risk_review_request(Vec::new()), token)
                    .await
            })
        };

        started_rx.await.expect("judgment source future starts");
        assert_eq!(source.call_count(), 1);

        token.cancel();
        let error = review
            .await
            .expect("review task should not panic")
            .expect_err("in-flight cancellation rejects");

        assert_eq!(error, JudgmentError::Cancelled);
        assert_eq!(judgment_harness_state(&runtime).await, before);
        drop(release_tx);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_source_error_or_cancel_records_nothing() {
        for (session, response) in [
            (
                "uncertainty-source-error",
                ScriptedJudgmentResponse::Error(JudgmentError::BlankField {
                    field: "scripted source failure",
                }),
            ),
            (
                "uncertainty-source-cancel",
                ScriptedJudgmentResponse::Cancelled,
            ),
        ] {
            let runtime = Runtime::builder(session_id(session))
                .build()
                .expect("runtime builds");
            {
                let mut state = runtime
                    .inner
                    .session
                    .try_lock()
                    .expect("session lock is free");
                state
                    .record_tool_call_pending(pending_tool_call(&format!("{session}-call")))
                    .expect("pending tool call records");
            }
            let before = judgment_harness_state(&runtime).await;
            let source = ScriptedJudgmentSource::new(vec![response]);

            let error = runtime
                .run_uncertainty_review(
                    &source,
                    tool_risk_review_request(Vec::new()),
                    CancellationToken::new(),
                )
                .await
                .expect_err("source failure rejects");

            assert!(matches!(
                error,
                JudgmentError::BlankField {
                    field: "scripted source failure",
                } | JudgmentError::Cancelled
            ));
            assert_eq!(source.call_count(), 1);
            assert_eq!(judgment_harness_state(&runtime).await, before);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn uncertainty_review_high_and_unknown_tool_risk_remain_non_authoritative() {
        for (session, outcome) in [
            ("uncertainty-high-risk", high_tool_risk_outcome(Vec::new())),
            (
                "uncertainty-unknown-risk",
                unknown_tool_risk_outcome(Vec::new()),
            ),
        ] {
            let runtime = Runtime::builder(session_id(session))
                .build()
                .expect("runtime builds");
            {
                let mut state = runtime
                    .inner
                    .session
                    .try_lock()
                    .expect("session lock is free");
                state
                    .record_tool_call_pending(pending_tool_call(&format!("{session}-call")))
                    .expect("pending tool call records");
            }
            let public_before = {
                let mut state = judgment_harness_state(&runtime).await;
                state.judgment_records.clear();
                state
            };
            let source = ScriptedJudgmentSource::with_outcome(outcome);

            let record = runtime
                .run_uncertainty_review(
                    &source,
                    tool_risk_review_request(Vec::new()),
                    CancellationToken::new(),
                )
                .await
                .expect("advisory tool risk review records");

            assert_eq!(source.call_count(), 1);
            assert!(matches!(
                record.outcome().recommendation(),
                JudgmentRecommendation::ToolRiskReview {
                    risk: JudgmentRiskLevel::High | JudgmentRiskLevel::Unknown,
                    ..
                }
            ));
            let after = judgment_harness_state(&runtime).await;
            assert_eq!(after.judgment_records.len(), 1);
            let public_after = JudgmentHarnessState {
                judgment_records: Vec::new(),
                ..after
            };
            assert_eq!(public_after, public_before);
        }
    }

    #[test]
    fn memory_activation_seed_uses_step_input_as_user_query_source() {
        let input = crate::StepInput::user_text("  Topic\trequest\n").expect("valid step input");

        let seed = memory_activation_seed_from_step_input(&input).expect("seed builds");

        assert_eq!(seed.query(), "topic request");
        assert_eq!(
            seed.provenance().source_kind(),
            MemoryActivationSourceKind::UserQuery
        );
        assert_eq!(seed.provenance().source_label(), "step input");
        assert_eq!(
            seed.provenance().allowed_scopes(),
            &[MemoryScope::Session, MemoryScope::Task, MemoryScope::Step]
        );
    }

    #[test]
    fn default_action_policy_matches_mvp_hard_policy() {
        let policy = DefaultActionPolicy;

        let read_only = policy.decide(ToolActionKind::ReadOnly);
        assert_eq!(read_only.action_kind(), ToolActionKind::ReadOnly);
        assert_eq!(read_only.risk_tier(), ActionRiskTier::ReadOnly);
        assert_eq!(read_only.disposition(), ActionPolicyDisposition::Allow);
        assert!(read_only.is_allowed());

        let runtime_control = policy.decide(ToolActionKind::RuntimeControl);
        assert_eq!(
            runtime_control.action_kind(),
            ToolActionKind::RuntimeControl
        );
        assert_eq!(runtime_control.risk_tier(), ActionRiskTier::RuntimeControl);
        assert_eq!(
            runtime_control.disposition(),
            ActionPolicyDisposition::Allow
        );
        assert!(runtime_control.is_allowed());

        for (action_kind, risk_tier) in [
            (ToolActionKind::WorkspaceWrite, ActionRiskTier::EditElevated),
            (ToolActionKind::CommandExec, ActionRiskTier::ProcessHigh),
            (ToolActionKind::Network, ActionRiskTier::Forbidden),
        ] {
            let decision = policy.decide(action_kind);
            assert_eq!(decision.action_kind(), action_kind);
            assert_eq!(decision.risk_tier(), risk_tier);
            assert_eq!(decision.disposition(), ActionPolicyDisposition::Deny);
            assert!(!decision.is_allowed());
        }
    }

    #[test]
    fn bridge_tool_registration_requires_explicit_builder_opt_in() {
        let result = Runtime::builder(session_id("runtime-bridge-tool-gate-deny"))
            .register_tool(RegisteredTool::bridge(policy_tool_spec("bridge_lookup")))
            .build();

        match result {
            Ok(_) => panic!("bridge tools require explicit builder opt-in"),
            Err(RuntimeError::BridgeToolsNotAllowed { name }) => {
                assert_eq!(name.as_str(), "bridge_lookup");
            }
            Err(other) => panic!("expected bridge gate error, got {other:?}"),
        }
    }

    #[test]
    fn allow_bridge_tools_accepts_bridge_registered_tool() {
        let runtime = Runtime::builder(session_id("runtime-bridge-tool-gate-allow"))
            .allow_bridge_tools()
            .register_tool(RegisteredTool::bridge(policy_tool_spec("bridge_lookup")))
            .build()
            .expect("explicit bridge opt-in should allow bridge tool registration");

        assert!(
            runtime
                .inner
                .tool_registry
                .registered_tool(&ToolName::new("bridge_lookup").expect("valid tool name"))
                .is_some()
        );
    }

    #[test]
    fn role_model_config_stores_all_roles_independently_and_overrides_same_role() {
        let first_primary_model = named_model("fake/primary-v1");
        let primary_model = named_model("fake/primary-v2");
        let first_tool_risk_model = named_model("fake/tool-risk-review-v1");
        let tool_risk_model = named_model("fake/tool-risk-review");
        let approval_model = named_model("fake/approval-review");
        let summary_model = named_model("fake/summary-memory");
        let compaction_model = named_model("fake/context-compaction");

        let runtime = Runtime::builder(session_id("runtime-role-model-config"))
            .model_provider(Arc::new(RecordingModelProvider::new()), first_primary_model)
            .model_provider(
                Arc::new(RecordingModelProvider::new()),
                primary_model.clone(),
            )
            .model_provider_for_role(
                RuntimeModelRole::ToolRiskReview,
                Arc::new(RecordingModelProvider::new()),
                first_tool_risk_model,
            )
            .model_provider_for_role(
                RuntimeModelRole::ApprovalReview,
                Arc::new(RecordingModelProvider::new()),
                approval_model.clone(),
            )
            .model_provider_for_role(
                RuntimeModelRole::SummaryMemory,
                Arc::new(RecordingModelProvider::new()),
                summary_model.clone(),
            )
            .model_provider_for_role(
                RuntimeModelRole::ContextCompaction,
                Arc::new(RecordingModelProvider::new()),
                compaction_model.clone(),
            )
            .model_provider_for_role(
                RuntimeModelRole::ToolRiskReview,
                Arc::new(RecordingModelProvider::new()),
                tool_risk_model.clone(),
            )
            .build()
            .expect("runtime should build");

        for (role, expected_model) in [
            (RuntimeModelRole::Primary, &primary_model),
            (RuntimeModelRole::ToolRiskReview, &tool_risk_model),
            (RuntimeModelRole::ApprovalReview, &approval_model),
            (RuntimeModelRole::SummaryMemory, &summary_model),
            (RuntimeModelRole::ContextCompaction, &compaction_model),
        ] {
            assert_eq!(
                runtime.inner.model_configs.model_for_role(role),
                Some(expected_model)
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn step_uses_primary_model_and_does_not_call_any_non_primary_role_provider() {
        let primary = RecordingModelProvider::new();
        let tool_risk_review = RecordingModelProvider::new();
        let approval_review = RecordingModelProvider::new();
        let summary_memory = RecordingModelProvider::new();
        let context_compaction = RecordingModelProvider::new();
        let runtime = Runtime::builder(session_id("runtime-step-primary-role-model"))
            .model_provider(Arc::new(primary.clone()), named_model("fake/primary-step"))
            .model_provider_for_role(
                RuntimeModelRole::ToolRiskReview,
                Arc::new(tool_risk_review.clone()),
                named_model("fake/tool-risk-review-step"),
            )
            .model_provider_for_role(
                RuntimeModelRole::ApprovalReview,
                Arc::new(approval_review.clone()),
                named_model("fake/approval-review-step"),
            )
            .model_provider_for_role(
                RuntimeModelRole::SummaryMemory,
                Arc::new(summary_memory.clone()),
                named_model("fake/summary-memory-step"),
            )
            .model_provider_for_role(
                RuntimeModelRole::ContextCompaction,
                Arc::new(context_compaction.clone()),
                named_model("fake/context-compaction-step"),
            )
            .build()
            .expect("runtime should build");

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            [
                "SessionStarted",
                "StepStarted",
                "ArtifactRecorded",
                "StepCompleted"
            ]
        );
        let primary_requests = primary.recorded_requests();
        assert_eq!(primary.calls.load(Ordering::SeqCst), 1);
        assert_eq!(primary_requests.len(), 1);
        assert_eq!(
            primary_requests[0].model(),
            &named_model("fake/primary-step")
        );
        for provider in [
            &tool_risk_review,
            &approval_review,
            &summary_memory,
            &context_compaction,
        ] {
            assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
            assert!(provider.recorded_requests().is_empty());
        }
    }

    async fn seed_two_history_items_for_compaction(runtime: &Runtime) {
        let events = collect_step(
            runtime,
            "old user message for compaction",
            crate::StepContext::default(),
        )
        .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, RuntimeEventKind::StepCompleted)),
            "seed step should complete"
        );
        let events = collect_step(
            runtime,
            "retained tail user message",
            crate::StepContext::default(),
        )
        .await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, RuntimeEventKind::StepCompleted)),
            "tail seed step should complete"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compaction_uses_context_compaction_role_when_configured() {
        let primary = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ]);
        let compactor = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                vec![ModelOutput::text(
                    r#"{
                      "claims": [
                        {
                          "id": "c1",
                          "kind": "completed_action",
                          "text": "Old history was compacted.",
                          "refs": ["r1", "r2"]
                        }
                      ],
                      "working_intent": null
                    }"#,
                )],
                FinishReason::Stop,
            ))]),
        ]);
        let runtime = Runtime::builder(session_id("compaction-role"))
            .model_provider(Arc::new(primary.clone()), model_name())
            .model_provider_for_role(
                RuntimeModelRole::ContextCompaction,
                Arc::new(compactor.clone()),
                ModelName::new("compaction-model").expect("valid model"),
            )
            .build()
            .expect("runtime builds");

        seed_two_history_items_for_compaction(&runtime).await;
        let primary_before = primary.recorded_requests().len();

        let outcome = runtime
            .compact_context_once(
                CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy"),
                StepContext::default(),
            )
            .await
            .expect("compaction succeeds")
            .expect("compaction happened");

        assert_eq!(outcome.covered_history_item_count(), 2);
        assert_eq!(primary.recorded_requests().len(), primary_before);
        assert_eq!(compactor.recorded_requests().len(), 1);
        assert_eq!(
            compactor.recorded_requests()[0].model().as_str(),
            "compaction-model"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compaction_accepts_streamed_text_delta_before_completed_response() {
        let primary = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event())]),
        ]);
        let candidate_json = r#"{
          "claims": [
            {
              "id": "c1",
              "kind": "completed_action",
              "text": "Old history was compacted.",
              "refs": ["r1", "r2"]
            }
          ],
          "working_intent": null
        }"#;
        let compactor =
            RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![
                Ok(ModelEvent::Started),
                Ok(ModelEvent::OutputTextDelta {
                    delta: candidate_json.to_owned(),
                }),
                Ok(completed_event_with(
                    vec![ModelOutput::text(candidate_json)],
                    FinishReason::Stop,
                )),
            ])]);
        let runtime = Runtime::builder(session_id("compaction-streamed-delta"))
            .model_provider(Arc::new(primary), model_name())
            .model_provider_for_role(
                RuntimeModelRole::ContextCompaction,
                Arc::new(compactor),
                ModelName::new("compaction-model").expect("valid model"),
            )
            .build()
            .expect("runtime builds");

        seed_two_history_items_for_compaction(&runtime).await;

        let outcome = runtime
            .compact_context_once(
                CitationCompactionPolicy::new(128, None, 4096, 2, 1200, 16).expect("valid policy"),
                StepContext::default(),
            )
            .await
            .expect("compaction accepts streamed text delta")
            .expect("compaction happened");

        assert_eq!(outcome.covered_history_item_count(), 2);
    }

    #[test]
    fn request_context_budget_uses_dynamic_estimate_watermarks() {
        let capabilities =
            ModelCapabilities::new(true, true, false, true, Some(100_000), Some(10_000))
                .expect("valid capabilities");
        let request = ModelRequest::new_with_continuations_and_stable_prefix(
            named_model("fake/budget-test"),
            vec![
                ModelMessage::new(
                    ModelMessageRole::System,
                    ModelContent::text("Base instructions.").expect("valid content"),
                )
                .expect("valid message"),
                ModelMessage::new(
                    ModelMessageRole::User,
                    ModelContent::text(&"a".repeat(260_000)).expect("valid content"),
                )
                .expect("valid message"),
            ],
            Vec::new(),
            Vec::new(),
            GenerationConfig::new(Some(10_000), false).expect("valid generation"),
            1,
        )
        .expect("valid request");

        let budget =
            request_context_budget(&capabilities, &request).expect("budget should calculate");

        assert_eq!(
            budget.window.source(),
            crate::ContextWindowSource::ProviderCapabilities
        );
        assert_eq!(budget.policy, ContextBudgetPolicy::Balanced);
        assert_eq!(budget.decision, CheckpointDecision::PlanCheckpoint);
        assert!(budget.dynamic_body_estimated_tokens >= budget.budget.soft_water_tokens());
        assert!(budget.dynamic_body_estimated_tokens < budget.budget.hard_water_tokens());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn default_stored_source_projects_session_memory_before_user_message() {
        let memory = memory_item(
            "memory-topic",
            "Remember that topic answers should mention runtime timing.",
            "memory-topic-artifact",
            &["topic"],
        );
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider("runtime-memory-context", provider.clone());
        record_memory_artifact(
            &runtime,
            "memory-topic-artifact",
            "exact evidence for timing memory",
        );
        record_memory_item(&runtime, memory);

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            [
                "SessionStarted",
                "StepStarted",
                "ArtifactRecorded",
                "StepCompleted"
            ]
        );

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages().len(), 3);
        assert_eq!(requests[0].stable_prefix_message_count(), 1);
        assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::System);
        assert!(
            requests[0].messages()[0]
                .content()
                .as_text()
                .contains("You are Merry, a pragmatic coding agent.")
        );
        assert_eq!(requests[0].messages()[1].role(), ModelMessageRole::System);
        assert_eq!(requests[0].messages()[2].role(), ModelMessageRole::User);
        assert!(
            requests[0].messages()[1]
                .content()
                .as_text()
                .contains("memory:memory-topic")
        );
        assert!(
            requests[0].messages()[1]
                .content()
                .as_text()
                .contains("memory-text:Remember that topic answers should mention runtime timing.")
        );
        assert_eq!(
            requests[0].messages()[2].content().as_text(),
            "Topic request."
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unmatched_stored_memory_does_not_add_system_message() {
        let memory = memory_item(
            "memory-other",
            "This memory should not match topic input.",
            "memory-other-artifact",
            &["other"],
        );
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider("runtime-memory-no-match", provider.clone());
        record_memory_artifact(
            &runtime,
            "memory-other-artifact",
            "exact evidence for unmatched memory",
        );
        record_memory_item(&runtime, memory);

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            [
                "SessionStarted",
                "StepStarted",
                "ArtifactRecorded",
                "StepCompleted"
            ]
        );
        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages().len(), 2);
        assert_eq!(requests[0].stable_prefix_message_count(), 1);
        assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::System);
        assert!(
            requests[0].messages()[0]
                .content()
                .as_text()
                .contains("You are Merry, a pragmatic coding agent.")
        );
        assert_eq!(requests[0].messages()[1].role(), ModelMessageRole::User);
        assert_eq!(
            requests[0].messages()[1].content().as_text(),
            "Topic request."
        );
        assert_eq!(compiled_context_snapshot(&runtime).await, "");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stored_memory_with_missing_evidence_fails_before_provider_call() {
        let memory = memory_item(
            "memory-missing-evidence",
            "This memory has no readable evidence artifact.",
            "memory-missing-evidence-artifact",
            &["topic"],
        );
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider("runtime-memory-missing-evidence", provider.clone());
        record_memory_item(&runtime, memory);

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Failed"]
        );
        assert_eq!(failed_code(&events), Some("context_compile"));
        assert_eq!(provider.recorded_requests().len(), 0);
        assert_eq!(
            crate::ContextCompiler::new()
                .compile(&runtime.context_snapshot().await)
                .expect("context compiles after missing evidence cleanup")
                .to_snapshot(),
            ""
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_step_replaces_activated_memories_between_requests() {
        let first_memory = activated_memory(
            "memory-stale",
            "Stale memory must not survive the next projection.",
            "memory-stale-artifact",
        );
        let source = ScriptedMemoryActivationSource::new(vec![vec![first_memory], Vec::new()]);
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-replace",
            provider.clone(),
            source.clone(),
        );
        record_memory_artifact(
            &runtime,
            "memory-stale-artifact",
            "exact evidence for stale memory",
        );

        let first_events = collect_step(
            &runtime,
            "First topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;
        let second_events = collect_step(
            &runtime,
            "Second topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&first_events),
            [
                "SessionStarted",
                "StepStarted",
                "ArtifactRecorded",
                "StepCompleted"
            ]
        );
        assert_eq!(
            event_kind_names(&second_events),
            ["StepStarted", "ArtifactRecorded", "StepCompleted"]
        );
        assert_eq!(source.call_count(), 2);

        let requests = provider.recorded_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].messages().len(), 3);
        assert_eq!(requests[0].stable_prefix_message_count(), 1);
        assert!(
            requests[0].messages()[1]
                .content()
                .as_text()
                .contains("memory:memory-stale")
        );
        assert_eq!(requests[1].messages().len(), 4);
        assert_eq!(requests[1].stable_prefix_message_count(), 1);
        assert_eq!(requests[1].messages()[0].role(), ModelMessageRole::System);
        assert!(
            requests[1].messages()[0]
                .content()
                .as_text()
                .contains("You are Merry, a pragmatic coding agent.")
        );
        assert_eq!(requests[1].messages()[1].role(), ModelMessageRole::User);
        assert_eq!(
            requests[1].messages()[1].content().as_text(),
            "First topic request."
        );
        assert_eq!(
            requests[1].messages()[2].role(),
            ModelMessageRole::Assistant
        );
        assert_eq!(
            requests[1].messages()[2].content().as_text(),
            "model result"
        );
        assert_eq!(requests[1].messages()[3].role(), ModelMessageRole::User);
        assert_eq!(
            requests[1].messages()[3].content().as_text(),
            "Second topic request."
        );
        assert!(
            requests[1]
                .messages()
                .iter()
                .all(|message| !message.content().as_text().contains("memory-stale"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn activation_source_error_clears_previous_successful_projection() {
        let memory = activated_memory(
            "memory-success",
            "Previous successful memory must not survive activation failure.",
            "memory-success-artifact",
        );
        let source = ScriptedMemoryActivationSource::with_script(vec![
            ScriptedMemoryActivationResponse::Memories(vec![memory]),
            ScriptedMemoryActivationResponse::Error(MemoryError::BlankField {
                field: "memory activation source label",
            }),
        ]);
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-source-error-clears",
            provider.clone(),
            source.clone(),
        );
        record_memory_artifact(
            &runtime,
            "memory-success-artifact",
            "exact evidence for successful memory",
        );

        let first_events = collect_step(
            &runtime,
            "First topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;
        let second_events = collect_step(
            &runtime,
            "Second topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&first_events),
            [
                "SessionStarted",
                "StepStarted",
                "ArtifactRecorded",
                "StepCompleted"
            ]
        );
        assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
        assert_eq!(failed_code(&second_events), Some("memory_activation"));
        assert_eq!(source.call_count(), 2);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_eq!(
            crate::ContextCompiler::new()
                .compile(&runtime.context_snapshot().await)
                .expect("context compiles after activation source failure")
                .to_snapshot(),
            ""
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unresolved_pending_tool_call_blocks_memory_activation() {
        let source = ScriptedMemoryActivationSource::new(vec![vec![activated_memory(
            "memory-blocked",
            "This memory must not activate while a tool call is pending.",
            "memory-blocked-artifact",
        )]]);
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-pending-gate",
            provider.clone(),
            source.clone(),
        );
        {
            let mut session = runtime.inner.session.lock().await;
            session
                .record_tool_call_pending(pending_tool_call("pending-call"))
                .expect("pending call records");
        }

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Failed"]
        );
        assert_eq!(
            failed_code(&events),
            Some(DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED)
        );
        assert_eq!(source.call_count(), 0);
        assert_eq!(provider.recorded_requests().len(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_cancelled_provider_step_does_not_activate_memory() {
        let source = ScriptedMemoryActivationSource::new(vec![vec![activated_memory(
            "memory-cancelled",
            "This memory must not activate for a pre-cancelled step.",
            "memory-cancelled-artifact",
        )]]);
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-pre-cancelled",
            provider.clone(),
            source.clone(),
        );
        let token = CancellationToken::new();
        token.cancel();

        let events = collect_step(&runtime, "Topic request.", crate::StepContext::new(token)).await;

        assert_eq!(event_kind_names(&events), ["Cancelled"]);
        assert_eq!(source.call_count(), 0);
        assert_eq!(provider.recorded_requests().len(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_absent_step_does_not_activate_memory() {
        let source = ScriptedMemoryActivationSource::new(vec![vec![activated_memory(
            "memory-no-provider",
            "This memory must not activate without a provider.",
            "memory-no-provider-artifact",
        )]]);
        let runtime = runtime_without_provider_with_memory_source(
            "runtime-memory-no-provider",
            source.clone(),
        );

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "StepCompleted"]
        );
        assert_eq!(source.call_count(), 0);
        assert_eq!(
            crate::ContextCompiler::new()
                .compile(&runtime.context_snapshot().await)
                .expect("empty context compiles")
                .to_snapshot(),
            ""
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unreadable_memory_evidence_from_activation_fails_before_provider_call() {
        let source = ScriptedMemoryActivationSource::new(vec![vec![
            activated_memory_with_unreadable_evidence("memory-unreadable"),
        ]]);
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-context-compile-failure",
            provider.clone(),
            source.clone(),
        );

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Failed"]
        );
        assert_eq!(failed_code(&events), Some("context_compile"));
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 0);
        assert_eq!(
            crate::ContextCompiler::new()
                .compile(&runtime.context_snapshot().await)
                .expect("context compiles after bad projection cleanup")
                .to_snapshot(),
            ""
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_pending_memory_activation_emits_cancelled_without_provider_call() {
        let (source, activation_started_rx, activation_dropped_rx) =
            pending_memory_activation_source();
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-pending-activation-cancel",
            provider.clone(),
            source.clone(),
        );
        let token = CancellationToken::new();
        let mut stream = runtime
            .step(
                crate::StepInput::user_text("Topic request.").expect("valid step input"),
                crate::StepContext::new(token.clone()),
            )
            .expect("step should start");

        assert!(matches!(
            stream.next().await.expect("session started event"),
            RuntimeEvent {
                kind: RuntimeEventKind::SessionStarted,
                ..
            }
        ));
        assert!(matches!(
            stream.next().await.expect("step started event"),
            RuntimeEvent {
                kind: RuntimeEventKind::StepStarted,
                ..
            }
        ));
        activation_started_rx
            .await
            .expect("activation future should start");

        token.cancel();
        activation_dropped_rx
            .await
            .expect("activation future should be dropped on cancellation");
        let remaining: Vec<_> = stream.collect().await;

        assert_eq!(event_kind_names(&remaining), ["Cancelled"]);
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 0);
        assert_activated_memory_projection_cleared(&runtime).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_stream_while_memory_activation_pending_drops_activation_without_provider_call()
     {
        let (source, activation_started_rx, activation_dropped_rx) =
            pending_memory_activation_source();
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-pending-activation-drop",
            provider.clone(),
            source.clone(),
        );
        let mut stream = runtime
            .step(
                crate::StepInput::user_text("Topic request.").expect("valid step input"),
                crate::StepContext::new(CancellationToken::new()),
            )
            .expect("step should start");

        assert!(matches!(
            stream.next().await.expect("session started event"),
            RuntimeEvent {
                kind: RuntimeEventKind::SessionStarted,
                ..
            }
        ));
        assert!(matches!(
            stream.next().await.expect("step started event"),
            RuntimeEvent {
                kind: RuntimeEventKind::StepStarted,
                ..
            }
        ));
        activation_started_rx
            .await
            .expect("activation future should start");

        drop(stream);
        activation_dropped_rx
            .await
            .expect("activation future should be dropped when stream is dropped");
        tokio::task::yield_now().await;

        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 0);
        assert_activated_memory_projection_cleared(&runtime).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn model_retry_events_are_emitted_and_failed_attempt_output_is_not_recorded() {
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![
                Ok(ModelEvent::Started),
                Ok(ModelEvent::OutputTextDelta {
                    delta: "partial attempt".to_owned(),
                }),
                Err(ModelError::provider(
                    ProviderErrorKind::Unavailable,
                    "stream interrupted",
                )),
            ]),
            ScriptedModelProviderResponse::Stream(vec![
                Ok(ModelEvent::Started),
                Ok(ModelEvent::OutputTextDelta {
                    delta: "successful attempt".to_owned(),
                }),
                Ok(completed_event_with(
                    vec![ModelOutput::text("successful attempt")],
                    FinishReason::Stop,
                )),
            ]),
        ]);
        let runtime = Runtime::builder(session_id("runtime-model-retry-events"))
            .model_retry_policy(
                ModelRetryPolicy::new(
                    true,
                    3,
                    std::time::Duration::from_millis(1),
                    std::time::Duration::from_millis(1),
                    std::time::Duration::from_millis(100),
                    false,
                )
                .expect("valid retry policy"),
            )
            .model_provider(Arc::new(provider.clone()), model_name())
            .build()
            .expect("runtime should build");

        let events = collect_step(
            &runtime,
            "Retry provider stream.",
            crate::StepContext::default(),
        )
        .await;

        assert_eq!(provider.recorded_requests().len(), 2);
        assert_eq!(
            event_kind_names(&events),
            [
                "SessionStarted",
                "StepStarted",
                "ModelRetryAttemptStarted",
                "ModelRetryScheduled",
                "ModelRetryAttemptStarted",
                "ArtifactRecorded",
                "StepCompleted",
            ]
        );
        let artifact_id = events
            .iter()
            .find_map(|event| match &event.kind {
                RuntimeEventKind::ArtifactRecorded { artifact } => Some(artifact.id().clone()),
                _ => None,
            })
            .expect("assistant output artifact should be recorded");
        let content = runtime
            .read_artifact_content(&artifact_id)
            .await
            .expect("artifact should be readable");
        assert_eq!(content.as_text(), Some("successful attempt"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_after_activation_before_provider_request_clears_projection() {
        let memory = activated_memory(
            "memory-cancelled-after-activation",
            "Activated memory must not survive cancellation before provider setup.",
            "memory-cancelled-after-activation-artifact",
        );
        let token = CancellationToken::new();
        let source = ScriptedMemoryActivationSource::with_script(vec![
            ScriptedMemoryActivationResponse::CancelThenMemories {
                token: token.clone(),
                memories: vec![memory],
            },
        ]);
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-activation-cancel-clears",
            provider.clone(),
            source.clone(),
        );
        record_memory_artifact(
            &runtime,
            "memory-cancelled-after-activation-artifact",
            "exact evidence for activation cancellation",
        );

        let events = collect_step(&runtime, "Topic request.", crate::StepContext::new(token)).await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Cancelled"]
        );
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 0);
        assert_eq!(
            crate::ContextCompiler::new()
                .compile(&runtime.context_snapshot().await)
                .expect("context compiles after cancellation cleanup")
                .to_snapshot(),
            ""
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_step_during_provider_setup_clears_activated_memory_projection() {
        let (provider_started_tx, provider_started_rx) = oneshot::channel();
        let provider =
            RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::PendingSetup(
                provider_started_tx,
            )]);
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-setup-drop-clears",
            provider.clone(),
            "memory-provider-setup-drop",
            "Activated memory must not survive dropped setup before stream commit.",
            "memory-provider-setup-drop-artifact",
        );

        let stream = runtime
            .step(
                crate::StepInput::user_text("Topic request.").expect("valid step input"),
                crate::StepContext::new(CancellationToken::new()),
            )
            .expect("step should start");
        provider_started_rx
            .await
            .expect("provider setup future should start");

        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-setup-drop",
            "Activated memory must not survive dropped setup before stream commit.",
        )
        .await;

        drop(stream);
        tokio::task::yield_now().await;

        assert_activated_memory_projection_cleared(&runtime).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_step_during_provider_setup_with_held_session_lock_defers_projection_cleanup()
    {
        let (provider_started_tx, provider_started_rx) = oneshot::channel();
        let (provider_dropped_tx, provider_dropped_rx) = oneshot::channel();
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::PendingSetupWithDrop {
                started: provider_started_tx,
                dropped: provider_dropped_tx,
            },
        ]);
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-setup-drop-spawned-cleanup",
            provider.clone(),
            "memory-provider-setup-drop-spawned",
            "Activated memory is cleared by spawned cleanup when drop cannot lock session.",
            "memory-provider-setup-drop-spawned-artifact",
        );

        let stream = runtime
            .step(
                crate::StepInput::user_text("Topic request.").expect("valid step input"),
                crate::StepContext::new(CancellationToken::new()),
            )
            .expect("step should start");
        provider_started_rx
            .await
            .expect("provider setup future should start");

        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-setup-drop-spawned",
            "Activated memory is cleared by spawned cleanup when drop cannot lock session.",
        )
        .await;

        let session = runtime.inner.session.lock().await;
        drop(stream);
        provider_dropped_rx
            .await
            .expect("provider setup future should be aborted");
        tokio::task::yield_now().await;

        let snapshot = crate::ContextCompiler::new()
            .compile(&session.context_snapshot())
            .expect("context compiles while cleanup waits for session lock")
            .to_snapshot();
        assert!(
            snapshot.contains("memory:memory-provider-setup-drop-spawned"),
            "projection should remain while spawned cleanup is waiting for session lock; snapshot:\n{snapshot}"
        );

        drop(session);
        tokio::task::yield_now().await;

        assert_activated_memory_projection_cleared(&runtime).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_setup_error_before_stream_clears_activated_memory_projection() {
        let provider =
            RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::SetupError(
                ModelError::provider(ProviderErrorKind::Unavailable, "provider setup failed"),
            )]);
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-setup-error-clears",
            provider.clone(),
            "memory-provider-setup-error",
            "Activated memory must not survive provider setup failure.",
            "memory-provider-setup-error-artifact",
        );

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Failed"]
        );
        assert_eq!(failed_code(&events), Some("model_unavailable"));
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_cleared(&runtime).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_stream_error_after_stream_start_retains_activated_memory_projection() {
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Err(ModelError::provider(
                ProviderErrorKind::Unavailable,
                "provider stream failed",
            ))]),
        ]);
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-stream-error-retains",
            provider.clone(),
            "memory-provider-stream-error",
            "Activated memory must survive provider stream failure after setup.",
            "memory-provider-stream-error-artifact",
        );

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Failed"]
        );
        assert_eq!(failed_code(&events), Some("model_unavailable"));
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-stream-error",
            "Activated memory must survive provider stream failure after setup.",
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_stream_cancelled_error_retains_activated_memory_projection() {
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Err(ModelError::Cancelled)]),
        ]);
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-stream-cancelled-error-retains",
            provider.clone(),
            "memory-provider-stream-cancelled-error",
            "Activated memory must survive stream cancellation after setup.",
            "memory-provider-stream-cancelled-error-artifact",
        );

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Cancelled"]
        );
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-stream-cancelled-error",
            "Activated memory must survive stream cancellation after setup.",
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_cancelled_finish_retains_activated_memory_projection() {
        let provider =
            RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![
                Ok(completed_event_with(Vec::new(), FinishReason::Cancelled)),
            ])]);
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-cancelled-finish-retains",
            provider.clone(),
            "memory-provider-cancelled-finish",
            "Activated memory must survive cancelled finish after setup.",
            "memory-provider-cancelled-finish-artifact",
        );

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Cancelled"]
        );
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-cancelled-finish",
            "Activated memory must survive cancelled finish after setup.",
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_completed_with_error_finish_retains_activated_memory_projection() {
        let provider =
            RecordingModelProvider::with_script(vec![ScriptedModelProviderResponse::Stream(vec![
                Ok(completed_event_with(Vec::new(), FinishReason::Error)),
            ])]);
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-finish-error-retains",
            provider.clone(),
            "memory-provider-finish-error",
            "Activated memory must survive provider error finish after setup.",
            "memory-provider-finish-error-artifact",
        );

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            ["SessionStarted", "StepStarted", "Failed"]
        );
        assert_eq!(failed_code(&events), Some("model_finish_error"));
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-finish-error",
            "Activated memory must survive provider error finish after setup.",
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_tool_call_pending_retains_activated_memory_projection_and_pending_gate_does_not_clear_it()
     {
        let call = model_tool_call("call-tool-pending");
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
            ))]),
        ]);
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-tool-call-retains",
            provider.clone(),
            "memory-provider-tool-call",
            "Activated memory must survive a pending tool call and pending gate.",
            "memory-provider-tool-call-artifact",
        );

        let first_events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&first_events),
            ["SessionStarted", "StepStarted", "ToolCallPending"]
        );
        assert_eq!(
            runtime.pending_tool_calls().await,
            vec![pending_tool_call("call-tool-pending")]
        );
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-tool-call",
            "Activated memory must survive a pending tool call and pending gate.",
        )
        .await;

        let second_events = collect_step(
            &runtime,
            "Second topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(event_kind_names(&second_events), ["StepStarted", "Failed"]);
        assert_eq!(
            failed_code(&second_events),
            Some(DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED)
        );
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-tool-call",
            "Activated memory must survive a pending tool call and pending gate.",
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridge_tool_call_emits_bridge_request_event() {
        let call = model_tool_call("call-bridge-tool");
        let provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(completed_event_with(
                vec![ModelOutput::tool_call(call)],
                FinishReason::ToolCalls,
            ))]),
        ]);
        let runtime = Runtime::builder(session_id("runtime-bridge-tool-event"))
            .allow_bridge_tools()
            .model_provider(Arc::new(provider.clone()), model_name())
            .register_tool(RegisteredTool::bridge(policy_tool_spec("lookup")))
            .build()
            .expect("bridge opt-in should allow bridge tool registration");

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            [
                "SessionStarted",
                "StepStarted",
                "ToolCallPending",
                "BridgeToolCallRequested"
            ]
        );
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            RuntimeEventKind::BridgeToolCallRequested { call }
                if call.id().as_str() == "call-bridge-tool"
                    && call.name().as_str() == "lookup"
        )));
        assert_eq!(
            runtime.pending_tool_calls().await,
            vec![pending_tool_call("call-bridge-tool")]
        );
        assert_eq!(provider.recorded_requests().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_stop_completion_retains_activated_memory_projection() {
        let provider = RecordingModelProvider::new();
        let (runtime, source) = runtime_with_provider_and_single_memory(
            "runtime-memory-provider-stop-retains",
            provider.clone(),
            "memory-provider-stop",
            "Activated memory must survive provider stop completion after setup.",
            "memory-provider-stop-artifact",
        );

        let events = collect_step(
            &runtime,
            "Topic request.",
            crate::StepContext::new(CancellationToken::new()),
        )
        .await;

        assert_eq!(
            event_kind_names(&events),
            [
                "SessionStarted",
                "StepStarted",
                "ArtifactRecorded",
                "StepCompleted"
            ]
        );
        assert_eq!(source.call_count(), 1);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert_activated_memory_projection_retained(
            &runtime,
            "memory-provider-stop",
            "Activated memory must survive provider stop completion after setup.",
        )
        .await;
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

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_registered_tool_executes_under_default_policy() {
        let executor = SuccessfulToolExecutor::new();
        let (runtime, pending) = register_policy_pending_tool(
            "runtime-policy-read-only",
            "policy_read",
            "call-read-only",
            ToolActionKind::ReadOnly,
            executor.clone(),
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("read-only tool execution should be allowed");

        assert_eq!(executor.call_count(), 1);
        assert_eq!(
            events
                .iter()
                .map(|event| match event.kind {
                    RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
                    RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
                    _ => "Other",
                })
                .collect::<Vec<_>>(),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
        assert!(runtime.pending_tool_calls().await.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_control_tool_resolves_even_if_token_is_cancelled_during_execution() {
        let executor = CancelDuringRuntimeControlExecutor::new();
        let (runtime, pending) = register_policy_pending_tool(
            "runtime-policy-runtime-control-cancel-race",
            "policy_control",
            "call-runtime-control",
            ToolActionKind::RuntimeControl,
            executor.clone(),
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("runtime control outcome should survive in-flight cancellation");

        assert_eq!(executor.call_count(), 1);
        assert!(executor.token_seen().is_cancelled());
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
        assert!(runtime.pending_tool_calls().await.is_empty());
    }

    #[test]
    fn generic_executor_admission_allows_read_only_and_rejects_mutating_actions() {
        let session_id = SessionId::new("generic-executor-admission").expect("valid session id");
        let pending = policy_pending_tool_call("call-admission", WORKSPACE_PATCH_TOOL_NAME);

        let read_only_decision = DefaultActionPolicy.decide(ToolActionKind::ReadOnly);
        admit_action_to_generic_executor(
            &pending,
            ToolActionKind::ReadOnly,
            &read_only_decision,
            None,
            &session_id,
        )
        .expect("read-only action may enter generic executor");

        for action_kind in [
            ToolActionKind::WorkspaceWrite,
            ToolActionKind::CommandExec,
            ToolActionKind::Network,
        ] {
            let decision = DefaultActionPolicy.decide(action_kind);
            let err = admit_action_to_generic_executor(
                &pending,
                action_kind,
                &decision,
                None,
                &session_id,
            )
            .expect_err("mutating action must require commit lifecycle");
            assert!(matches!(
                err,
                crate::RuntimeError::MutatingActionCommitLifecycleRequired {
                    session_id: ref guarded_session,
                    call_id: ref guarded_call,
                    action_kind: guarded_kind,
                } if guarded_session == &session_id
                    && guarded_call == pending.id()
                    && guarded_kind == action_kind
            ));
            assert!(
                err.to_string()
                    .contains("requires an explicit commit lifecycle")
            );
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
        let proposal = ActionProposal::new(
            &pending,
            ToolActionKind::WorkspaceWrite,
            "workspace patch",
            "notes/proposed.txt",
            "Replace one matched preimage in notes/proposed.txt",
            ActionProposalEvidence::WorkspacePatch(patch),
        )
        .expect("test action proposal is valid");
        let allowed_decision = ActionPolicyDecision::allow_low_risk_workspace_patch();
        admit_action_to_generic_executor(
            &pending,
            ToolActionKind::WorkspaceWrite,
            &allowed_decision,
            Some(&proposal),
            &session_id,
        )
        .expect("low-risk workspace patch proposal may enter generic executor");

        let non_patch_pending =
            policy_pending_tool_call("call-admission-other", "policy_admission");
        let err = admit_action_to_generic_executor(
            &non_patch_pending,
            ToolActionKind::WorkspaceWrite,
            &allowed_decision,
            Some(&proposal),
            &session_id,
        )
        .expect_err("only workspace_patch may enter the low-risk patch lane");
        assert!(matches!(
            err,
            crate::RuntimeError::MutatingActionCommitLifecycleRequired {
                action_kind: ToolActionKind::WorkspaceWrite,
                ..
            }
        ));

        for action_kind in [ToolActionKind::CommandExec, ToolActionKind::Network] {
            let err = admit_action_to_generic_executor(
                &pending,
                action_kind,
                &allowed_decision,
                Some(&proposal),
                &session_id,
            )
            .expect_err("only workspace patch proposals may enter generic executor");
            assert!(matches!(
                err,
                crate::RuntimeError::MutatingActionCommitLifecycleRequired {
                    action_kind: guarded_kind,
                    ..
                } if guarded_kind == action_kind
            ));
        }

        let elevated_decision = DefaultActionPolicy
            .decide(ToolActionKind::WorkspaceWrite)
            .with_risk_tier(ActionRiskTier::EditElevated);
        let err = admit_action_to_generic_executor(
            &pending,
            ToolActionKind::WorkspaceWrite,
            &elevated_decision,
            Some(&proposal),
            &session_id,
        )
        .expect_err("workspace write requires low-risk allow decision");
        assert!(matches!(
            err,
            crate::RuntimeError::MutatingActionCommitLifecycleRequired {
                action_kind: ToolActionKind::WorkspaceWrite,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_write_tool_is_denied_before_executor_and_records_sanitized_failure_artifact()
    {
        let executor = SuccessfulToolExecutor::new();
        let (runtime, pending) = register_policy_pending_tool(
            "runtime-policy-workspace-write",
            "policy_write",
            "call-workspace-write",
            ToolActionKind::WorkspaceWrite,
            executor.clone(),
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("policy denial should durably resolve the pending call");

        assert_eq!(executor.call_count(), 0);
        assert_eq!(
            events
                .iter()
                .map(|event| match event.kind {
                    RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
                    RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
                    _ => "Other",
                })
                .collect::<Vec<_>>(),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        let result = resolved_tool_result(&events);
        assert!(matches!(
            &events[0].kind,
            RuntimeEventKind::ArtifactRecorded { artifact } if artifact == result.artifact()
        ));
        assert!(matches!(
            &events[1].kind,
            RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == result
        ));
        assert_eq!(result.status(), merry_core::ToolCallResultStatus::Failed);
        assert_eq!(
            result
                .diagnostic()
                .expect("policy denial should include diagnostic")
                .code(),
            DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED
        );
        assert!(runtime.pending_tool_calls().await.is_empty());

        let content = denied_action_content(&runtime, &events).await;
        assert_sanitized_policy_denial_content(&content, "policy_write");

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 1);
        let audit = &audits[0];
        assert_eq!(audit.id().as_str(), "action-audit-00000000000000000000");
        assert_eq!(audit.order(), 0);
        assert_eq!(audit.tool_call_id(), pending.id());
        assert_eq!(audit.tool_name(), pending.name());
        assert_eq!(audit.action_kind(), ToolActionKind::WorkspaceWrite);
        assert_eq!(audit.status(), ActionAuditStatus::Denied);
        let policy = audit.policy().expect("denied audit should include policy");
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
        assert_eq!(policy.risk_tier(), ActionRiskTier::EditElevated);
        assert_eq!(
            policy.reason(),
            "workspace write tool actions are denied by default policy"
        );

        let projection = runtime.ledger_projection().await;
        let lifecycle = lifecycle_kinds(&projection);
        assert_lifecycle_order(
            &lifecycle,
            LedgerFactKind::ActionAuditRecorded,
            LedgerFactKind::ArtifactRecorded,
        );
        assert_lifecycle_order(
            &lifecycle,
            LedgerFactKind::ActionAuditRecorded,
            LedgerFactKind::ToolCallResolved,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_write_tool_with_proposal_records_proposed_before_denied_and_resolution() {
        let executor = ProposingToolExecutor::immediate();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_write_proposed"),
            Arc::new(executor.clone()),
            ToolActionKind::WorkspaceWrite,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool(
            "runtime-policy-proposed-workspace-write",
            "policy_write_proposed",
            "call-workspace-write-proposed",
            tool,
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("policy denial should durably resolve proposed action");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_sanitized_policy_denial_content(
            &denied_action_content(&runtime, &events).await,
            "policy_write_proposed",
        );

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[0].tool_call_id(), pending.id());
        assert_eq!(audits[0].tool_name(), pending.name());
        assert_eq!(audits[0].action_kind(), ToolActionKind::WorkspaceWrite);
        assert!(audits[0].policy().is_none());
        let proposal = audits[0]
            .proposal()
            .expect("proposed audit should include proposal evidence");
        assert_eq!(proposal.tool_call_id(), pending.id());
        assert_eq!(proposal.tool_name(), pending.name());
        assert_eq!(proposal.action_kind(), ToolActionKind::WorkspaceWrite);
        assert_eq!(proposal.label(), "workspace patch");
        assert_eq!(proposal.subject(), "notes/proposed.txt");
        assert!(proposal.summary().contains("notes/proposed.txt"));
        let ActionProposalEvidence::WorkspacePatch(patch) = proposal.evidence() else {
            panic!("workspace write proposal should record workspace patch evidence");
        };
        assert_eq!(patch.relative_path(), "notes/proposed.txt");
        assert_eq!(patch.preimage_bytes(), 3);
        assert_eq!(patch.replacement_bytes(), 7);
        assert_eq!(patch.file_bytes_before(), 20);
        assert_eq!(patch.file_bytes_after(), 24);
        assert_eq!(patch.file_fingerprint_before(), "fnv1a64:0000000000000001");
        assert_eq!(patch.file_fingerprint_after(), "fnv1a64:0000000000000002");

        assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
        assert_eq!(audits[1].tool_call_id(), pending.id());
        assert_eq!(audits[1].tool_name(), pending.name());
        assert!(audits[1].proposal().is_none());
        let denied_policy = audits[1]
            .policy()
            .expect("denied audit should include policy");
        assert_eq!(denied_policy.risk_tier(), ActionRiskTier::EditLow);
        assert_eq!(denied_policy.disposition(), ActionPolicyDisposition::Deny);

        let projection = runtime.ledger_projection().await;
        let lifecycle = lifecycle_kinds(&projection);
        let audit_indexes = lifecycle
            .iter()
            .enumerate()
            .filter_map(|(index, kind)| {
                (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(audit_indexes.len(), 2);
        let artifact_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
            .expect("artifact lifecycle should be recorded");
        let resolved_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
            .expect("resolution lifecycle should be recorded");
        assert!(audit_indexes[0] < audit_indexes[1]);
        assert!(audit_indexes[1] < artifact_index);
        assert!(artifact_index < resolved_index);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preflight_outcome_must_be_failed_to_resolve_without_policy_bypass() {
        let executor = ProposingToolExecutor::with_preflight_outcome(
            ToolExecutionOutcome::succeeded_text("must not bypass policy\n"),
        );
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_write_preflight_success"),
            Arc::new(executor.clone()),
            ToolActionKind::WorkspaceWrite,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool(
            "runtime-policy-preflight-success-rejected",
            "policy_write_preflight_success",
            "call-preflight-success-rejected",
            tool,
        )
        .await;

        let error = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect_err("successful preflight outcome must not bypass action policy");

        match error {
            RuntimeError::Core { source } => assert!(
                source.to_string().contains("preflight tool outcome"),
                "unexpected core error: {source}"
            ),
            other => panic!("expected core validation error, got {other:?}"),
        }
        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(
            runtime
                .pending_tool_calls()
                .await
                .iter()
                .map(|call| call.id().as_str().to_owned())
                .collect::<Vec<_>>(),
            vec![pending.id().as_str().to_owned()]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_workspace_write_patch_proposal_executes_and_records_execution_audit() {
        let executor = ProposingToolExecutor::immediate();
        let tool = RegisteredTool::new(
            policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
            Arc::new(executor.clone()),
            ToolActionKind::WorkspaceWrite,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-proposed-workspace-write-opt-in",
            WORKSPACE_PATCH_TOOL_NAME,
            "call-workspace-write-opt-in",
            tool,
            |builder| builder.allow_low_risk_workspace_patches().build(),
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("opted-in low-risk workspace patch should execute");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 1);
        assert_eq!(executor.approved_proposal_seen(), vec![true]);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Succeeded
        );
        assert!(runtime.pending_tool_calls().await.is_empty());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[0].tool_call_id(), pending.id());
        assert_eq!(audits[0].tool_name(), pending.name());
        assert_eq!(audits[0].action_kind(), ToolActionKind::WorkspaceWrite);
        assert!(audits[0].policy().is_none());
        assert!(audits[0].proposal().is_some());
        assert!(audits[0].execution_evidence().is_none());

        assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
        assert_eq!(audits[1].tool_call_id(), pending.id());
        assert_eq!(audits[1].tool_name(), pending.name());
        assert_eq!(audits[1].action_kind(), ToolActionKind::WorkspaceWrite);
        assert!(audits[1].proposal().is_none());
        let policy = audits[1]
            .policy()
            .expect("executed audit should include allow policy");
        assert_eq!(policy.risk_tier(), ActionRiskTier::EditLow);
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
        let ActionExecutionEvidence::WorkspacePatch(evidence) = audits[1]
            .execution_evidence()
            .expect("executed audit should include actual evidence")
        else {
            panic!("workspace patch execution should record workspace patch evidence");
        };
        assert_eq!(evidence.relative_path(), "notes/proposed.txt");
        assert_eq!(evidence.preimage_bytes(), 3);
        assert_eq!(evidence.replacement_bytes(), 7);
        assert_eq!(evidence.file_bytes_before(), 20);
        assert_eq!(evidence.file_bytes_after(), 24);
        assert_eq!(
            evidence.file_fingerprint_before(),
            "fnv1a64:0000000000000001"
        );
        assert_eq!(
            evidence.file_fingerprint_after(),
            "fnv1a64:0000000000000002"
        );

        let projection = runtime.ledger_projection().await;
        let lifecycle = lifecycle_kinds(&projection);
        let audit_indexes = lifecycle
            .iter()
            .enumerate()
            .filter_map(|(index, kind)| {
                (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(audit_indexes.len(), 2);
        let artifact_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
            .expect("artifact lifecycle should be recorded");
        let resolved_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
            .expect("resolution lifecycle should be recorded");
        assert!(audit_indexes[0] < audit_indexes[1]);
        assert!(audit_indexes[1] < artifact_index);
        assert!(artifact_index < resolved_index);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_workspace_write_patch_proposal_rejects_non_patch_tool_name() {
        let executor = ProposingToolExecutor::immediate();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_write_opt_in"),
            Arc::new(executor.clone()),
            ToolActionKind::WorkspaceWrite,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-proposed-workspace-write-opt-in-wrong-tool",
            "policy_write_opt_in",
            "call-workspace-write-opt-in-wrong-tool",
            tool,
            |builder| builder.allow_low_risk_workspace_patches().build(),
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("non-patch-file low-risk proposal should resolve as policy denial");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Failed
        );
        assert_sanitized_policy_denial_content(
            &denied_action_content(&runtime, &events).await,
            "policy_write_opt_in",
        );

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[0].tool_call_id(), pending.id());
        assert_eq!(audits[0].tool_name(), pending.name());
        assert!(audits[0].proposal().is_some());
        assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
        assert_eq!(audits[1].tool_call_id(), pending.id());
        assert_eq!(audits[1].tool_name(), pending.name());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_workspace_write_patch_records_outcome_when_cancelled_after_side_effect() {
        let executor = CancellingOptInPatchExecutor::new();
        let tool = RegisteredTool::new(
            policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
            Arc::new(executor.clone()),
            ToolActionKind::WorkspaceWrite,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-workspace-write-opt-in-cancel-after-side-effect",
            WORKSPACE_PATCH_TOOL_NAME,
            "call-workspace-write-opt-in-cancel-after-side-effect",
            tool,
            |builder| builder.allow_low_risk_workspace_patches().build(),
        )
        .await;
        let token = CancellationToken::new();

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::new(token))
            .await
            .expect("successful opt-in patch execution must be durably recorded");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 1);
        assert_eq!(executor.approved_proposal_seen(), vec![true]);
        assert!(executor.side_effect_happened());
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Succeeded
        );
        assert!(runtime.pending_tool_calls().await.is_empty());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[0].tool_call_id(), pending.id());
        assert_eq!(audits[0].tool_name(), pending.name());
        assert_eq!(audits[0].action_kind(), ToolActionKind::WorkspaceWrite);
        assert!(audits[0].policy().is_none());
        assert!(audits[0].proposal().is_some());
        assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
        assert_eq!(audits[1].tool_call_id(), pending.id());
        assert_eq!(audits[1].tool_name(), pending.name());
        assert_eq!(audits[1].action_kind(), ToolActionKind::WorkspaceWrite);
        assert!(audits[1].proposal().is_none());
        assert!(audits[1].execution_evidence().is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_workspace_write_patch_missing_execution_evidence_fails_closed() {
        let executor = ProposingToolExecutor::missing_execution_evidence();
        let tool = RegisteredTool::new(
            policy_tool_spec(WORKSPACE_PATCH_TOOL_NAME),
            Arc::new(executor.clone()),
            ToolActionKind::WorkspaceWrite,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-workspace-write-opt-in-missing-evidence",
            WORKSPACE_PATCH_TOOL_NAME,
            "call-workspace-write-opt-in-missing-evidence",
            tool,
            |builder| builder.allow_low_risk_workspace_patches().build(),
        )
        .await;
        let projection_before = runtime.ledger_projection().await;

        let err = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect_err("successful admitted patch without evidence must fail closed");

        assert!(matches!(
            err,
            RuntimeError::MissingActionExecutionEvidence { call_id, action_kind, .. }
                if call_id == *pending.id() && action_kind == ToolActionKind::WorkspaceWrite
        ));
        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 1);
        assert_eq!(executor.approved_proposal_seen(), vec![true]);
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert_eq!(runtime.ledger_projection().await, projection_before);
        assert!(action_audit_records(&runtime).await.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_write_tool_without_proposal_opt_in_does_not_call_propose() {
        let executor = ProposingToolExecutor::immediate();
        let (runtime, pending) = register_policy_pending_tool(
            "runtime-policy-proposal-disabled",
            "policy_write_proposal_disabled",
            "call-workspace-write-proposal-disabled",
            ToolActionKind::WorkspaceWrite,
            executor.clone(),
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("policy denial should durably resolve without proposal hook");

        assert_eq!(executor.propose_count(), 0);
        assert_eq!(executor.execute_count(), 0);
        assert_sanitized_policy_denial_content(
            &denied_action_content(&runtime, &events).await,
            "policy_write_proposal_disabled",
        );

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].status(), ActionAuditStatus::Denied);
        assert!(audits[0].proposal().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn command_exec_tool_is_denied_before_executor() {
        let executor = ProposingToolExecutor::immediate();
        let (runtime, pending) = register_policy_pending_tool(
            "runtime-policy-command-exec",
            "policy_command",
            "call-command-exec",
            ToolActionKind::CommandExec,
            executor.clone(),
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("policy denial should durably resolve the pending call");

        assert_eq!(executor.propose_count(), 0);
        assert_eq!(executor.execute_count(), 0);
        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].action_kind(), ToolActionKind::CommandExec);
        assert_eq!(audits[0].status(), ActionAuditStatus::Denied);
        assert_eq!(
            audits[0]
                .policy()
                .expect("denied audit should include policy")
                .disposition(),
            ActionPolicyDisposition::Deny
        );
        let content = denied_action_content(&runtime, &events).await;
        assert_sanitized_policy_denial_content(&content, "policy_command");
        assert_eq!(
            resolved_tool_result(&events)
                .diagnostic()
                .expect("policy denial should include diagnostic")
                .code(),
            DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED
        );
        assert!(runtime.pending_tool_calls().await.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn command_exec_with_process_proposal_records_proposed_then_denied_without_execute() {
        let executor =
            ProcessProposingToolExecutor::with_argv(["cargo", "test", "-p", "merry-runtime"]);
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_proposed"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool(
            "runtime-policy-proposed-command-exec",
            "policy_command_proposed",
            "call-command-exec-proposed",
            tool,
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("policy denial should durably resolve proposed command exec");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_sanitized_policy_denial_content(
            &denied_action_content(&runtime, &events).await,
            "policy_command_proposed",
        );
        assert_eq!(
            resolved_tool_result(&events)
                .diagnostic()
                .expect("policy denial should include diagnostic")
                .code(),
            DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED
        );

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[0].tool_call_id(), pending.id());
        assert_eq!(audits[0].tool_name(), pending.name());
        assert_eq!(audits[0].action_kind(), ToolActionKind::CommandExec);
        assert!(audits[0].policy().is_none());
        let proposal = audits[0]
            .proposal()
            .expect("proposed audit should include process proposal");
        assert_eq!(proposal.action_kind(), ToolActionKind::CommandExec);
        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("command exec proposal should record process action evidence");
        };
        assert_eq!(intent.argv(), ["cargo", "test", "-p", "merry-runtime"]);
        assert_eq!(intent.cwd(), Some("."));
        assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);

        assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
        assert_eq!(audits[1].tool_call_id(), pending.id());
        assert_eq!(audits[1].tool_name(), pending.name());
        assert_eq!(audits[1].action_kind(), ToolActionKind::CommandExec);
        assert!(audits[1].proposal().is_none());
        let denied_policy = audits[1]
            .policy()
            .expect("denied audit should include policy");
        assert_eq!(
            denied_policy.risk_tier(),
            ActionRiskTier::ProcessLocalWorkspaceEffect
        );
        assert_eq!(denied_policy.disposition(), ActionPolicyDisposition::Deny);
        assert_eq!(
            denied_policy.reason(),
            "command execution tool actions are denied by default policy"
        );

        let projection = runtime.ledger_projection().await;
        let lifecycle = lifecycle_kinds(&projection);
        let audit_indexes = lifecycle
            .iter()
            .enumerate()
            .filter_map(|(index, kind)| {
                (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(audit_indexes.len(), 2);
        let artifact_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
            .expect("artifact lifecycle should be recorded");
        let resolved_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
            .expect("resolution lifecycle should be recorded");
        assert!(audit_indexes[0] < audit_indexes[1]);
        assert!(audit_indexes[1] < artifact_index);
        assert!(artifact_index < resolved_index);
        assert!(runtime.pending_tool_calls().await.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_permissions_invalid_arguments_skip_review_and_runner() {
        let review_provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(permission_review_completed_event(
                "approve",
                "Invalid arguments should not reach review.",
            ))]),
        ]);
        let runner = FakeProcessRunner::succeeding();
        let pending = invalid_permission_pending_tool_call("call-permission-invalid-arguments");
        let runtime = Runtime::builder(session_id("runtime-permission-invalid-arguments"))
            .register_tool(request_permissions_tool().expect("permission tool builds"))
            .model_provider_for_role(
                RuntimeModelRole::ApprovalReview,
                Arc::new(review_provider.clone()),
                named_model("fake/approval-review"),
            )
            .allow_permissioned_process_actions(Arc::new(runner.clone()))
            .build()
            .expect("runtime should build");
        {
            let mut session = runtime.inner.session.lock().await;
            session.record_session_started_if_needed();
            session
                .record_tool_call_pending(pending.clone())
                .expect("pending call should record");
        }

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("invalid permission request should resolve failed tool result");

        assert_eq!(runner.call_count(), 0);
        assert!(
            review_provider.recorded_requests().is_empty(),
            "invalid permission arguments must not invoke review"
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            ToolCallResultStatus::Failed
        );
        let result = resolved_tool_result(&events);
        assert_eq!(
            result
                .diagnostic()
                .expect("invalid permission request should include diagnostic")
                .code(),
            "permission_request_invalid_arguments"
        );
        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("invalid permission artifact should be readable");
        let payload: serde_json::Value = serde_json::from_str(
            content
                .as_text()
                .expect("invalid permission result should be textual JSON"),
        )
        .expect("invalid permission artifact should parse as JSON");
        assert_eq!(
            payload["guidance"]["kind"],
            "permission_request_invalid_arguments"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_permissions_approved_by_review_executes_exact_process_action() {
        let review_provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(permission_review_completed_event(
                "approve",
                "The user asked to run this command.",
            ))]),
        ]);
        let runner = FakeProcessRunner::succeeding();
        let runner_factory =
            RecordingPermissionedProcessRunnerFactory::new(Arc::new(runner.clone()));
        let (runtime, pending) = register_permission_pending_tool_with_builder(
            "runtime-permission-approved-process",
            "call-permission-approved-process",
            |builder| {
                builder
                    .model_provider_for_role(
                        RuntimeModelRole::ApprovalReview,
                        Arc::new(review_provider.clone()),
                        named_model("fake/approval-review"),
                    )
                    .permissioned_process_runner_factory(Arc::new(runner_factory.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("approved permission request should execute exact action");

        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(runner_factory.call_count(), 1);
        assert_eq!(runner_factory.observed_network_requests(), [true]);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(runner.observed_intents()[0].argv(), ["cargo", "test"]);
        assert_eq!(
            resolved_tool_result(&events).status(),
            ToolCallResultStatus::Succeeded
        );
        let result = resolved_tool_result(&events);
        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("process artifact should be readable");
        let payload: serde_json::Value = serde_json::from_str(
            content
                .as_text()
                .expect("permissioned process result should be JSON"),
        )
        .expect("process artifact should parse as JSON");
        assert_eq!(payload["kind"], "process_action");
        assert_eq!(
            payload["permission_profile_id"],
            ProcessPermissionProfileId::APPROVED_PERMISSION_REQUEST_V1.as_str()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_permissions_denied_by_review_does_not_execute_process() {
        let review_provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(permission_review_completed_event(
                "deny",
                "The requested network access is not authorized.",
            ))]),
        ]);
        let runner = FakeProcessRunner::succeeding();
        let (runtime, pending) = register_permission_pending_tool_with_builder(
            "runtime-permission-denied-process",
            "call-permission-denied-process",
            |builder| {
                builder
                    .model_provider_for_role(
                        RuntimeModelRole::ApprovalReview,
                        Arc::new(review_provider.clone()),
                        named_model("fake/approval-review"),
                    )
                    .allow_permissioned_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("denied permission request should resolve the tool call");

        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            resolved_tool_result(&events).status(),
            ToolCallResultStatus::Failed
        );
        let result = resolved_tool_result(&events);
        assert_eq!(
            result
                .diagnostic()
                .expect("denied permission should include diagnostic")
                .code(),
            "permission_request_denied"
        );
        let payload = denied_action_content(&runtime, &events).await;
        assert_eq!(payload["guidance"]["kind"], "permission_request_denied");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_permissions_review_uses_approval_role_and_runtime_context() {
        let primary_provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(permission_review_completed_event(
                "approve",
                "Primary should not be used.",
            ))]),
        ]);
        let approval_provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(permission_review_completed_event(
                "deny",
                "Review saw no sufficient authorization.",
            ))]),
        ]);
        let runner = FakeProcessRunner::succeeding();
        let (runtime, pending) = register_permission_pending_tool_with_builder(
            "runtime-permission-review-role-context",
            "call-permission-review-role-context",
            |builder| {
                builder
                    .model_provider(
                        Arc::new(primary_provider.clone()),
                        named_model("fake/primary"),
                    )
                    .model_provider_for_role(
                        RuntimeModelRole::ApprovalReview,
                        Arc::new(approval_provider.clone()),
                        named_model("fake/approval-review"),
                    )
                    .allow_permissioned_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;
        record_prior_failed_tool_result(
            &runtime,
            r#"{"ok":false,"stderr":{"text":"Could not resolve host: crates.io"}}"#,
        );

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("review denial should resolve the tool call");

        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            resolved_tool_result(&events).status(),
            ToolCallResultStatus::Failed
        );
        assert!(
            primary_provider.recorded_requests().is_empty(),
            "approval role should be preferred over primary when configured"
        );
        let approval_requests = approval_provider.recorded_requests();
        assert_eq!(approval_requests.len(), 1);
        assert_eq!(
            approval_requests[0].model().as_str(),
            "fake/approval-review"
        );
        let user_prompt = approval_requests[0].messages()[1].content().as_text();
        assert!(user_prompt.contains(">>> RECENT RUNTIME CONTEXT START"));
        assert!(user_prompt.contains("Please run cargo test"));
        assert!(user_prompt.contains("Could not resolve host: crates.io"));
        assert!(user_prompt.contains("\"network\":true"));
        assert!(user_prompt.contains("\"argv\":[\"cargo\",\"test\"]"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_permissions_review_falls_back_to_primary_model() {
        let primary_provider = RecordingModelProvider::with_script(vec![
            ScriptedModelProviderResponse::Stream(vec![Ok(permission_review_completed_event(
                "approve",
                "Primary review approved because no approval role is configured.",
            ))]),
        ]);
        let runner = FakeProcessRunner::succeeding();
        let (runtime, pending) = register_permission_pending_tool_with_builder(
            "runtime-permission-primary-review-fallback",
            "call-permission-primary-review-fallback",
            |builder| {
                builder
                    .model_provider(
                        Arc::new(primary_provider.clone()),
                        named_model("fake/primary"),
                    )
                    .allow_permissioned_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("primary fallback review should approve execution");

        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            resolved_tool_result(&events).status(),
            ToolCallResultStatus::Succeeded
        );
        let requests = primary_provider.recorded_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model().as_str(), "fake/primary");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_permissions_default_agent_without_review_model_fails_closed() {
        let runner = FakeProcessRunner::succeeding();
        let (runtime, pending) = register_permission_pending_tool_with_builder(
            "runtime-permission-no-review-model",
            "call-permission-no-review-model",
            |builder| {
                builder
                    .allow_permissioned_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("missing review model should durably resolve failed permission request");

        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            resolved_tool_result(&events).status(),
            ToolCallResultStatus::Failed
        );
        let result = resolved_tool_result(&events);
        assert_eq!(
            result
                .diagnostic()
                .expect("blocked permission should include diagnostic")
                .code(),
            "permission_review_failed"
        );
        let payload = denied_action_content(&runtime, &events).await;
        assert_eq!(payload["guidance"]["kind"], "permission_review_failed");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_permissions_without_permissioned_runner_guides_model_to_stop_retrying() {
        let (runtime, pending) = register_permission_pending_tool_with_builder(
            "runtime-permission-no-runner",
            "call-permission-no-runner",
            RuntimeBuilder::build,
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("missing permissioned runner should resolve failed permission request");

        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            result
                .diagnostic()
                .expect("blocked permission should include diagnostic")
                .code(),
            "permission_request_blocked"
        );
        let payload = denied_action_content(&runtime, &events).await;
        assert_eq!(
            payload["guidance"]["kind"],
            "permission_request_unavailable"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_permissions_trusted_sdk_host_decision_can_skip_model_review() {
        let admission = StaticPermissionAdmissionSource::approving();
        let runner = FakeProcessRunner::succeeding();
        let (runtime, pending) = register_permission_pending_tool_with_builder(
            "runtime-permission-trusted-host-decision",
            "call-permission-trusted-host-decision",
            |builder| {
                builder
                    .runtime_trust_level(RuntimeTrustLevel::TrustedSdk)
                    .permission_review_mode(PermissionReviewMode::HostDecisionOnly)
                    .permission_admission_source(Arc::new(admission.clone()))
                    .allow_permissioned_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("trusted host admission should execute exact action");

        assert_eq!(admission.call_count(), 1);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            resolved_tool_result(&events).status(),
            ToolCallResultStatus::Succeeded
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_process_action_uses_runner_and_records_execution_audit() {
        let executor = ProcessProposingToolExecutor::new();
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_opt_in"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-opt-in",
            "policy_command_opt_in",
            "call-command-exec-opt-in",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("opted-in low-risk process action should execute through runner");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
        assert!(result.diagnostic().is_none());
        assert!(matches!(
            &events[0].kind,
            RuntimeEventKind::ArtifactRecorded { artifact } if artifact == result.artifact()
        ));
        assert!(matches!(
            &events[1].kind,
            RuntimeEventKind::ToolCallResolved { result: resolved } if resolved == result
        ));
        assert!(runtime.pending_tool_calls().await.is_empty());

        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("process result artifact should be readable");
        let payload: serde_json::Value = serde_json::from_str(
            content
                .as_text()
                .expect("process result artifact should be textual JSON"),
        )
        .expect("process result artifact should parse as JSON");
        assert_eq!(
            payload,
            json!({
                "ok": true,
                "kind": "process_action",
                "permission_profile_id": "process.read_only.v1",
                "status": {
                    "kind": "exited",
                    "code": 0,
                },
                "intent": {
                    "summary": "process argv[0]=rustc; argc=2; cwd=.",
                    "argv": ["rustc", "--version"],
                    "cwd": ".",
                },
                "stdout": {
                    "text": "runtime tests passed\n",
                    "bytes": "runtime tests passed\n".len(),
                    "truncated": false,
                },
                "stderr": {
                    "text": "",
                    "bytes": 0,
                    "truncated": false,
                }
            })
        );
        assert!(payload.get("provider").is_none());
        assert!(payload.get("wire").is_none());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[0].tool_call_id(), pending.id());
        assert_eq!(audits[0].tool_name(), pending.name());
        assert_eq!(audits[0].action_kind(), ToolActionKind::CommandExec);
        assert!(audits[0].policy().is_none());
        let proposal = audits[0]
            .proposal()
            .expect("proposed audit should include process proposal");
        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposed audit should record process action intent");
        };
        assert_eq!(runner.observed_intents(), vec![intent.clone()]);

        assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
        assert_eq!(audits[1].tool_call_id(), pending.id());
        assert_eq!(audits[1].tool_name(), pending.name());
        assert_eq!(audits[1].action_kind(), ToolActionKind::CommandExec);
        assert!(audits[1].proposal().is_none());
        let policy = audits[1]
            .policy()
            .expect("executed audit should include process allow policy");
        assert_eq!(policy.risk_tier(), ActionRiskTier::ProcessLow);
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
        let ActionExecutionEvidence::ProcessAction(evidence) = audits[1]
            .execution_evidence()
            .expect("executed audit should include process evidence")
        else {
            panic!("process action should record process execution evidence");
        };
        assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
        assert_eq!(evidence.stdout_bytes(), "runtime tests passed\n".len());
        assert!(!evidence.stdout_truncated());
        assert_eq!(evidence.stderr_bytes(), 0);
        assert!(!evidence.stderr_truncated());
        assert_eq!(
            evidence.permission_profile_id().as_str(),
            "process.read_only.v1"
        );
        assert!(evidence.matches_intent(intent));

        let projection = runtime.ledger_projection().await;
        let lifecycle = lifecycle_kinds(&projection);
        let audit_indexes = lifecycle
            .iter()
            .enumerate()
            .filter_map(|(index, kind)| {
                (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(audit_indexes.len(), 2);
        let artifact_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
            .expect("artifact lifecycle should be recorded");
        let resolved_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
            .expect("resolution lifecycle should be recorded");
        assert!(audit_indexes[0] < audit_indexes[1]);
        assert!(audit_indexes[1] < artifact_index);
        assert!(artifact_index < resolved_index);

        let artifact_order = projection
            .entries()
            .iter()
            .find_map(|entry| match entry {
                LedgerProjection::Lifecycle {
                    kind: LedgerFactKind::ArtifactRecorded,
                    order,
                    ..
                } => Some(*order),
                LedgerProjection::Lifecycle { .. } | LedgerProjection::Fact { .. } => None,
            })
            .expect("artifact lifecycle should be projected");
        let resolved_order = projection
            .entries()
            .iter()
            .find_map(|entry| match entry {
                LedgerProjection::Lifecycle {
                    kind: LedgerFactKind::ToolCallResolved,
                    order,
                    ..
                } => Some(*order),
                LedgerProjection::Lifecycle { .. } | LedgerProjection::Fact { .. } => None,
            })
            .expect("resolution lifecycle should be projected");
        let (observation_order, observation_scope, observation_text) = projection
            .entries()
            .iter()
            .find_map(|entry| match entry {
                LedgerProjection::Fact {
                    order, scope, text, ..
                } if text.starts_with("process action `rustc --version`") => {
                    Some((*order, *scope, text.as_str()))
                }
                LedgerProjection::Fact { .. } | LedgerProjection::Lifecycle { .. } => None,
            })
            .expect("process result should be reduced into a compact ledger observation");
        assert_eq!(observation_scope, LedgerScope::Tool);
        assert!(artifact_order < observation_order);
        assert!(observation_order < resolved_order);
        assert!(observation_text.contains("exit code 0"));
        assert!(observation_text.contains("permission_profile=process.read_only.v1"));
        assert!(observation_text.contains("stdout_bytes=21"));
        assert!(observation_text.contains("stderr_bytes=0"));
        assert!(
            observation_text.contains(&format!("artifact={}", result.artifact().id().as_str()))
        );
        assert!(!observation_text.contains("runtime tests passed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_action_artifact_guides_model_when_output_is_truncated() {
        let executor = ProcessProposingToolExecutor::new();
        let runner = FakeProcessRunner::succeeding_with_truncated_stdout();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_truncated_output"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-truncated-output",
            "policy_command_truncated_output",
            "call-command-truncated-output",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("process action should execute");

        let result = resolved_tool_result(&events);
        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("process result artifact should be readable");
        let payload: serde_json::Value = serde_json::from_str(
            content
                .as_text()
                .expect("process result artifact should be textual JSON"),
        )
        .expect("process result artifact should parse as JSON");

        assert_eq!(payload["stdout"]["truncated"], true);
        assert_eq!(payload["stderr"]["truncated"], false);
        assert_eq!(payload["guidance"]["kind"], "process_output_truncated");
        assert_eq!(payload["guidance"]["stdout_truncated"], true);
        assert_eq!(payload["guidance"]["stderr_truncated"], false);
        assert!(
            payload["guidance"]["message"]
                .as_str()
                .expect("guidance message should be text")
                .contains("rerun with a narrower command")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_process_action_denies_dangerous_argv_without_runner_call() {
        let executor = ProcessProposingToolExecutor::with_argv(["sh", "-c", "rm -rf target"]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_dangerous_argv"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-dangerous-argv",
            "policy_command_dangerous_argv",
            "call-command-exec-dangerous-argv",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .allow_accepted_local_workspace_process_actions(
                        accepted_local_workspace_process_admission(),
                        Arc::new(runner.clone()),
                    )
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("dangerous process proposal should be denied durably");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Failed
        );
        assert!(runtime.pending_tool_calls().await.is_empty());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        let proposal = audits[0]
            .proposal()
            .expect("proposed audit should include dangerous argv identity");
        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposal should include process action intent");
        };
        assert_eq!(intent.argv(), ["sh", "-c", "rm -rf target"]);
        assert_eq!(intent.stdin_text(), None);
        assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
        let policy = audits[1]
            .policy()
            .expect("denied audit should include policy");
        assert_eq!(policy.risk_tier(), ActionRiskTier::Forbidden);
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn denied_process_action_traces_denied_tool_finish_without_process_execution() {
        let executor = ProcessProposingToolExecutor::with_argv(["sh", "-c", "rm -rf target"]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_dangerous_trace"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-dangerous-trace",
            "policy_command_dangerous_trace",
            "call-command-exec-dangerous-trace",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .allow_accepted_local_workspace_process_actions(
                        accepted_local_workspace_process_admission(),
                        Arc::new(runner.clone()),
                    )
                    .build()
            },
        )
        .await;

        let (events, logs) = capture_traces_for(
            "runtime-policy-command-exec-dangerous-trace",
            runtime.execute_tool_call(pending.id(), ToolExecutionContext::default()),
        )
        .await;
        let events = events.expect("dangerous process proposal should be denied durably");

        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(runner.call_count(), 0);
        assert!(logs.contains("\"event\":\"runtime.tool.execute.finish\""));
        assert!(logs.contains("\"status\":\"denied\""));
        assert!(logs.contains("\"diagnostic_code\":\"action_policy_denied\""));
        assert!(logs.contains("\"tool_name\":\"policy_command_dangerous_trace\""));
        assert!(logs.contains("\"tool_call_id\":\"call-command-exec-dangerous-trace\""));
        assert!(!logs.contains("runtime.process.execute.start"));
        assert!(!logs.contains("runtime.process.execute.finish"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_process_action_denies_local_workspace_effect_without_accepted_risk_opt_in() {
        let executor =
            ProcessProposingToolExecutor::with_argv(["cargo", "test", "-p", "merry-runtime"]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_local_effect"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-local-effect",
            "policy_command_local_effect",
            "call-command-exec-local-effect",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("local workspace effect process proposal should be denied durably");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Failed
        );
        assert!(runtime.pending_tool_calls().await.is_empty());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        let proposal = audits[0]
            .proposal()
            .expect("proposed audit should include local effect argv identity");
        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposal should include process action intent");
        };
        assert_eq!(intent.argv(), ["cargo", "test", "-p", "merry-runtime"]);
        assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
        let policy = audits[1]
            .policy()
            .expect("denied audit should include policy");
        assert_eq!(
            policy.risk_tier(),
            ActionRiskTier::ProcessLocalWorkspaceEffect
        );
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_accepted_local_workspace_process_action_executes_local_workspace_effect_and_records_policy()
     {
        let executor =
            ProcessProposingToolExecutor::with_argv(["cargo", "test", "-p", "merry-runtime"]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_accepted_local_effect"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-accepted-local-effect",
            "policy_command_accepted_local_effect",
            "call-command-exec-accepted-local-effect",
            tool,
            |builder| {
                builder
                    .allow_accepted_local_workspace_process_actions(
                        accepted_local_workspace_process_admission(),
                        Arc::new(runner.clone()),
                    )
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("accepted local workspace process action should execute through runner");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
        assert!(result.diagnostic().is_none());
        assert!(runtime.pending_tool_calls().await.is_empty());

        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("process result artifact should be readable");
        let payload: serde_json::Value = serde_json::from_str(
            content
                .as_text()
                .expect("process result artifact should be textual JSON"),
        )
        .expect("process result artifact should parse as JSON");
        assert_eq!(
            payload,
            json!({
                "ok": true,
                "kind": "process_action",
                "permission_profile_id": "process.local_workspace.bwrap.v1",
                "status": {
                    "kind": "exited",
                    "code": 0,
                },
                "intent": {
                    "summary": "process argv[0]=cargo; argc=4; cwd=.",
                    "argv": ["cargo", "test", "-p", "merry-runtime"],
                    "cwd": ".",
                },
                "stdout": {
                    "text": "runtime tests passed\n",
                    "bytes": "runtime tests passed\n".len(),
                    "truncated": false,
                },
                "stderr": {
                    "text": "",
                    "bytes": 0,
                    "truncated": false,
                }
            })
        );
        assert!(payload.get("provider").is_none());
        assert!(payload.get("wire").is_none());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[0].tool_call_id(), pending.id());
        assert_eq!(audits[0].tool_name(), pending.name());
        assert_eq!(audits[0].action_kind(), ToolActionKind::CommandExec);
        assert!(audits[0].policy().is_none());
        let proposal = audits[0]
            .proposal()
            .expect("proposed audit should include process proposal");
        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposed audit should record process action intent");
        };
        assert_eq!(intent.argv(), ["cargo", "test", "-p", "merry-runtime"]);
        assert_eq!(intent.cwd(), Some("."));
        assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);
        assert_eq!(intent.stdin_text(), None);
        assert_eq!(runner.observed_intents(), vec![intent.clone()]);

        assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
        assert_eq!(audits[1].tool_call_id(), pending.id());
        assert_eq!(audits[1].tool_name(), pending.name());
        assert_eq!(audits[1].action_kind(), ToolActionKind::CommandExec);
        assert!(audits[1].proposal().is_none());
        let policy = audits[1]
            .policy()
            .expect("executed audit should include process allow policy");
        assert_eq!(
            policy.risk_tier(),
            ActionRiskTier::ProcessLocalWorkspaceEffect
        );
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
        assert_eq!(
            policy.reason(),
            "local workspace effect process actions are allowed only by explicit runtime opt-in for accepted local workspace process risk"
        );
        let ActionExecutionEvidence::ProcessAction(evidence) = audits[1]
            .execution_evidence()
            .expect("executed audit should include process evidence")
        else {
            panic!("process action should record process execution evidence");
        };
        assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
        assert_eq!(evidence.stdout_bytes(), "runtime tests passed\n".len());
        assert!(!evidence.stdout_truncated());
        assert_eq!(evidence.stderr_bytes(), 0);
        assert!(!evidence.stderr_truncated());
        assert_eq!(
            evidence.permission_profile_id().as_str(),
            "process.local_workspace.bwrap.v1"
        );
        assert!(evidence.matches_intent(intent));

        let projection = runtime.ledger_projection().await;
        let lifecycle = lifecycle_kinds(&projection);
        let audit_indexes = lifecycle
            .iter()
            .enumerate()
            .filter_map(|(index, kind)| {
                (*kind == LedgerFactKind::ActionAuditRecorded).then_some(index)
            })
            .collect::<Vec<_>>();
        assert_eq!(audit_indexes.len(), 2);
        let artifact_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ArtifactRecorded)
            .expect("artifact lifecycle should be recorded");
        let resolved_index = lifecycle
            .iter()
            .position(|kind| *kind == LedgerFactKind::ToolCallResolved)
            .expect("resolution lifecycle should be recorded");
        assert!(audit_indexes[0] < audit_indexes[1]);
        assert!(audit_indexes[1] < artifact_index);
        assert!(artifact_index < resolved_index);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn accepted_local_workspace_process_action_denies_when_admission_profile_mismatches() {
        let executor =
            ProcessProposingToolExecutor::with_argv(["cargo", "test", "-p", "merry-runtime"]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_mismatched_local_effect_profile"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let mismatched_admission =
            AcceptedLocalWorkspaceProcessAdmission::for_test_permission_profile_id(
                ProcessPermissionProfileId::READ_ONLY_V1,
            );
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-mismatched-local-effect-profile",
            "policy_command_mismatched_local_effect_profile",
            "call-command-exec-mismatched-local-effect-profile",
            tool,
            |builder| {
                builder
                    .allow_accepted_local_workspace_process_actions(
                        mismatched_admission,
                        Arc::new(runner.clone()),
                    )
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("mismatched local workspace process profile should be denied durably");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Failed
        );
        assert!(runtime.pending_tool_calls().await.is_empty());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
        let policy = audits[1]
            .policy()
            .expect("denied audit should include policy");
        assert_eq!(
            policy.risk_tier(),
            ActionRiskTier::ProcessLocalWorkspaceEffect
        );
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_shell_process_requires_explicit_shell_runner_opt_in() {
        let executor =
            ProcessProposingToolExecutor::with_argv(["bash", "-lc", "rg ProcessRunner | wc -l"]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_shell_read_only_without_shell_opt_in"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-shell-read-only-without-shell-opt-in",
            "policy_command_shell_read_only_without_shell_opt_in",
            "call-command-exec-shell-read-only-without-shell-opt-in",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("shell process proposal should be denied without shell opt-in");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Failed
        );

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
        let policy = audits[1]
            .policy()
            .expect("denied audit should include shell read-only policy");
        assert_eq!(policy.risk_tier(), ActionRiskTier::ProcessShellReadOnly);
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_shell_process_executes_under_shell_profile_when_opted_in() {
        let executor =
            ProcessProposingToolExecutor::with_argv(["bash", "-lc", "rg ProcessRunner | wc -l"]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_shell_read_only_opt_in"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-shell-read-only-opt-in",
            "policy_command_shell_read_only_opt_in",
            "call-command-exec-shell-read-only-opt-in",
            tool,
            |builder| {
                builder
                    .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("opted-in read-only shell process action should execute through shell runner");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
        );
        let RuntimeEventKind::ArtifactRecorded {
            artifact: input_artifact,
        } = &events[0].kind
        else {
            panic!("shell process input artifact should be recorded first");
        };
        assert_eq!(input_artifact.id().as_str(), "process-input-2");
        let input_content = runtime
            .read_artifact_content(input_artifact.id())
            .await
            .expect("shell process input artifact should be readable");
        let input_payload: serde_json::Value = serde_json::from_str(
            input_content
                .as_text()
                .expect("shell process input artifact should be textual JSON"),
        )
        .expect("shell process input artifact should parse as JSON");
        assert_eq!(
            input_payload,
            json!({
                "kind": "shell_command_input",
                "permission_profile_id": "process.shell.read_only.v1",
                "tool_call_id": "call-command-exec-shell-read-only-opt-in",
                "tool_name": "policy_command_shell_read_only_opt_in",
                "intent": {
                    "summary": "process argv[0]=bash; argc=3; cwd=.",
                    "cwd": ".",
                },
                "input_evidence": {
                    "kind": "shell_command_script",
                    "shell": "bash",
                    "flag": "-lc",
                    "script": "rg ProcessRunner | wc -l",
                    "script_bytes": "rg ProcessRunner | wc -l".len(),
                    "script_fingerprint": stable_process_input_fingerprint(
                        "rg ProcessRunner | wc -l".as_bytes()
                    ),
                },
            })
        );
        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);

        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("shell process result artifact should be readable");
        let payload: serde_json::Value = serde_json::from_str(
            content
                .as_text()
                .expect("shell process result artifact should be textual JSON"),
        )
        .expect("shell process result artifact should parse as JSON");
        assert_eq!(
            payload["permission_profile_id"],
            "process.shell.read_only.v1"
        );
        assert!(payload["intent"].get("argv").is_none());
        assert_eq!(
            payload["input_artifact"],
            json!({
                "id": input_artifact.id().as_str(),
                "kind": "json",
            })
        );
        assert!(payload.get("input_evidence").is_none());
        assert!(payload.get("provider").is_none());
        assert!(payload.get("wire").is_none());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
        let policy = audits[1]
            .policy()
            .expect("executed audit should include shell allow policy");
        assert_eq!(policy.risk_tier(), ActionRiskTier::ProcessShellReadOnly);
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
        let ActionExecutionEvidence::ProcessAction(evidence) = audits[1]
            .execution_evidence()
            .expect("executed audit should include shell process evidence")
        else {
            panic!("shell process action should record process execution evidence");
        };
        assert_eq!(
            evidence.permission_profile_id(),
            ProcessPermissionProfileId::SHELL_READ_ONLY_V1
        );
        let ActionProposalEvidence::ProcessAction(intent) = audits[0]
            .proposal()
            .expect("proposed audit should include shell process proposal")
            .evidence()
        else {
            panic!("proposed audit should record shell process intent");
        };
        assert_eq!(runner.observed_intents(), vec![intent.clone()]);
        assert!(evidence.matches_intent(intent));

        let projection = runtime.ledger_projection().await;
        let observation_text = projection
            .entries()
            .iter()
            .find_map(|entry| match entry {
                LedgerProjection::Fact { text, .. }
                    if text.starts_with("shell process action ") =>
                {
                    Some(text.as_str())
                }
                LedgerProjection::Fact { .. } | LedgerProjection::Lifecycle { .. } => None,
            })
            .expect("shell process result should reduce into a compact ledger observation");
        assert!(observation_text.contains("permission_profile=process.shell.read_only.v1"));
        assert!(observation_text.contains("shell=bash"));
        assert!(observation_text.contains("shell_flag=-lc"));
        assert!(observation_text.contains("shell_script_bytes=24"));
        assert!(observation_text.contains(&format!(
            "shell_script_fingerprint={}",
            stable_process_input_fingerprint("rg ProcessRunner | wc -l".as_bytes())
        )));
        assert!(
            observation_text.contains(&format!("artifact={}", result.artifact().id().as_str()))
        );
        assert!(observation_text.contains("input_artifact=process-input-2"));
        assert!(!observation_text.contains("rg ProcessRunner | wc -l"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_shell_process_traces_payload_free_input_metadata_when_opted_in() {
        let script = "rg ProcessRunner | wc -l";
        let executor = ProcessProposingToolExecutor::with_argv(["bash", "-lc", script]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_shell_read_only_trace"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-shell-read-only-trace",
            "policy_command_shell_read_only_trace",
            "call-command-exec-shell-read-only-trace",
            tool,
            |builder| {
                builder
                    .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let (events, logs) = capture_traces_for(
            "runtime-policy-command-exec-shell-read-only-trace",
            runtime.execute_tool_call(pending.id(), ToolExecutionContext::default()),
        )
        .await;
        let events = events.expect("opted-in read-only shell process action should execute");

        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(runner.call_count(), 1);
        assert!(logs.contains("\"event\":\"runtime.process.execute.start\""));
        assert!(logs.contains("\"event\":\"runtime.process.execute.finish\""));
        assert!(logs.contains("\"permission_profile_id\":\"process.shell.read_only.v1\""));
        assert!(logs.contains("\"shell\":\"bash\""));
        assert!(logs.contains("\"shell_flag\":\"-lc\""));
        assert!(logs.contains("\"shell_script_bytes\":24"));
        assert!(logs.contains(&format!(
            "\"shell_script_fingerprint\":\"{}\"",
            stable_process_input_fingerprint(script.as_bytes())
        )));
        assert!(!logs.contains("\"argv\""));
        assert!(!logs.contains(script));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_shell_process_denies_complex_or_mutating_shell_without_runner_call() {
        for (name, argv) in [
            ("redirect", ["bash", "-lc", "rg ProcessRunner > out.txt"]),
            ("substitution", ["bash", "-lc", "echo $(pwd)"]),
            (
                "mutating-segment",
                ["bash", "-lc", "rg ProcessRunner | rm -rf target"],
            ),
        ] {
            let executor = ProcessProposingToolExecutor::with_argv(argv);
            let runner = FakeProcessRunner::succeeding();
            let tool = RegisteredTool::new(
                policy_tool_spec(&format!("policy_command_shell_{name}")),
                Arc::new(executor.clone()),
                ToolActionKind::CommandExec,
            )
            .with_action_proposal();
            let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
                &format!("runtime-policy-command-exec-shell-{name}"),
                &format!("policy_command_shell_{name}"),
                &format!("call-command-exec-shell-{name}"),
                tool,
                |builder| {
                    builder
                        .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                        .build()
                },
            )
            .await;

            let events = runtime
                .execute_tool_call(pending.id(), ToolExecutionContext::default())
                .await
                .expect("non-read-only shell process proposal should be denied durably");

            assert_eq!(executor.propose_count(), 1);
            assert_eq!(executor.execute_count(), 0);
            assert_eq!(runner.call_count(), 0);
            assert_eq!(
                resolved_tool_result(&events).status(),
                merry_core::ToolCallResultStatus::Failed
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn accepted_local_workspace_process_action_executes_unknown_argv_under_bwrap_profile() {
        let executor =
            ProcessProposingToolExecutor::with_argv(["unknown-readonly-ish", "--version"]);
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_unknown_argv"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-unknown-argv",
            "policy_command_unknown_argv",
            "call-command-exec-unknown-argv",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .allow_accepted_local_workspace_process_actions(
                        accepted_local_workspace_process_admission(),
                        Arc::new(runner.clone()),
                    )
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("accepted unknown process proposal should execute through runner");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Succeeded
        );
        assert!(runtime.pending_tool_calls().await.is_empty());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        let proposal = audits[0]
            .proposal()
            .expect("proposed audit should include unknown argv identity");
        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposal should include process action intent");
        };
        assert_eq!(intent.argv(), ["unknown-readonly-ish", "--version"]);
        assert_eq!(intent.stdin_text(), None);
        assert_eq!(runner.observed_intents(), vec![intent.clone()]);
        assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
        let policy = audits[1]
            .policy()
            .expect("executed audit should include policy");
        assert_eq!(
            policy.risk_tier(),
            ActionRiskTier::ProcessLocalWorkspaceEffect
        );
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Allow);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_process_action_commits_output_after_runner_cancels_token() {
        let executor = ProcessProposingToolExecutor::new();
        let runner = FakeProcessRunner::succeeding_then_cancelling_token();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_post_output_cancel"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-post-output-cancel",
            "policy_command_post_output_cancel",
            "call-command-exec-post-output-cancel",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("runner output should commit even if token is cancelled afterward");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        let result = resolved_tool_result(&events);
        assert_eq!(result.status(), merry_core::ToolCallResultStatus::Succeeded);
        assert!(runtime.pending_tool_calls().await.is_empty());

        let content = runtime
            .read_artifact_content(result.artifact().id())
            .await
            .expect("process result artifact should be readable");
        let payload: serde_json::Value = serde_json::from_str(
            content
                .as_text()
                .expect("process result artifact should be textual JSON"),
        )
        .expect("process result artifact should parse as JSON");
        assert_eq!(
            payload
                .pointer("/stdout/text")
                .expect("process stdout text should be present"),
            "runtime tests passed after token cancellation\n"
        );

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[1].status(), ActionAuditStatus::Executed);
        let ActionExecutionEvidence::ProcessAction(evidence) = audits[1]
            .execution_evidence()
            .expect("executed audit should include process evidence")
        else {
            panic!("process action should record execution evidence");
        };
        assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
        assert_eq!(
            evidence.stdout_bytes(),
            "runtime tests passed after token cancellation\n".len()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_process_action_pre_cancel_keeps_pending_without_audit_or_result_artifact() {
        let executor = ProcessProposingToolExecutor::new();
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_pre_cancel"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-pre-cancel",
            "policy_command_pre_cancel",
            "call-command-exec-pre-cancel",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;
        let projection_before = runtime.ledger_projection().await;
        let token = CancellationToken::new();
        token.cancel();

        let err = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::new(token))
            .await
            .expect_err("pre-cancelled process action should not resolve");

        assert!(matches!(
            err,
            RuntimeError::ToolExecutionCancelled { call_id, .. } if call_id == *pending.id()
        ));
        assert_eq!(executor.propose_count(), 0);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 0);
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert_eq!(runtime.ledger_projection().await, projection_before);
        assert!(action_audit_records(&runtime).await.is_empty());
        let expected_result_artifact_id = artifact_id("tool-result-2");
        let evidence_err = runtime
            .evidence_ref(
                &expected_result_artifact_id,
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect_err("pre-cancelled process action must not record result artifact");
        assert!(matches!(
            evidence_err,
            RuntimeError::Artifact {
                source: ArtifactError::MissingArtifact { id }
            } if id == expected_result_artifact_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_process_action_runner_cancel_keeps_pending_without_audit_or_result_artifact() {
        let executor = ProcessProposingToolExecutor::new();
        let runner = FakeProcessRunner::cancelling();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_runner_cancel"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-runner-cancel",
            "policy_command_runner_cancel",
            "call-command-exec-runner-cancel",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;
        let projection_before = runtime.ledger_projection().await;

        let err = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect_err("runner-cancelled process action should not resolve");

        assert!(matches!(
            err,
            RuntimeError::ToolExecutionCancelled { call_id, .. } if call_id == *pending.id()
        ));
        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert_eq!(runtime.ledger_projection().await, projection_before);
        assert!(action_audit_records(&runtime).await.is_empty());
        let expected_result_artifact_id = artifact_id("tool-result-2");
        let evidence_err = runtime
            .evidence_ref(
                &expected_result_artifact_id,
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect_err("runner-cancelled process action must not record result artifact");
        assert!(matches!(
            evidence_err,
            RuntimeError::Artifact {
                source: ArtifactError::MissingArtifact { id }
            } if id == expected_result_artifact_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_shell_process_runner_cancel_keeps_input_artifact_before_unresolved_pending()
    {
        let script = "rg ProcessRunner | wc -l";
        let executor = ProcessProposingToolExecutor::with_argv(["bash", "-lc", script]);
        let runner = FakeProcessRunner::cancelling();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_shell_runner_cancel"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-shell-runner-cancel",
            "policy_command_shell_runner_cancel",
            "call-command-exec-shell-runner-cancel",
            tool,
            |builder| {
                builder
                    .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let err = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect_err("runner-cancelled shell process action should not resolve");

        assert!(matches!(
            err,
            RuntimeError::ToolExecutionCancelled { call_id, .. } if call_id == *pending.id()
        ));
        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert!(action_audit_records(&runtime).await.is_empty());

        let input_content = runtime
            .read_artifact_content(&artifact_id("process-input-2"))
            .await
            .expect("shell input artifact should be durable before runner output");
        let input_payload: serde_json::Value = serde_json::from_str(
            input_content
                .as_text()
                .expect("shell input artifact should be textual JSON"),
        )
        .expect("shell input artifact should parse as JSON");
        assert_eq!(
            input_payload["input_evidence"]["script"],
            "rg ProcessRunner | wc -l"
        );
        assert_eq!(
            input_payload["input_evidence"]["script_fingerprint"],
            stable_process_input_fingerprint(script.as_bytes())
        );
        let input_evidence = runtime
            .evidence_ref(
                &artifact_id("process-input-2"),
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect("shell input artifact should have an exact evidence ref");
        assert_eq!(input_evidence.artifact_id, artifact_id("process-input-2"));
        assert!(
            lifecycle_kinds(&runtime.ledger_projection().await)
                .contains(&LedgerFactKind::ArtifactRecorded)
        );

        let expected_result_artifact_id = artifact_id("tool-result-3");
        let evidence_err = runtime
            .evidence_ref(
                &expected_result_artifact_id,
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect_err("runner-cancelled shell action must not record result artifact");
        assert!(matches!(
            evidence_err,
            RuntimeError::Artifact {
                source: ArtifactError::MissingArtifact { id }
            } if id == expected_result_artifact_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_only_shell_process_runner_failure_keeps_input_artifact_before_unresolved_pending()
    {
        let script = "rg ProcessRunner | wc -l";
        let executor = ProcessProposingToolExecutor::with_argv(["bash", "-lc", script]);
        let runner = FakeProcessRunner::infrastructure_failure("shell runner unavailable");
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_shell_runner_failure"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-shell-runner-failure",
            "policy_command_shell_runner_failure",
            "call-command-exec-shell-runner-failure",
            tool,
            |builder| {
                builder
                    .allow_read_only_shell_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let err = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect_err("infrastructure-failed shell process action should not resolve");

        assert!(matches!(
            err,
            RuntimeError::ToolExecutionFailed { call_id, message, .. }
                if call_id == *pending.id() && message == "shell runner unavailable"
        ));
        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 1);
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert!(action_audit_records(&runtime).await.is_empty());

        let input_content = runtime
            .read_artifact_content(&artifact_id("process-input-2"))
            .await
            .expect("shell input artifact should be durable before runner failure");
        let input_payload: serde_json::Value = serde_json::from_str(
            input_content
                .as_text()
                .expect("shell input artifact should be textual JSON"),
        )
        .expect("shell input artifact should parse as JSON");
        assert_eq!(
            input_payload["input_evidence"]["script"],
            "rg ProcessRunner | wc -l"
        );
        assert_eq!(
            input_payload["input_evidence"]["script_fingerprint"],
            stable_process_input_fingerprint(script.as_bytes())
        );
        let input_evidence = runtime
            .evidence_ref(
                &artifact_id("process-input-2"),
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect("shell input artifact should have an exact evidence ref");
        assert_eq!(input_evidence.artifact_id, artifact_id("process-input-2"));
        assert!(
            lifecycle_kinds(&runtime.ledger_projection().await)
                .contains(&LedgerFactKind::ArtifactRecorded)
        );

        let expected_result_artifact_id = artifact_id("tool-result-3");
        let evidence_err = runtime
            .evidence_ref(
                &expected_result_artifact_id,
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect_err("failed shell action must not record result artifact");
        assert!(matches!(
            evidence_err,
            RuntimeError::Artifact {
                source: ArtifactError::MissingArtifact { id }
            } if id == expected_result_artifact_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opt_in_process_action_with_stdin_is_denied_without_runner_call() {
        let executor = ProcessProposingToolExecutor::with_stdin_text("stdin is not admitted\n");
        let runner = FakeProcessRunner::succeeding();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_command_stdin"),
            Arc::new(executor.clone()),
            ToolActionKind::CommandExec,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool_with_builder(
            "runtime-policy-command-exec-stdin",
            "policy_command_stdin",
            "call-command-exec-stdin",
            tool,
            |builder| {
                builder
                    .allow_low_risk_process_actions(Arc::new(runner.clone()))
                    .build()
            },
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("stdin process proposal should be denied durably");

        assert_eq!(executor.propose_count(), 1);
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runner.call_count(), 0);
        assert_eq!(
            event_kind_names_for_tool_execution(&events),
            ["ArtifactRecorded", "ToolCallResolved"]
        );
        assert_eq!(
            resolved_tool_result(&events).status(),
            merry_core::ToolCallResultStatus::Failed
        );
        assert_sanitized_policy_denial_content(
            &denied_action_content(&runtime, &events).await,
            "policy_command_stdin",
        );
        assert!(runtime.pending_tool_calls().await.is_empty());

        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 2);
        assert_eq!(audits[0].status(), ActionAuditStatus::Proposed);
        assert_eq!(audits[0].action_kind(), ToolActionKind::CommandExec);
        let proposal = audits[0]
            .proposal()
            .expect("proposed audit should include stdin process proposal");
        let ActionProposalEvidence::ProcessAction(intent) = proposal.evidence() else {
            panic!("proposal should include process action intent");
        };
        assert_eq!(intent.argv(), ["cargo", "test", "-p", "merry-runtime"]);
        assert_eq!(intent.cwd(), Some("."));
        assert_eq!(intent.env_policy(), ProcessEnvPolicy::Empty);
        assert_eq!(intent.stdin_text(), None);
        assert_eq!(audits[1].status(), ActionAuditStatus::Denied);
        assert_eq!(audits[1].action_kind(), ToolActionKind::CommandExec);
        let policy = audits[1]
            .policy()
            .expect("denied audit should include policy");
        assert_eq!(
            policy.risk_tier(),
            ActionRiskTier::ProcessLocalWorkspaceEffect
        );
        assert_eq!(policy.disposition(), ActionPolicyDisposition::Deny);
    }

    #[test]
    fn process_execution_evidence_matches_process_action_kind() {
        let intent = ProcessActionIntent::new(
            vec!["rustc".to_owned(), "--version".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            4096,
            4096,
        )
        .expect("valid process intent");
        let evidence = ProcessExecutionEvidence::new(
            &intent,
            ProcessPermissionProfileId::READ_ONLY_V1,
            ProcessExitStatus::Exited(0),
            64,
            false,
            0,
            false,
        )
        .expect("valid process execution evidence");
        let execution_evidence = ActionExecutionEvidence::ProcessAction(evidence);

        assert!(execution_evidence.matches_action_kind(ToolActionKind::CommandExec));
        assert!(!execution_evidence.matches_action_kind(ToolActionKind::WorkspaceWrite));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn network_tool_is_denied_before_executor() {
        let executor = ProposingToolExecutor::immediate();
        let (runtime, pending) = register_policy_pending_tool(
            "runtime-policy-network",
            "policy_network",
            "call-network",
            ToolActionKind::Network,
            executor.clone(),
        )
        .await;

        let events = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::default())
            .await
            .expect("policy denial should durably resolve the pending call");

        assert_eq!(executor.propose_count(), 0);
        assert_eq!(executor.execute_count(), 0);
        let audits = action_audit_records(&runtime).await;
        assert_eq!(audits.len(), 1);
        assert_eq!(audits[0].action_kind(), ToolActionKind::Network);
        assert_eq!(audits[0].status(), ActionAuditStatus::Denied);
        assert_eq!(
            audits[0]
                .policy()
                .expect("denied audit should include policy")
                .disposition(),
            ActionPolicyDisposition::Deny
        );
        let content = denied_action_content(&runtime, &events).await;
        assert_sanitized_policy_denial_content(&content, "policy_network");
        assert_eq!(
            resolved_tool_result(&events)
                .diagnostic()
                .expect("policy denial should include diagnostic")
                .code(),
            DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED
        );
        assert!(runtime.pending_tool_calls().await.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pre_cancelled_denied_tool_execution_keeps_pending_without_artifact() {
        let executor = SuccessfulToolExecutor::new();
        let (runtime, pending) = register_policy_pending_tool(
            "runtime-policy-pre-cancel",
            "policy_pre_cancel",
            "call-policy-pre-cancel",
            ToolActionKind::WorkspaceWrite,
            executor.clone(),
        )
        .await;
        let projection_before = runtime.ledger_projection().await;
        let token = CancellationToken::new();
        token.cancel();

        let err = runtime
            .execute_tool_call(pending.id(), ToolExecutionContext::new(token))
            .await
            .expect_err("pre-cancelled denied tool should not resolve");

        assert!(matches!(
            err,
            crate::RuntimeError::ToolExecutionCancelled { call_id, .. }
                if call_id == *pending.id()
        ));
        assert_eq!(executor.call_count(), 0);
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert_eq!(runtime.ledger_projection().await, projection_before);
        assert!(action_audit_records(&runtime).await.is_empty());
        let expected_result_artifact_id = artifact_id("tool-result-2");
        let evidence_err = runtime
            .evidence_ref(
                &expected_result_artifact_id,
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect_err("pre-cancelled policy denial must not record result artifact");
        assert!(matches!(
            evidence_err,
            crate::RuntimeError::Artifact {
                source: ArtifactError::MissingArtifact { id }
            } if id == expected_result_artifact_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_before_proposal_commit_keeps_pending_without_audit_or_result_artifact() {
        let executor = ProposingToolExecutor::cancelling();
        let tool = RegisteredTool::new(
            policy_tool_spec("policy_proposal_cancel"),
            Arc::new(executor.clone()),
            ToolActionKind::WorkspaceWrite,
        )
        .with_action_proposal();
        let (runtime, pending) = register_policy_pending_registered_tool(
            "runtime-policy-proposal-cancel",
            "policy_proposal_cancel",
            "call-policy-proposal-cancel",
            tool,
        )
        .await;
        let projection_before = runtime.ledger_projection().await;
        let token = CancellationToken::new();
        let execute_runtime = runtime.clone();
        let execute_call_id = pending.id().clone();
        let execute_token = token.clone();

        let handle = tokio::spawn(async move {
            execute_runtime
                .execute_tool_call(&execute_call_id, ToolExecutionContext::new(execute_token))
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(executor.propose_count(), 1);

        token.cancel();
        let err = handle
            .await
            .expect("proposal cancellation task should not panic")
            .expect_err("cancelled proposal should not resolve");

        assert!(matches!(
            err,
            crate::RuntimeError::ToolExecutionCancelled { call_id, .. }
                if call_id == *pending.id()
        ));
        assert_eq!(executor.execute_count(), 0);
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert_eq!(runtime.ledger_projection().await, projection_before);
        assert!(action_audit_records(&runtime).await.is_empty());
        let expected_result_artifact_id = artifact_id("tool-result-2");
        let evidence_err = runtime
            .evidence_ref(
                &expected_result_artifact_id,
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect_err("cancelled proposal must not record result artifact");
        assert!(matches!(
            evidence_err,
            crate::RuntimeError::Artifact {
                source: ArtifactError::MissingArtifact { id }
            } if id == expected_result_artifact_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_event_send_returns_false_when_channel_is_closed() {
        let inner = runtime_inner();
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);

        let sent = send_cancelled_event(&inner, &sender).await;
        let projection = {
            let session = inner.session.lock().await;
            session.ledger_projection()
        };

        assert!(!sent);
        assert!(projection.entries().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_unregistered_tool_while_waiting_to_submit_keeps_pending() {
        let session_id =
            SessionId::new("runtime-unregistered-submit-cancel").expect("valid session id");
        let call_id = ToolCallId::new("call-unregistered").expect("valid tool call id");
        let pending = PendingToolCall::new(
            call_id.clone(),
            ToolName::new("missing_tool").expect("valid tool name"),
            ToolCallArguments::new(Default::default()),
        );
        let runtime = Runtime::builder(session_id)
            .build()
            .expect("runtime should build");

        let mut initial_session_guard = runtime.inner.session.lock().await;
        initial_session_guard
            .record_tool_call_pending(pending.clone())
            .expect("pending call should record");
        let projection_before = initial_session_guard.ledger_projection();

        let token = CancellationToken::new();
        let execute_runtime = runtime.clone();
        let execute_call_id = call_id.clone();
        let execute_token = token.clone();
        let execute_handle = tokio::spawn(async move {
            execute_runtime
                .execute_tool_call(&execute_call_id, ToolExecutionContext::new(execute_token))
                .await
        });
        tokio::task::yield_now().await;

        let (lock_acquired_sender, lock_acquired_receiver) = oneshot::channel();
        let (release_lock_sender, release_lock_receiver) = oneshot::channel();
        let blocker_runtime = runtime.clone();
        let blocker_handle = tokio::spawn(async move {
            let _session_guard = blocker_runtime.inner.session.lock().await;
            let _ = lock_acquired_sender.send(());
            let _ = release_lock_receiver.await;
        });
        tokio::task::yield_now().await;

        drop(initial_session_guard);
        lock_acquired_receiver
            .await
            .expect("blocker should acquire the session lock after pending lookup");
        tokio::task::yield_now().await;

        token.cancel();
        release_lock_sender
            .send(())
            .expect("blocker should still be waiting for release");

        let err = execute_handle
            .await
            .expect("tool execution task should not panic")
            .expect_err("cancelled unregistered tool execution should not resolve pending");
        blocker_handle
            .await
            .expect("session lock blocker should not panic");

        assert!(matches!(
            err,
            crate::RuntimeError::ToolExecutionCancelled { call_id: cancelled, .. }
                if cancelled == call_id
        ));
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert_eq!(runtime.ledger_projection().await, projection_before);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_registered_tool_after_success_before_submit_keeps_pending() {
        let session_id =
            SessionId::new("runtime-registered-submit-cancel").expect("valid session id");
        let call_id = ToolCallId::new("call-registered").expect("valid tool call id");
        let tool_spec = registered_tool_spec();
        let pending = PendingToolCall::new(
            call_id.clone(),
            tool_spec.name().clone(),
            ToolCallArguments::new(Default::default()),
        );
        let executor = SuccessfulToolExecutor::new();
        let runtime = Runtime::builder(session_id)
            .register_tool(RegisteredTool::read_only(
                tool_spec,
                Arc::new(executor.clone()),
            ))
            .build()
            .expect("runtime should build");

        let mut initial_session_guard = runtime.inner.session.lock().await;
        initial_session_guard
            .record_tool_call_pending(pending.clone())
            .expect("pending call should record");
        let projection_before = initial_session_guard.ledger_projection();

        let token = CancellationToken::new();
        let execute_runtime = runtime.clone();
        let execute_call_id = call_id.clone();
        let execute_token = token.clone();
        let execute_handle = tokio::spawn(async move {
            execute_runtime
                .execute_tool_call(&execute_call_id, ToolExecutionContext::new(execute_token))
                .await
        });
        tokio::task::yield_now().await;

        let (lock_acquired_sender, lock_acquired_receiver) = oneshot::channel();
        let (release_lock_sender, release_lock_receiver) = oneshot::channel();
        let blocker_runtime = runtime.clone();
        let blocker_handle = tokio::spawn(async move {
            let _session_guard = blocker_runtime.inner.session.lock().await;
            let _ = lock_acquired_sender.send(());
            let _ = release_lock_receiver.await;
        });
        tokio::task::yield_now().await;

        drop(initial_session_guard);
        lock_acquired_receiver
            .await
            .expect("blocker should acquire the session lock after pending lookup");
        tokio::task::yield_now().await;
        assert_eq!(executor.call_count(), 1);

        token.cancel();
        release_lock_sender
            .send(())
            .expect("blocker should still be waiting for release");

        let err = execute_handle
            .await
            .expect("tool execution task should not panic")
            .expect_err("late-cancelled registered tool execution should not resolve pending");
        blocker_handle
            .await
            .expect("session lock blocker should not panic");

        assert!(matches!(
            err,
            crate::RuntimeError::ToolExecutionCancelled { call_id: cancelled, .. }
                if cancelled == call_id
        ));
        assert_eq!(runtime.pending_tool_calls().await, vec![pending]);
        assert_eq!(runtime.ledger_projection().await, projection_before);

        let expected_result_artifact_id = artifact_id("tool-result-1");
        let evidence_err = runtime
            .evidence_ref(
                &expected_result_artifact_id,
                EvidenceLocator::whole_artifact(),
            )
            .await
            .expect_err("cancelled tool execution must not record runtime-owned result artifact");
        assert!(matches!(
            evidence_err,
            crate::RuntimeError::Artifact {
                source: ArtifactError::MissingArtifact { id }
            } if id == expected_result_artifact_id
        ));
    }
}
