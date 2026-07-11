//! Runtime builder and step execution skeleton.
//!
//! [`Runtime`] is the MVP facade for session-owned state. Step execution and
//! direct mutation APIs admit one active operation at a time, record durable
//! session state before returning observable events where applicable, and keep
//! provider wire details behind the `merry-llm` provider boundary.

use crate::{
    AcceptedLocalWorkspaceProcessAdmission, CheckpointRefId, CitationCompactionInput,
    CitationCompactionPolicy, CompactedCheckpointSummary, CompactionError, CompactionOutcome,
    FileSessionStore, ProcessRunner, RuntimeCapabilities, RuntimeError, RuntimeModelRole,
    TextEvidencePage,
    events::{
        ActiveStepPermit, RuntimeEventProjector, RuntimeEventStream, RuntimeJournalEventBatch,
        RuntimeJournalEventStream,
    },
    judgment::{JudgmentContext, JudgmentError, JudgmentRecord, JudgmentRequest, JudgmentSource},
    memory::MemoryActivationSource,
    model_config::{ModelProviderConfig, RuntimeModelConfigs},
    permission::{PermissionAdmissionSource, PermissionReviewMode, RuntimeTrustLevel},
    process::PermissionedProcessRunnerFactory,
    session::SessionState,
    step::{StepContext, StepInput},
    subagent::SubagentManager,
    tool::{ToolExecutionContext, ToolRegistry},
};
use merry_core::{PendingToolCall, RuntimeEvent, RuntimeJournalEvent, SessionId, ToolCallId};
use merry_llm::{GenerationConfig, ModelName, ModelProvider, ModelRetryPolicy};
use std::{
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

mod auto_compaction;
mod builder;
mod checkpoint_ref_tool;
mod diagnostics;
mod journal_emission;
mod memory_activation;
mod model_output;
mod permission_execution;
mod process_execution;
mod provider_request;
mod provider_step;
mod session_access;
mod tool_batch;
mod tool_execution;

use self::auto_compaction::{
    compact_context_once_inner, compaction_input_for_policy,
    install_citation_compaction_candidate_transactionally,
};
pub use self::builder::{AutomaticCompactionConfig, RuntimeBuilder};
#[cfg(test)]
use self::checkpoint_ref_tool::merry_read_checkpoint_ref_tool_name;
use self::diagnostics::{
    DIAGNOSTIC_TOOL_ACTION_POLICY_DENIED, DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
    DIAGNOSTIC_TOOL_NOT_REGISTERED, TOOL_ACTION_POLICY_DENIED_MESSAGE, WORKSPACE_PATCH_TOOL_NAME,
    diagnostic_from_text,
};
use self::journal_emission::{
    send_cancelled_event, send_cancelled_if_requested, send_normal_event,
};
#[cfg(test)]
use self::memory_activation::memory_activation_seed_from_step_input;
#[cfg(test)]
use self::provider_request::{request_context_budget, step_usage_context_snapshot};
#[cfg(test)]
use self::tool_execution::admit_action_to_generic_executor;

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

    /// Resumes a session from the default XDG state store.
    pub async fn resume(session_id: SessionId) -> Result<Self, RuntimeError> {
        let store = FileSessionStore::default_store()?;
        Self::builder(session_id).resume_from_store(store).await
    }

    /// Starts a runtime step and returns its event stream.
    ///
    /// Only one step or direct mutation may own the runtime at a time. The
    /// step producer owns the initial active-step permit. Dropping the returned
    /// [`RuntimeJournalEventStream`] cancels and aborts the producer; the runtime
    /// becomes available after the producer and any in-flight persistence
    /// transaction have dropped their permit handles.
    ///
    /// All events emitted by the step are provider-neutral [`RuntimeJournalEvent`]
    /// values. The runtime records session, ledger, artifact, and pending-tool
    /// state before the corresponding event becomes observable.
    ///
    /// Cancellation records a cancelled event when the producer reaches a
    /// cancellation checkpoint. Pending tool calls remain pending unless a
    /// durable result has already been recorded. If compaction persistence is
    /// already in flight, its transaction keeps the active-step permit until
    /// the staged state is discarded or durably installed.
    pub fn step(
        &self,
        input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeJournalEventStream, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;

        self.step_with_active_permit(input, context, active_permit)
    }

    /// Starts a runtime step and returns the raw ordered journal stream.
    ///
    /// This is the explicit low-level alias for [`Runtime::step`]. Runtime
    /// internals, debugging tools, and replay inspection should use this API
    /// when they need exact journal payloads rather than SDK-facing projection.
    pub fn journal_stream(
        &self,
        input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeJournalEventStream, RuntimeError> {
        self.step(input, context)
    }

    /// Starts a runtime step and returns SDK-facing public events.
    ///
    /// Public events are projected from the internal journal after the
    /// corresponding session state is recorded. They expose assistant text and
    /// tool activity directly while omitting internal bridge handoff details.
    pub fn stream(
        &self,
        input: StepInput,
        context: StepContext,
    ) -> Result<RuntimeEventStream, RuntimeError> {
        let journal_stream = self.step(input, context)?;
        Ok(RuntimeEventStream::new(
            journal_stream,
            self.clone(),
            self.inner.event_buffer_size.get(),
        ))
    }

    /// Projects raw ordered journal events into SDK-facing public events.
    ///
    /// This helper is read-only and is intended for SDK bindings that need to
    /// return public events for a completed run while preserving raw journal
    /// evidence inside Rust runtime results.
    pub async fn project_journal_events(
        &self,
        events: &[RuntimeJournalEvent],
    ) -> Result<Vec<RuntimeEvent>, RuntimeError> {
        let mut projector = RuntimeEventProjector::new();
        let mut projected = Vec::new();
        for event in events {
            if let Some(event) = projector.project(event.clone(), self).await? {
                projected.push(event);
            }
        }
        Ok(projected)
    }

    /// Returns the latest authoritative session usage snapshot.
    pub async fn usage(&self) -> Option<merry_core::SessionUsage> {
        let session = self.inner.session.lock().await;
        session.usage()
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

    /// Saves the current resume-safe session state to the provided store.
    pub async fn save_session_to(&self, store: FileSessionStore) -> Result<(), RuntimeError> {
        let _active_permit = self.acquire_active_step_permit()?;
        let bundle = {
            let session = self.inner.session.lock().await;
            session.persistable_bundle()?
        };
        store.write_bundle(bundle).await?;
        Ok(())
    }

    /// Saves the current resume-safe session state to the configured store.
    pub async fn save_session(&self) -> Result<(), RuntimeError> {
        let store = self
            .inner
            .session_store
            .clone()
            .unwrap_or(FileSessionStore::default_store()?);
        self.save_session_to(store).await
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

    /// Returns the configured skill metadata for UI/SDK discovery.
    ///
    /// This exposes only metadata already stored in the session. Skill bodies
    /// remain on disk and do not enter prompt context through this accessor.
    pub async fn skills(&self) -> Vec<crate::SkillMetadata> {
        let session = self.inner.session.lock().await;
        session
            .skill_catalog()
            .map(|catalog| catalog.skills().to_vec())
            .unwrap_or_default()
    }

    /// Finds one configured skill by exact metadata name.
    pub async fn find_skill(&self, name: &str) -> Option<crate::SkillMetadata> {
        self.skills()
            .await
            .into_iter()
            .find(|skill| skill.name() == name)
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
    ) -> Result<RuntimeJournalEventStream, RuntimeError> {
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

        Ok(RuntimeJournalEventStream::new(
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
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
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
    ) -> Result<Vec<RuntimeJournalEvent>, RuntimeError> {
        tool_execution::execute_tool_call_with_active_permit(&self.inner, call_id, context).await
    }

    pub(crate) async fn execute_tool_call_batch_with_active_permit(
        &self,
        calls: Vec<PendingToolCall>,
        context: ToolExecutionContext,
        _active_permit: &ActiveStepPermit,
    ) -> tool_batch::ToolBatchExecution {
        tool_batch::execute_tool_call_batch_with_active_permit(&self.inner, calls, context).await
    }

    /// Payload-free summary of the installed compacted checkpoint, if any.
    pub async fn compacted_checkpoint_summary(&self) -> Option<CompactedCheckpointSummary> {
        let session = self.inner.session.lock().await;
        session.compacted_checkpoint_summary()
    }

    /// Reads one bounded page from a checkpoint ref's original artifact evidence.
    pub async fn read_checkpoint_ref_page(
        &self,
        ref_id: &CheckpointRefId,
        offset: usize,
        max_bytes: usize,
    ) -> Result<TextEvidencePage, RuntimeError> {
        let session = self.inner.session.lock().await;
        session.read_checkpoint_ref_page(ref_id, offset, max_bytes)
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
        compaction_input_for_policy(&self.inner, policy).await
    }

    /// Installs a validated citation compaction candidate and removes the covered history prefix.
    pub async fn install_citation_compaction_candidate(
        &self,
        input: CitationCompactionInput,
        candidate_json: &str,
    ) -> Result<CompactionOutcome, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
            .ok_or_else(|| RuntimeError::StepAlreadyActive {
                session_id: self.inner.session_id.clone(),
            })?;
        install_citation_compaction_candidate_transactionally(
            Arc::clone(&self.inner),
            input,
            candidate_json,
            CancellationToken::new(),
            active_permit,
        )
        .await
    }

    /// Runs one model-backed compaction pass when a compressible history prefix exists.
    pub async fn compact_context_once(
        &self,
        policy: CitationCompactionPolicy,
        context: StepContext,
    ) -> Result<Option<CompactionOutcome>, RuntimeError> {
        let active_permit = ActiveStepPermit::acquire(Arc::clone(&self.inner.active_step))
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

        compact_context_once_inner(&self.inner, policy, token, active_permit).await
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

async fn persist_resume_safe_savepoint_if_configured(inner: &RuntimeInner) {
    let Some(store) = inner.session_store.clone() else {
        return;
    };
    let bundle = {
        let session = inner.session.lock().await;
        session.persistable_bundle_if_resume_safe()
    };
    let bundle = match bundle {
        Ok(Some(bundle)) => bundle,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                session_id = %inner.session_id,
                error = %error,
                "automatic session resume savepoint skipped"
            );
            return;
        }
    };
    if let Err(error) = store.write_bundle(bundle).await {
        tracing::warn!(
            session_id = %inner.session_id,
            error = %error,
            "automatic session resume savepoint failed"
        );
    }
}

struct RuntimeInner {
    session_id: SessionId,
    session: Mutex<SessionState>,
    active_step: Arc<AtomicBool>,
    memory_projection_epoch: AtomicU64,
    event_buffer_size: NonZeroUsize,
    max_parallel_tool_calls: NonZeroUsize,
    model_configs: RuntimeModelConfigs,
    primary_model_override: RwLock<Option<ModelProviderConfig>>,
    automatic_compaction: RwLock<AutomaticCompactionConfig>,
    context_window_tokens: RwLock<Option<NonZeroU64>>,
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
    session_store: Option<FileSessionStore>,
}

impl RuntimeInner {
    async fn model_config(&self, role: RuntimeModelRole) -> Option<ModelProviderConfig> {
        if role == RuntimeModelRole::Primary
            && let Some(config) = self.primary_model_override.read().await.as_ref()
        {
            return Some(config.clone());
        }
        self.model_configs.get(role)
    }

    async fn model_config_with_primary_fallback(
        &self,
        role: RuntimeModelRole,
    ) -> Option<ModelProviderConfig> {
        if role != RuntimeModelRole::Primary
            && let Some(config) = self.model_configs.get(role)
        {
            return Some(config);
        }
        self.model_config(RuntimeModelRole::Primary).await
    }

    fn visible_tool_specs(&self) -> Vec<merry_core::ToolSpec> {
        let mut specs = self.tool_registry.tool_specs();
        if let Some(manager) = self.subagent_manager.as_ref() {
            specs.retain(|spec| manager.is_tool_visible(spec.name()));
        }
        specs
    }
}

impl Runtime {
    /// Returns the automatic compaction policy used by subsequent requests.
    pub async fn automatic_compaction_config(&self) -> AutomaticCompactionConfig {
        *self.inner.automatic_compaction.read().await
    }

    pub(crate) async fn update_interactive_primary_model(
        &self,
        provider: Arc<dyn ModelProvider>,
        model: ModelName,
        retry_policy: ModelRetryPolicy,
    ) {
        *self.inner.primary_model_override.write().await =
            Some(ModelProviderConfig::new(provider, model, retry_policy));
    }

    pub(crate) async fn update_interactive_subagents(
        &self,
        enabled: bool,
        config: crate::SubagentConfig,
    ) -> Result<(), RuntimeError> {
        let manager =
            self.inner
                .subagent_manager
                .as_ref()
                .ok_or(RuntimeError::InvalidStepInput {
                    reason: "interactive subagent runtime control is unavailable",
                })?;
        manager.update_policy(enabled, config).await
    }

    pub(crate) async fn update_interactive_automatic_compaction(
        &self,
        config: AutomaticCompactionConfig,
    ) {
        *self.inner.automatic_compaction.write().await = config;
    }

    pub(crate) async fn update_interactive_context_window_tokens(
        &self,
        context_window_tokens: Option<NonZeroU64>,
    ) {
        *self.inner.context_window_tokens.write().await = context_window_tokens;
    }
}

#[derive(Clone)]
struct AcceptedLocalWorkspaceProcessRunner {
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
}

async fn run_step(
    inner: Arc<RuntimeInner>,
    sender: mpsc::Sender<RuntimeJournalEventBatch>,
    token: CancellationToken,
    input: StepInput,
    generation_config: GenerationConfig,
    final_output_contract: Option<crate::FinalOutputContract>,
    active_permit: ActiveStepPermit,
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

    let Some(provider_config) = inner.model_config(RuntimeModelRole::Primary).await else {
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
        provider_step::ProviderStepControl::new(&token, &active_permit),
        input,
        generation_config,
        final_output_contract,
        provider_config,
    )
    .await;
}

#[cfg(test)]
mod tests {
    mod bridge_tool_flow;
    mod builder_checkpoint;
    mod checkpoint_ref_tool;
    mod compaction_transaction;
    mod context_cache;
    mod event_cancellation;
    mod memory_activation_flow;
    mod model_role_flow;
    mod permission_execution;
    mod process_cancellation;
    mod process_execution;
    mod process_shell_execution;
    mod provider_step_flow;
    mod provider_step_turn_lifecycle;
    mod session_resume;
    mod tool_execution;
    mod tool_submit_cancellation;
    mod uncertainty_review;

    include!("runtime/tests/support.rs");
}
