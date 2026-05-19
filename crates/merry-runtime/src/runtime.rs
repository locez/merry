//! Runtime builder and step execution skeleton.
//!
//! [`Runtime`] is the MVP facade for session-owned state. Step execution and
//! event-producing direct mutation APIs admit one active operation at a time,
//! record durable session state before returning observable events, and keep
//! provider wire details behind the `merry-llm` provider boundary.

use crate::{
    ArtifactContent, ContextCompiler, ContextEntry, ContextSummary, LedgerProjectionSnapshot,
    RuntimeError, RuntimeEventStream, SessionContextSnapshot,
    event_stream::ActiveStepPermit,
    memory::{
        MemoryActivationSeed, MemoryActivationSource, MemoryActivationSourceKind, MemoryScope,
        NoopMemoryActivationSource,
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
    sync::{Arc, atomic::AtomicBool},
};
use tokio::sync::{Mutex, mpsc, mpsc::Permit};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

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
/// and event-producing direct mutation APIs such as
/// [`Runtime::record_artifact`], [`Runtime::submit_tool_result`], and
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
    /// Only one step or event-producing direct mutation may own the runtime at
    /// a time. The step producer owns the active-step permit. Dropping the
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

        let (parent_token, generation_config) = context.into_parts();
        let step_token = parent_token.child_token();
        let producer_token = step_token.clone();
        let (sender, receiver) = mpsc::channel(self.inner.event_buffer_size.get());
        let inner = Arc::clone(&self.inner);

        let producer_handle = tokio::spawn(async move {
            run_step(
                inner,
                sender,
                producer_token,
                input,
                generation_config,
                active_permit,
            )
            .await;
        });

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
    /// This is the current MVP context mutation surface. It records
    /// summary-only context entries today by taking the session lock. It does
    /// not acquire the active-step permit and does not emit runtime events.
    /// This surface may expand when Memory Activation becomes part of the
    /// runtime state model.
    pub async fn record_context_entry(&self, entry: ContextEntry) {
        let mut session = self.inner.session.lock().await;
        session.record_context_entry(entry);
    }

    /// Records a summary context entry into the owning session.
    ///
    /// Summaries are navigation only; exact supporting evidence must remain
    /// readable through session-owned artifacts. This helper delegates to
    /// [`Runtime::record_context_entry`], so it takes the session lock and does
    /// not acquire the active-step permit or emit runtime events.
    pub async fn record_context_summary(&self, summary: ContextSummary) {
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
            memory_activation_source: Arc::new(NoopMemoryActivationSource),
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
    if token.is_cancelled() {
        let _ = send_cancelled_event(&inner, &sender).await;
        return;
    }

    if !send_normal_event(&inner, &sender, &token, |session| {
        session.record_session_started_if_needed()
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
        if !send_normal_event(&inner, &sender, &token, |session| {
            Some(session.record_step_completed())
        })
        .await
        {
            let _ = send_cancelled_if_requested(&inner, &sender, &token).await;
        }
        return;
    };

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
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    input: StepInput,
    generation_config: GenerationConfig,
    provider_config: ModelProviderConfig,
) {
    if has_unresolved_pending_tool_calls(inner).await {
        let diagnostic = diagnostic_from_text(
            DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
            "a pending tool call must be resolved before the next provider step",
        );
        let _ = send_failed_event(inner, sender, token, diagnostic).await;
        return;
    }

    clear_current_activated_memories(inner).await;

    if token.is_cancelled() {
        let _ = send_cancelled_event(inner, sender).await;
        return;
    }

    let seed = match memory_activation_seed_from_step_input(&input) {
        Ok(seed) => seed,
        Err(error) => {
            let diagnostic = diagnostic_from_text("memory_activation", error.to_string());
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };
    let activated_memories = match inner.memory_activation_source.activate(&seed) {
        Ok(memories) => memories,
        Err(error) => {
            let diagnostic = diagnostic_from_text("memory_activation", error.to_string());
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let (snapshot, continuations) = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            drop(session);
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        session.replace_activated_memories(activated_memories);
        let continuations = match session.unconsumed_tool_continuation_snapshots() {
            Ok(continuations) => continuations,
            Err(error) => {
                session.replace_activated_memories(Vec::new());
                let diagnostic = diagnostic_from_text(
                    "tool_continuation_artifact",
                    format!("tool continuation artifact could not be read: {error}"),
                );
                drop(session);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        };
        (session.context_snapshot(), continuations)
    };
    let sent_continuation_count = continuations.len();
    let tool_specs = inner.tool_registry.tool_specs();

    let compiled_context = match ContextCompiler::new().compile(&snapshot) {
        Ok(context) => context,
        Err(error) => {
            clear_current_activated_memories(inner).await;
            let diagnostic = diagnostic_from_text("context_compile", error.to_string());
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
        Ok(request) => request,
        Err(error) => {
            clear_current_activated_memories(inner).await;
            let diagnostic = diagnostic_from_text("model_request", error.to_string());
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let stream_context = ModelStreamContext::new(token.clone());
    let stream_result = tokio::select! {
        biased;
        () = token.cancelled() => {
            clear_current_activated_memories(inner).await;
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        result = provider_config.provider.stream_model(request, stream_context) => result,
    };

    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            clear_current_activated_memories(inner).await;
            if is_cancelled_model_error(&error) {
                let _ = send_cancelled_event(inner, sender).await;
                return;
            }

            let diagnostic = diagnostic_from_model_error(error);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let mut saw_non_empty_text_delta = false;
    let mut streamed_tool_call: Option<PendingToolCall> = None;

    loop {
        let item = tokio::select! {
            biased;
            () = token.cancelled() => {
                let _ = send_cancelled_event(inner, sender).await;
                return;
            }
            item = stream.next() => item,
        };

        match item {
            Some(Ok(ModelEvent::Started)) => {}
            Some(Ok(ModelEvent::OutputTextDelta { delta })) => {
                if !delta.is_empty() {
                    saw_non_empty_text_delta = true;
                }
            }
            Some(Ok(ModelEvent::Completed { response })) => match response.finish_reason() {
                FinishReason::Stop => {
                    if streamed_tool_call.is_some() {
                        let diagnostic = diagnostic_from_text(
                            DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                            "model requested a tool call before completing with text output",
                        );
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }

                    let [ModelOutput::Text { text }] = response.outputs() else {
                        let diagnostic = diagnostic_from_text(
                            "model_output_unsupported",
                            "model stop output must contain exactly one text item",
                        );
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
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
                FinishReason::Cancelled => {
                    let _ = send_cancelled_event(inner, sender).await;
                    return;
                }
                FinishReason::Error => {
                    let diagnostic = diagnostic_from_text(
                        "model_finish_error",
                        "model output stopped because the provider reported a finish error",
                    );
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
            },
            Some(Ok(ModelEvent::ToolCallRequested { call })) => {
                if saw_non_empty_text_delta {
                    let diagnostic = diagnostic_from_text(
                        DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                        "model emitted text before requesting a tool call",
                    );
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
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }
                }
            }
            Some(Err(error)) => {
                if is_cancelled_model_error(&error) {
                    let _ = send_cancelled_event(inner, sender).await;
                    return;
                }

                let diagnostic = diagnostic_from_model_error(error);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
            None => {
                let diagnostic = diagnostic_from_text(
                    "model_stream_eof",
                    "model stream ended before completion",
                );
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        }
    }
}

async fn clear_current_activated_memories(inner: &RuntimeInner) {
    let mut session = inner.session.lock().await;
    session.replace_activated_memories(Vec::new());
}

async fn has_unresolved_pending_tool_calls(inner: &RuntimeInner) -> bool {
    let session = inner.session.lock().await;
    session.has_pending_tool_calls()
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
    use crate::memory::{
        ActivatedMemory, MemoryActivationReason, MemoryActivationScore, MemoryActivationSource,
        MemoryActivationSourceKind, MemoryError, MemoryEvidence, MemoryId, MemoryItem,
        MemoryItemSelection, MemoryScope,
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
        ModelResponse, ModelStreamContext,
    };
    use schemars::Schema;
    use serde_json::json;
    use std::{
        num::NonZeroUsize,
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
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
            event_buffer_size: NonZeroUsize::new(1).expect("non-zero buffer"),
            model_provider: None,
            tool_registry: ToolRegistry::default(),
            memory_activation_source: Arc::new(crate::memory::NoopMemoryActivationSource),
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

    fn activated_memory(id: &str, text: &str, evidence_artifact: &str) -> ActivatedMemory {
        let item = MemoryItem::new(
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
            MemoryItemSelection::new(vec!["topic".to_owned()], 0.8, 1, None)
                .expect("valid memory selection"),
        )
        .expect("valid memory item");
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

    #[derive(Debug, Clone)]
    enum ScriptedMemoryActivationResponse {
        Memories(Vec<ActivatedMemory>),
        Error(MemoryError),
        CancelThenMemories {
            token: CancellationToken,
            memories: Vec<ActivatedMemory>,
        },
    }

    impl ScriptedMemoryActivationResponse {
        fn into_result(self) -> Result<Vec<ActivatedMemory>, MemoryError> {
            match self {
                Self::Memories(memories) => Ok(memories),
                Self::Error(error) => Err(error),
                Self::CancelThenMemories { token, memories } => {
                    token.cancel();
                    Ok(memories)
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

        fn observed_queries(&self) -> Vec<String> {
            self.observed_queries
                .lock()
                .expect("observed query mutex should not be poisoned")
                .clone()
        }
    }

    impl MemoryActivationSource for ScriptedMemoryActivationSource {
        fn activate(
            &self,
            seed: &crate::memory::MemoryActivationSeed,
        ) -> Result<Vec<ActivatedMemory>, MemoryError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.observed_queries
                .lock()
                .expect("observed query mutex should not be poisoned")
                .push(seed.query().to_owned());
            self.responses
                .lock()
                .expect("memory response mutex should not be poisoned")
                .pop()
                .map_or(
                    Ok(Vec::new()),
                    ScriptedMemoryActivationResponse::into_result,
                )
        }
    }

    #[derive(Debug, Clone)]
    struct RecordingModelProvider {
        requests: Arc<StdMutex<Vec<ModelRequest>>>,
        calls: Arc<AtomicUsize>,
    }

    impl RecordingModelProvider {
        fn new() -> Self {
            Self {
                requests: Arc::new(StdMutex::new(Vec::new())),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn recorded_requests(&self) -> Vec<ModelRequest> {
            self.requests
                .lock()
                .expect("recorded requests mutex should not be poisoned")
                .clone()
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
                let stream: ModelEventStream =
                    Box::pin(futures_util::stream::iter(vec![Ok(completed_event())]));
                Ok(stream)
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

    fn runtime_without_provider_with_memory_source<S>(session: &str, source: S) -> Runtime
    where
        S: MemoryActivationSource + 'static,
    {
        Runtime {
            inner: Arc::new(RuntimeInner {
                session_id: session_id(session),
                session: Mutex::new(SessionState::new(session_id(session))),
                active_step: Arc::new(AtomicBool::new(false)),
                event_buffer_size: NonZeroUsize::new(16).expect("non-zero buffer"),
                model_provider: None,
                tool_registry: ToolRegistry::default(),
                memory_activation_source: Arc::new(source),
            }),
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
    async fn provider_step_projects_activated_memory_before_user_message() {
        let memory = activated_memory(
            "memory-topic",
            "Remember that topic answers should mention runtime timing.",
            "memory-topic-artifact",
        );
        let source = ScriptedMemoryActivationSource::new(vec![vec![memory]]);
        let provider = RecordingModelProvider::new();
        let runtime = runtime_with_provider_and_memory_source(
            "runtime-memory-context",
            provider.clone(),
            source.clone(),
        );
        record_memory_artifact(
            &runtime,
            "memory-topic-artifact",
            "exact evidence for timing memory",
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
        assert_eq!(source.observed_queries(), ["topic request."]);

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
