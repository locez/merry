//! Runtime builder and step execution skeleton.
//!
//! [`Runtime`] is the MVP facade for session-owned state. Step execution and
//! direct mutation APIs admit one active operation at a time, record durable
//! session state before returning observable events where applicable, and keep
//! provider wire details behind the `merry-llm` provider boundary.

use crate::{
    ArtifactContent, ContextCompiler, ContextEntry, ContextSummary, LedgerProjectionSnapshot,
    RuntimeError, RuntimeEventStream, SessionContextSnapshot,
    event_stream::ActiveStepPermit,
    judgment::{JudgmentContext, JudgmentError, JudgmentRecord, JudgmentRequest, JudgmentSource},
    memory::{
        MemoryActivationContext, MemoryActivationSeed, MemoryActivationSource,
        MemoryActivationSourceKind, MemoryScope, StoredMemoryActivationSource,
    },
    session::{SessionState, is_runtime_reserved_artifact_id},
    step::{StepContext, StepInput, compile_step_model_request},
    tool::{RegisteredTool, ToolExecutionContext, ToolExecutionError, ToolRegistry},
};
use futures_util::StreamExt;
use merry_core::{
    ArtifactId, ArtifactRef, CoreError, ErrorInfo, EvidenceLocator, EvidenceRef, PendingToolCall,
    RuntimeEvent, SessionId, ToolCallArguments, ToolCallId, ToolCallResult,
};
use merry_llm::{
    FinishReason, GenerationConfig, ModelError, ModelEvent, ModelName, ModelOutput, ModelProvider,
    ModelStreamContext, ModelToolCall, ProviderErrorKind,
};
use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::{Mutex, mpsc, mpsc::Permit};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

const DEFAULT_EVENT_BUFFER_SIZE: usize = 16;
const DIAGNOSTIC_MODEL_TOOL_CALL_INVALID: &str = "model_tool_call_invalid";
const DIAGNOSTIC_MODEL_TOOL_CALL_MISSING: &str = "model_tool_call_missing";
const DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT: &str = "model_tool_call_mixed_output";
const DIAGNOSTIC_MODEL_PARALLEL_TOOL_CALLS_UNSUPPORTED: &str =
    "model_parallel_tool_calls_unsupported";
const DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED: &str = "tool_call_result_required";
const DIAGNOSTIC_TOOL_NOT_REGISTERED: &str = "tool_not_registered";

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

    pub(crate) fn step_with_active_permit(
        &self,
        input: StepInput,
        context: StepContext,
        active_permit: ActiveStepPermit,
    ) -> Result<RuntimeEventStream, RuntimeError> {
        let (parent_token, generation_config) = context.into_parts();
        let step_token = parent_token.child_token();
        let producer_token = step_token.clone();
        let (sender, receiver) = mpsc::channel(self.inner.event_buffer_size.get());
        let inner = Arc::clone(&self.inner);
        let producer_span = tracing::debug_span!(
            "runtime.step",
            session_id = self.inner.session_id.as_str(),
            event_buffer_size = self.inner.event_buffer_size.get(),
            provider_configured = self.inner.model_provider.is_some(),
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

        if is_runtime_reserved_artifact_id(result.artifact().id()) {
            return Err(RuntimeError::ReservedArtifactId {
                artifact_id: result.artifact().id().clone(),
            });
        }

        let mut session = self.inner.session.lock().await;
        session.submit_tool_result(result, content)
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
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: self.inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }

        let pending = {
            let session = self.inner.session.lock().await;
            session
                .pending_tool_call(call_id)
                .ok_or_else(|| RuntimeError::UnknownToolCall {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                })?
        };

        let Some(executor) = self.inner.tool_registry.executor(pending.name()) else {
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }

            let diagnostic = diagnostic_from_text(
                DIAGNOSTIC_TOOL_NOT_REGISTERED,
                format!("tool {} is not registered", pending.name()),
            );
            let content = ArtifactContent::json(format!(
                r#"{{"error":"tool_not_registered","tool":"{}"}}"#,
                pending.name()
            ));
            let mut session = self.inner.session.lock().await;
            if context.cancellation_token().is_cancelled() {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            return session.submit_tool_execution_outcome(
                call_id,
                merry_core::ToolCallResultStatus::Failed,
                content,
                Some(diagnostic),
            );
        };

        let execution = tokio::select! {
            biased;
            () = context.cancellation_token().cancelled() => {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            execution = executor.execute(pending, context.clone()) => execution,
        };

        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: self.inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }

        let outcome = match execution {
            Ok(outcome) => outcome,
            Err(ToolExecutionError::Cancelled) => {
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                });
            }
            Err(ToolExecutionError::Infrastructure { message }) => {
                return Err(RuntimeError::ToolExecutionFailed {
                    session_id: self.inner.session_id.clone(),
                    call_id: call_id.clone(),
                    message,
                });
            }
        };

        let (status, content, diagnostic) = outcome.into_parts();
        let mut session = self.inner.session.lock().await;
        if context.cancellation_token().is_cancelled() {
            return Err(RuntimeError::ToolExecutionCancelled {
                session_id: self.inner.session_id.clone(),
                call_id: call_id.clone(),
            });
        }
        session.submit_tool_execution_outcome(call_id, status, content, diagnostic)
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
    model_provider: Option<ModelProviderConfig>,
    registered_tools: Vec<RegisteredTool>,
    memory_activation_source: Arc<dyn MemoryActivationSource>,
}

impl RuntimeBuilder {
    fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            event_buffer_size: NonZeroUsize::new(DEFAULT_EVENT_BUFFER_SIZE)
                .expect("default event buffer size is non-zero"),
            model_provider: None,
            registered_tools: Vec::new(),
            memory_activation_source: Arc::new(StoredMemoryActivationSource),
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
        self.model_provider = Some(ModelProviderConfig { provider, model });
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

        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: self.session_id.clone(),
                session: Mutex::new(SessionState::new(self.session_id)),
                active_step: Arc::new(AtomicBool::new(false)),
                memory_projection_epoch: AtomicU64::new(0),
                event_buffer_size: self.event_buffer_size,
                model_provider: self.model_provider,
                tool_registry,
                memory_activation_source: self.memory_activation_source,
            }),
        })
    }
}

#[derive(Clone)]
struct ModelProviderConfig {
    provider: Arc<dyn ModelProvider>,
    model: ModelName,
}

struct RuntimeInner {
    session_id: SessionId,
    session: Mutex<SessionState>,
    active_step: Arc<AtomicBool>,
    memory_projection_epoch: AtomicU64,
    event_buffer_size: NonZeroUsize,
    model_provider: Option<ModelProviderConfig>,
    tool_registry: ToolRegistry,
    memory_activation_source: Arc<dyn MemoryActivationSource>,
}

async fn run_step(
    inner: Arc<RuntimeInner>,
    sender: mpsc::Sender<RuntimeEvent>,
    token: CancellationToken,
    input: StepInput,
    generation_config: GenerationConfig,
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

    let Some(provider_config) = inner.model_provider.clone() else {
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
    run_provider_step(
        &inner,
        &sender,
        &token,
        input,
        generation_config,
        provider_config,
    )
    .await;
}

async fn run_provider_step(
    inner: &Arc<RuntimeInner>,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    input: StepInput,
    generation_config: GenerationConfig,
    provider_config: ModelProviderConfig,
) {
    if has_unresolved_pending_tool_calls(inner).await {
        tracing::debug!(
            category = "unresolved_pending_tool_gate",
            diagnostic_code = DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
            "runtime provider step gated by unresolved pending tool call"
        );
        let diagnostic = diagnostic_from_text(
            DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
            "a pending tool call must be resolved before the next provider step",
        );
        trace_provider_step_failed(&diagnostic);
        let _ = send_failed_event(inner, sender, token, diagnostic).await;
        return;
    }

    clear_current_activated_memories(inner).await;

    if token.is_cancelled() {
        trace_provider_step_cancelled();
        let _ = send_cancelled_event(inner, sender).await;
        return;
    }

    let seed = match memory_activation_seed_from_step_input(&input) {
        Ok(seed) => seed,
        Err(error) => {
            let diagnostic = diagnostic_from_text("memory_activation", error.to_string());
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let candidates = {
        let session = inner.session.lock().await;
        session.memory_store().candidate_snapshot()
    };
    tracing::debug!(
        category = "memory_candidate_count",
        count = candidates.len(),
        "runtime memory candidates collected"
    );
    if token.is_cancelled() {
        trace_provider_step_cancelled();
        let _ = send_cancelled_event(inner, sender).await;
        return;
    }

    let activation_context = MemoryActivationContext::new(token.clone());
    let activation_result = tokio::select! {
        biased;
        () = token.cancelled() => {
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        result = inner
            .memory_activation_source
            .activate(seed, candidates, activation_context) => result,
    };
    if token.is_cancelled() {
        trace_provider_step_cancelled();
        let _ = send_cancelled_event(inner, sender).await;
        return;
    }

    let activated_memories = match activation_result {
        Ok(memories) => memories,
        Err(error) => {
            let diagnostic = diagnostic_from_text("memory_activation", error.to_string());
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };
    tracing::debug!(
        category = "activated_memory_count",
        count = activated_memories.len(),
        "runtime memories activated"
    );

    let (snapshot, continuations, activation_epoch) = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            drop(session);
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        session.replace_activated_memories(activated_memories);
        let activation_epoch = inner
            .memory_projection_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let continuations = match session.unconsumed_tool_continuation_snapshots() {
            Ok(continuations) => continuations,
            Err(error) => {
                session.replace_activated_memories(Vec::new());
                inner.memory_projection_epoch.fetch_add(1, Ordering::AcqRel);
                let diagnostic = diagnostic_from_text(
                    "tool_continuation_artifact",
                    format!("tool continuation artifact could not be read: {error}"),
                );
                drop(session);
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        };
        (session.context_snapshot(), continuations, activation_epoch)
    };
    let mut projection_guard =
        ActivationProjectionGuard::new(Arc::clone(inner), token.clone(), activation_epoch);
    let sent_continuation_count = continuations.len();
    let tool_specs = inner.tool_registry.tool_specs();
    tracing::debug!(
        category = "continuations_and_tools",
        continuation_count = sent_continuation_count,
        tool_spec_count = tool_specs.len(),
        "runtime provider request inputs counted"
    );

    let compiled_context = match ContextCompiler::new().compile(&snapshot) {
        Ok(context) => context,
        Err(error) => {
            clear_current_activated_memories(inner).await;
            let diagnostic = diagnostic_from_text("context_compile", error.to_string());
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let request = match compile_step_model_request(
        &input,
        &provider_config.model,
        &compiled_context,
        &continuations,
        tool_specs,
        generation_config,
    ) {
        Ok(request) => {
            tracing::debug!(
                category = "model_request_compiled",
                "runtime model request compiled"
            );
            request
        }
        Err(error) => {
            clear_current_activated_memories(inner).await;
            let diagnostic = diagnostic_from_text("model_request", error.to_string());
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let stream_context = ModelStreamContext::new(token.clone());
    tracing::debug!(
        category = "provider_setup_start",
        "runtime provider stream setup started"
    );
    let stream_result = tokio::select! {
        biased;
        () = token.cancelled() => {
            clear_current_activated_memories(inner).await;
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        result = provider_config.provider.stream_model(request, stream_context) => result,
    };

    let mut stream = match stream_result {
        Ok(stream) => {
            tracing::debug!(
                category = "provider_setup_success",
                "runtime provider stream setup succeeded"
            );
            stream
        }
        Err(error) => {
            clear_current_activated_memories(inner).await;
            let error_kind = error.kind();
            tracing::debug!(
                category = "provider_setup_error",
                error_kind = ?error_kind,
                "runtime provider stream setup failed"
            );
            if is_cancelled_model_error(&error) {
                trace_provider_step_cancelled();
                let _ = send_cancelled_event(inner, sender).await;
                return;
            }

            let diagnostic = diagnostic_from_model_error(error);
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };
    projection_guard.disarm();

    let mut saw_non_empty_text_delta = false;
    let mut streamed_tool_call: Option<PendingToolCall> = None;

    loop {
        let item = tokio::select! {
            biased;
            () = token.cancelled() => {
                trace_provider_step_cancelled();
                let _ = send_cancelled_event(inner, sender).await;
                return;
            }
            item = stream.next() => item,
        };

        match item {
            Some(Ok(ModelEvent::Started)) => {
                tracing::debug!(category = "started", "runtime model stream event received");
            }
            Some(Ok(ModelEvent::OutputTextDelta { delta })) => {
                if !delta.is_empty() {
                    tracing::trace!(
                        category = "output_text_delta_nonempty",
                        "runtime model stream event received"
                    );
                    saw_non_empty_text_delta = true;
                }
            }
            Some(Ok(ModelEvent::Completed { response })) => {
                tracing::debug!(
                    category = "completed",
                    finish_reason = ?response.finish_reason(),
                    "runtime model stream event received"
                );
                match response.finish_reason() {
                    FinishReason::Stop => {
                        if streamed_tool_call.is_some() {
                            let diagnostic = diagnostic_from_text(
                                DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                                "model requested a tool call before completing with text output",
                            );
                            trace_provider_step_failed(&diagnostic);
                            let _ = send_failed_event(inner, sender, token, diagnostic).await;
                            return;
                        }

                        let [ModelOutput::Text { text }] = response.outputs() else {
                            let diagnostic = diagnostic_from_text(
                                "model_output_unsupported",
                                "model stop output must contain exactly one text item",
                            );
                            trace_provider_step_failed(&diagnostic);
                            let _ = send_failed_event(inner, sender, token, diagnostic).await;
                            return;
                        };

                        if !send_assistant_text_output_completed_events(
                            inner,
                            sender,
                            token,
                            text.clone(),
                            sent_continuation_count,
                        )
                        .await
                        {
                            let _ = send_cancelled_if_requested(inner, sender, token).await;
                        }
                        return;
                    }
                    FinishReason::ToolCalls => {
                        if saw_non_empty_text_delta {
                            let diagnostic = diagnostic_from_text(
                                DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                                "model emitted text before requesting a tool call",
                            );
                            trace_provider_step_failed(&diagnostic);
                            let _ = send_failed_event(inner, sender, token, diagnostic).await;
                            return;
                        }

                        match pending_tool_call_from_outputs(
                            response.outputs(),
                            streamed_tool_call.as_ref(),
                        ) {
                            Ok(call) => {
                                if !send_tool_call_pending_event(
                                    inner,
                                    sender,
                                    token,
                                    call,
                                    sent_continuation_count,
                                )
                                .await
                                {
                                    let _ = send_cancelled_if_requested(inner, sender, token).await;
                                }
                            }
                            Err(diagnostic) => {
                                trace_provider_step_failed(&diagnostic);
                                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                            }
                        }
                        return;
                    }
                    FinishReason::Length => {
                        let diagnostic = diagnostic_from_text(
                            "model_length",
                            "model output stopped because it reached a length limit",
                        );
                        trace_provider_step_failed(&diagnostic);
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }
                    FinishReason::Cancelled => {
                        trace_provider_step_cancelled();
                        let _ = send_cancelled_event(inner, sender).await;
                        return;
                    }
                    FinishReason::Error => {
                        let diagnostic = diagnostic_from_text(
                            "model_finish_error",
                            "model output stopped because the provider reported a finish error",
                        );
                        trace_provider_step_failed(&diagnostic);
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }
                }
            }
            Some(Ok(ModelEvent::ToolCallRequested { call })) => {
                tracing::debug!(
                    category = "tool_call_requested",
                    "runtime model stream event received"
                );
                if saw_non_empty_text_delta {
                    let diagnostic = diagnostic_from_text(
                        DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                        "model emitted text before requesting a tool call",
                    );
                    trace_provider_step_failed(&diagnostic);
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }

                match pending_tool_call_from_model(&call)
                    .and_then(|call| record_streamed_tool_call(&mut streamed_tool_call, call))
                {
                    Ok(call) => {
                        streamed_tool_call = Some(call);
                    }
                    Err(diagnostic) => {
                        trace_provider_step_failed(&diagnostic);
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }
                }
            }
            Some(Err(error)) => {
                let error_kind = error.kind();
                tracing::debug!(
                    category = "provider_error",
                    error_kind = ?error_kind,
                    "runtime model stream event received"
                );
                if is_cancelled_model_error(&error) {
                    trace_provider_step_cancelled();
                    let _ = send_cancelled_event(inner, sender).await;
                    return;
                }

                let diagnostic = diagnostic_from_model_error(error);
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
            None => {
                tracing::debug!(category = "eof", "runtime model stream ended");
                let diagnostic = diagnostic_from_text(
                    "model_stream_eof",
                    "model stream ended before completion",
                );
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        }
    }
}

async fn clear_current_activated_memories(inner: &RuntimeInner) {
    let mut session = inner.session.lock().await;
    session.replace_activated_memories(Vec::new());
    inner.memory_projection_epoch.fetch_add(1, Ordering::AcqRel);
}

async fn has_unresolved_pending_tool_calls(inner: &RuntimeInner) -> bool {
    let session = inner.session.lock().await;
    session.has_pending_tool_calls()
}

/// Clears pre-commit memory activation if the producer is aborted before the
/// provider has returned an event stream.
struct ActivationProjectionGuard {
    inner: Arc<RuntimeInner>,
    token: CancellationToken,
    epoch: u64,
    armed: bool,
}

impl ActivationProjectionGuard {
    fn new(inner: Arc<RuntimeInner>, token: CancellationToken, epoch: u64) -> Self {
        Self {
            inner,
            token,
            epoch,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ActivationProjectionGuard {
    fn drop(&mut self) {
        if !self.armed || !self.token.is_cancelled() {
            return;
        }

        if self.inner.memory_projection_epoch.load(Ordering::Acquire) != self.epoch {
            return;
        }

        if clear_activated_memories_if_epoch_matches(&self.inner, self.epoch) {
            return;
        }

        let inner = Arc::clone(&self.inner);
        let epoch = self.epoch;
        tokio::spawn(async move {
            if inner.memory_projection_epoch.load(Ordering::Acquire) != epoch {
                return;
            }

            let mut session = inner.session.lock().await;
            if inner
                .memory_projection_epoch
                .compare_exchange(epoch, epoch + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                session.replace_activated_memories(Vec::new());
            }
        });
    }
}

fn clear_activated_memories_if_epoch_matches(inner: &RuntimeInner, epoch: u64) -> bool {
    let Ok(mut session) = inner.session.try_lock() else {
        return false;
    };

    if inner
        .memory_projection_epoch
        .compare_exchange(epoch, epoch + 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        session.replace_activated_memories(Vec::new());
    }

    true
}

fn memory_activation_seed_from_step_input(
    input: &StepInput,
) -> Result<MemoryActivationSeed, crate::memory::MemoryError> {
    MemoryActivationSeed::new(
        input.text(),
        vec![MemoryScope::Session, MemoryScope::Task, MemoryScope::Step],
        MemoryActivationSourceKind::UserQuery,
        "step input",
    )
}

async fn send_assistant_text_output_completed_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    text: String,
    sent_continuation_count: usize,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(artifact_permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let artifact_event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        match session.record_assistant_text_output(text) {
            Ok(event) => {
                session.consume_tool_continuations(sent_continuation_count);
                Ok(event)
            }
            Err(error) => Err(error),
        }
    };

    let Ok(artifact_event) = artifact_event else {
        drop(artifact_permit);
        let diagnostic = diagnostic_from_text(
            "assistant_output_artifact",
            "assistant output artifact could not be recorded",
        );
        let _ = send_failed_event(inner, sender, token, diagnostic).await;
        return false;
    };

    artifact_permit.send(artifact_event);

    if token.is_cancelled() {
        let _ = send_cancelled_event(inner, sender).await;
        return false;
    }

    let Some(completed_permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let completed_event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            drop(completed_permit);
            let _ = send_cancelled_event(inner, sender).await;
            return false;
        }
        session.record_step_completed()
    };

    completed_permit.send(completed_event);
    true
}

async fn send_tool_call_pending_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    call: PendingToolCall,
    sent_continuation_count: usize,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        match session.record_tool_call_pending(call) {
            Ok(event) => {
                session.consume_tool_continuations(sent_continuation_count);
                Ok(event)
            }
            Err(diagnostic) => Err(diagnostic),
        }
    };

    match event {
        Ok(event) => {
            permit.send(event);
            true
        }
        Err(diagnostic) => {
            drop(permit);
            send_failed_event(inner, sender, token, diagnostic).await
        }
    }
}

async fn send_failed_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    diagnostic: ErrorInfo,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    send_normal_event(inner, sender, token, |session| {
        Some(session.record_failed(diagnostic))
    })
    .await
}

fn record_streamed_tool_call(
    streamed_tool_call: &mut Option<PendingToolCall>,
    call: PendingToolCall,
) -> Result<PendingToolCall, ErrorInfo> {
    match streamed_tool_call.as_ref() {
        Some(existing) if existing == &call => Ok(existing.clone()),
        Some(_) => Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_PARALLEL_TOOL_CALLS_UNSUPPORTED,
            "model requested multiple streamed tool calls, but runtime policy only supports one pending tool call",
        )),
        None => Ok(call),
    }
}

fn pending_tool_call_from_outputs(
    outputs: &[ModelOutput],
    streamed_tool_call: Option<&PendingToolCall>,
) -> Result<PendingToolCall, ErrorInfo> {
    if outputs.is_empty() {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MISSING,
            "model finished with tool calls but returned no tool call output",
        ));
    }

    let tool_call_count = outputs
        .iter()
        .filter(|output| matches!(output, ModelOutput::ToolCall { .. }))
        .count();
    let text_output_count = outputs
        .iter()
        .filter(|output| matches!(output, ModelOutput::Text { .. }))
        .count();

    if tool_call_count == 0 {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MISSING,
            "model finished with tool calls but returned no tool call output",
        ));
    }

    if tool_call_count > 1 {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_PARALLEL_TOOL_CALLS_UNSUPPORTED,
            "model returned multiple tool calls, but runtime policy only supports one pending tool call",
        ));
    }

    if text_output_count > 0 || outputs.len() != 1 {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
            "model returned text and a tool call in the same response",
        ));
    }

    let [ModelOutput::ToolCall { call }] = outputs else {
        return Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_TOOL_CALL_MISSING,
            "model finished with tool calls but returned no tool call output",
        ));
    };

    let completed_call = pending_tool_call_from_model(call)?;
    match streamed_tool_call {
        Some(streamed_call) if streamed_call == &completed_call => Ok(streamed_call.clone()),
        Some(_) => Err(diagnostic_from_text(
            DIAGNOSTIC_MODEL_PARALLEL_TOOL_CALLS_UNSUPPORTED,
            "model completed with a different tool call than the streamed pending call",
        )),
        None => Ok(completed_call),
    }
}

fn pending_tool_call_from_model(call: &ModelToolCall) -> Result<PendingToolCall, ErrorInfo> {
    let id = ToolCallId::new(call.id().as_str()).map_err(tool_call_conversion_diagnostic)?;
    let arguments = ToolCallArguments::new(call.arguments().as_object().clone());
    Ok(PendingToolCall::new(id, call.name().clone(), arguments))
}

fn tool_call_conversion_diagnostic(error: CoreError) -> ErrorInfo {
    diagnostic_from_text(
        DIAGNOSTIC_MODEL_TOOL_CALL_INVALID,
        format!("model tool call could not be normalized: {error}"),
    )
}

fn is_cancelled_model_error(error: &ModelError) -> bool {
    error.kind() == ProviderErrorKind::Cancelled
}

fn diagnostic_from_model_error(error: ModelError) -> ErrorInfo {
    let code = match error.kind() {
        ProviderErrorKind::InvalidRequest => "model_invalid_request",
        ProviderErrorKind::Cancelled => "cancelled",
        ProviderErrorKind::Authentication => "model_authentication",
        ProviderErrorKind::RateLimited => "model_rate_limited",
        ProviderErrorKind::Unavailable => "model_unavailable",
        ProviderErrorKind::Protocol => "model_protocol",
        ProviderErrorKind::Other => "model_other",
    };

    diagnostic_from_text(code, error.to_string())
}

fn trace_provider_step_failed(diagnostic: &ErrorInfo) {
    tracing::debug!(
        category = "failed",
        diagnostic_code = diagnostic.code(),
        "runtime provider step failed"
    );
}

fn trace_provider_step_cancelled() {
    tracing::debug!(
        category = "cancelled",
        diagnostic_code = "cancelled",
        "runtime provider step cancelled"
    );
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

async fn send_normal_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    make_event: impl FnOnce(&mut SessionState) -> Option<RuntimeEvent>,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        make_event(&mut session)
    };

    if let Some(event) = event {
        permit.send(event);
    }

    true
}

async fn reserve_normal_event_slot<'a>(
    sender: &'a mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
) -> Option<Permit<'a, RuntimeEvent>> {
    if token.is_cancelled() || sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = token.cancelled() => None,
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}

async fn send_cancelled_if_requested(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
) -> bool {
    if !token.is_cancelled() {
        return false;
    }

    send_cancelled_event(inner, sender).await
}

async fn send_cancelled_event(inner: &RuntimeInner, sender: &mpsc::Sender<RuntimeEvent>) -> bool {
    let Some(permit) = reserve_cancelled_event_slot(sender).await else {
        return false;
    };

    if sender.is_closed() {
        return false;
    }

    let diagnostic = ErrorInfo::new("cancelled", "runtime step cancelled")
        .expect("static cancellation diagnostic is valid");
    let event = {
        let mut session = inner.session.lock().await;
        session.record_cancelled(diagnostic)
    };
    permit.send(event);
    true
}

async fn reserve_cancelled_event_slot<'a>(
    sender: &'a mpsc::Sender<RuntimeEvent>,
) -> Option<Permit<'a, RuntimeEvent>> {
    if sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED, Runtime, RuntimeInner,
        memory_activation_seed_from_step_input, send_cancelled_event,
    };
    use crate::ArtifactError;
    use crate::artifact::ArtifactContent;
    use crate::judgment::{
        JudgmentConfidence, JudgmentContext, JudgmentError, JudgmentEvidence, JudgmentFuture,
        JudgmentOutcome, JudgmentProvenance, JudgmentPurpose, JudgmentRecommendation,
        JudgmentRecord, JudgmentRiskLevel, JudgmentSource, JudgmentSourceKind,
        ModelBackedJudgmentSource,
    };
    use crate::memory::{
        ActivatedMemory, MemoryActivationContext, MemoryActivationFuture, MemoryActivationReason,
        MemoryActivationScore, MemoryActivationSource, MemoryActivationSourceKind, MemoryError,
        MemoryEvidence, MemoryId, MemoryItem, MemoryItemSelection, MemoryScope,
    };
    use crate::session::SessionState;
    use crate::tool::{
        RegisteredTool, ToolExecutionContext, ToolExecutionOutcome, ToolExecutor,
        ToolExecutorFuture, ToolRegistry,
    };
    use futures_util::StreamExt;
    use merry_core::{
        ArtifactId, ArtifactKind, ArtifactRef, EvidenceLocator, EvidenceRef, PendingToolCall,
        RuntimeEvent, RuntimeEventKind, SessionId, ToolCallArguments, ToolCallId, ToolInputSchema,
        ToolName, ToolSpec,
    };
    use merry_llm::{
        FinishReason, ModelCapabilities, ModelError, ModelEvent, ModelEventStream,
        ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelProviderFuture, ModelRequest,
        ModelResponse, ModelStreamContext, ModelToolCall, ModelToolCallId, ProviderErrorKind,
        ToolArguments,
    };
    use schemars::Schema;
    use serde_json::json;
    use std::{
        num::NonZeroUsize,
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        },
    };
    use tokio::sync::{Mutex, mpsc, oneshot};
    use tokio_util::sync::CancellationToken;

    fn runtime_inner() -> RuntimeInner {
        let session_id = SessionId::new("runtime-send-test").expect("valid session id");
        RuntimeInner {
            session_id: session_id.clone(),
            session: Mutex::new(SessionState::new(session_id)),
            active_step: Arc::new(AtomicBool::new(false)),
            memory_projection_epoch: AtomicU64::new(0),
            event_buffer_size: NonZeroUsize::new(1).expect("non-zero buffer"),
            model_provider: None,
            tool_registry: ToolRegistry::default(),
            memory_activation_source: Arc::new(crate::memory::StoredMemoryActivationSource),
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

    fn event_kind_names(events: &[RuntimeEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|event| match event.kind {
                RuntimeEventKind::SessionStarted => "SessionStarted",
                RuntimeEventKind::StepStarted => "StepStarted",
                RuntimeEventKind::StepCompleted => "StepCompleted",
                RuntimeEventKind::Cancelled { .. } => "Cancelled",
                RuntimeEventKind::Failed { .. } => "Failed",
                RuntimeEventKind::ArtifactRecorded { .. } => "ArtifactRecorded",
                RuntimeEventKind::EvidenceReferenced { .. } => "EvidenceReferenced",
                RuntimeEventKind::ToolCallPending { .. } => "ToolCallPending",
                RuntimeEventKind::ToolCallResolved { .. } => "ToolCallResolved",
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
                model_provider: Some(super::ModelProviderConfig {
                    provider: Arc::new(provider),
                    model: model_name(),
                }),
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(source),
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
                model_provider: Some(super::ModelProviderConfig {
                    provider: Arc::new(provider),
                    model: model_name(),
                }),
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(crate::memory::StoredMemoryActivationSource),
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
                model_provider: None,
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(source),
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
        assert_eq!(requests[0].messages().len(), 2);
        assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::System);
        assert_eq!(requests[0].messages()[1].role(), ModelMessageRole::User);
        assert!(
            requests[0].messages()[0]
                .content()
                .as_text()
                .contains("memory:memory-topic")
        );
        assert!(
            requests[0].messages()[0]
                .content()
                .as_text()
                .contains("memory-text:Remember that topic answers should mention runtime timing.")
        );
        assert_eq!(
            requests[0].messages()[1].content().as_text(),
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
        assert_eq!(requests[0].messages().len(), 1);
        assert_eq!(requests[0].messages()[0].role(), ModelMessageRole::User);
        assert_eq!(
            requests[0].messages()[0].content().as_text(),
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
        assert_eq!(requests[0].messages().len(), 2);
        assert!(
            requests[0].messages()[0]
                .content()
                .as_text()
                .contains("memory:memory-stale")
        );
        assert_eq!(requests[1].messages().len(), 1);
        assert_eq!(requests[1].messages()[0].role(), ModelMessageRole::User);
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
            .register_tool(RegisteredTool::new(tool_spec, Arc::new(executor.clone())))
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
