//! High-level Rust semantic SDK for Merry.
//!
//! This module composes the runtime-owned loop and interactive handles into a
//! small application-facing API. Runtime state, tool admission, persistence,
//! and lifecycle remain owned by `merry-runtime`.

use crate::{
    errors::{AgentBuildError, AgentError},
    interactive::InteractiveRun,
    profile::{AgentProfile, AgentProfileContext},
    providers::{ConfiguredModelProvider, RuntimeBuilderProviderExt},
    stream::{
        AgentEventStream, StructuredAgentEventStream, StructuredAgentRun, StructuredRunResult,
    },
    tools::Tool,
};
use merry_core::{
    ErrorInfo, PendingToolCall, RuntimeEvent, SessionId, ToolCallArguments, ToolCallBatchId,
    ToolCallId, ToolInputSchema, ToolName,
};
use merry_llm::{GenerationConfig, ModelName, ModelProvider};
use merry_runtime::{
    AgentLoopConfig, AgentRun as RuntimeAgentRun, FileSessionStore, FinalOutputContract, Runtime,
    RuntimeBuilder, RuntimeModelRole, RuntimeProfile, StepContext, StepInput,
    StructuredOutputRetryPolicy, ToolExecutionOutcome,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use std::{collections::BTreeMap, fmt, num::NonZeroUsize, sync::Arc};
use thiserror::Error;

pub use crate::run_result::RunResult;

/// Builder for a high-level semantic Merry agent.
///
/// The builder owns only application-facing composition. The resulting
/// [`Agent`] delegates execution and state to [`Runtime`]. A primary provider
/// must be configured before [`Self::build`] or [`Self::resume_from_store`].
pub struct AgentBuilder {
    runtime_builder: RuntimeBuilder,
    loop_config: AgentLoopConfig,
    generation_config: GenerationConfig,
    has_primary_provider: bool,
    profile: Option<Arc<dyn AgentProfile>>,
}

impl AgentBuilder {
    /// Creates a builder for a new or persisted session.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            runtime_builder: Runtime::builder(session_id),
            loop_config: AgentLoopConfig::default(),
            generation_config: GenerationConfig::default(),
            has_primary_provider: false,
            profile: None,
        }
    }

    /// Installs a provider-neutral configured provider component.
    #[must_use]
    pub fn provider(mut self, provider: ConfiguredModelProvider) -> Self {
        self.has_primary_provider |= provider.role() == RuntimeModelRole::Primary;
        self.runtime_builder = self.runtime_builder.with_provider(provider);
        self
    }

    /// Installs a primary provider and model directly through Merry's provider trait.
    #[must_use]
    pub fn model_provider(mut self, provider: Arc<dyn ModelProvider>, model: ModelName) -> Self {
        self.runtime_builder = self.runtime_builder.model_provider(provider, model);
        self.has_primary_provider = true;
        self
    }

    /// Applies a low-level provider-neutral runtime profile directly.
    ///
    /// This is the runtime escape hatch. Use [`Self::profile`] for an
    /// application-level profile that also owns the agent loop policy.
    pub fn runtime_profile(mut self, profile: RuntimeProfile) -> Result<Self, AgentBuildError> {
        self.runtime_builder = self.runtime_builder.with_profile(profile)?;
        Ok(self)
    }

    /// Applies an application-level agent profile and its loop policy.
    pub fn profile<P>(mut self, profile: P) -> Result<Self, AgentBuildError>
    where
        P: AgentProfile + 'static,
    {
        let mut context = AgentProfileContext::new(self.runtime_builder, self.loop_config);
        profile.configure(&mut context)?;
        let (runtime_builder, loop_config) = context.into_parts()?;
        self.runtime_builder = runtime_builder;
        self.loop_config = loop_config;
        self.profile = Some(Arc::new(profile));
        Ok(self)
    }

    /// Overrides the bounded model-turn policy used by normal runs.
    #[must_use]
    pub fn loop_config(mut self, config: AgentLoopConfig) -> Self {
        self.loop_config = config;
        self
    }

    /// Sets the number of model continuations allowed after a structured
    /// output decoder rejects the model's final-output call.
    #[must_use]
    pub fn structured_output_retry_policy(mut self, policy: StructuredOutputRetryPolicy) -> Self {
        self.loop_config = self
            .loop_config
            .clone()
            .with_structured_output_retry_policy(policy);
        self
    }

    /// Sets provider-neutral model generation controls.
    #[must_use]
    pub fn generation_config(mut self, config: GenerationConfig) -> Self {
        self.generation_config = config;
        self
    }

    /// Registers one typed application tool.
    #[must_use]
    pub fn tool(mut self, tool: Tool) -> Self {
        self.runtime_builder = self
            .runtime_builder
            .register_tool(tool.into_registered_tool());
        self
    }

    /// Registers one low-level runtime or host integration tool.
    #[doc(hidden)]
    #[must_use]
    pub fn register_tool(mut self, tool: merry_runtime::RegisteredTool) -> Self {
        self.runtime_builder = self.runtime_builder.register_tool(tool);
        self
    }

    /// Registers low-level runtime or host integration tools in the supplied order.
    #[doc(hidden)]
    #[must_use]
    pub fn register_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = merry_runtime::RegisteredTool>,
    {
        for tool in tools {
            self = self.register_tool(tool);
        }
        self
    }

    /// Enables explicit host-side bridge tool execution for this agent.
    #[doc(hidden)]
    #[must_use]
    pub fn allow_bridge_tools(mut self) -> Self {
        self.runtime_builder = self.runtime_builder.allow_bridge_tools();
        self
    }

    /// Sets the store used by explicit and automatic session savepoints.
    #[must_use]
    pub fn session_store(mut self, store: FileSessionStore) -> Self {
        self.runtime_builder = self.runtime_builder.session_store(store);
        self
    }

    /// Sets the bounded runtime event buffer capacity.
    #[must_use]
    pub fn event_buffer_size(mut self, size: NonZeroUsize) -> Self {
        self.runtime_builder = self.runtime_builder.event_buffer_size(size);
        self
    }

    /// Builds a new agent with the configured runtime state.
    pub fn build(self) -> Result<Agent, AgentBuildError> {
        if !self.has_primary_provider {
            return Err(AgentBuildError::MissingPrimaryProvider);
        }
        let Self {
            runtime_builder,
            loop_config,
            generation_config,
            profile,
            ..
        } = self;
        let runtime = runtime_builder.build()?;
        Ok(Agent {
            runtime,
            loop_config,
            generation_config,
            profile,
        })
    }

    /// Resumes this agent's session from an explicit file-backed store.
    pub async fn resume_from_store(
        self,
        store: FileSessionStore,
    ) -> Result<Agent, AgentBuildError> {
        if !self.has_primary_provider {
            return Err(AgentBuildError::MissingPrimaryProvider);
        }
        let Self {
            runtime_builder,
            loop_config,
            generation_config,
            profile,
            ..
        } = self;
        let runtime = runtime_builder.resume_from_store(store).await?;
        Ok(Agent {
            runtime,
            loop_config,
            generation_config,
            profile,
        })
    }
}

/// High-level semantic agent handle for one session.
///
/// Clones share the same runtime-owned session. The runtime admits one active
/// run or direct mutation per session at a time.
#[derive(Clone)]
pub struct Agent {
    runtime: Runtime,
    loop_config: AgentLoopConfig,
    generation_config: GenerationConfig,
    profile: Option<Arc<dyn AgentProfile>>,
}

impl Agent {
    /// Returns the session identity owned by this agent.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        self.runtime.session_id()
    }

    /// Returns the configured bounded loop policy.
    #[must_use]
    pub fn loop_config(&self) -> &AgentLoopConfig {
        &self.loop_config
    }

    /// Returns the configured application-level profile when one was installed.
    #[must_use]
    pub fn profile(&self) -> Option<&dyn AgentProfile> {
        self.profile.as_deref()
    }

    /// Runs one task to a terminal, blocked, failed, or cancelled result.
    pub async fn run(&self, task: &str) -> Result<RunResult, AgentError> {
        self.run_with_config(task, self.loop_config.clone()).await
    }

    /// Runs one task and decodes its runtime-recorded final output as `T`.
    ///
    /// `T` must produce an object-shaped JSON Schema. Merry validates that
    /// contract before starting the model loop; scalar and array output types
    /// return [`AgentError::FinalOutputContract`] immediately.
    pub async fn run_structured<T>(&self, task: &str) -> Result<StructuredRunResult<T>, AgentError>
    where
        T: DeserializeOwned + JsonSchema,
    {
        let config = self.structured_loop_config::<T>()?;
        let result = self.run_with_config(task, config).await?;
        StructuredRunResult::from_run(result)
    }

    /// Starts an event-only stream for one non-interactive task.
    ///
    /// Registered Rust tools are executed by runtime and are therefore not
    /// exposed as invocation messages. A host that deliberately owns tool
    /// callables can use [`Self::stream_with_tool_handoff`] instead.
    pub fn stream(&self, task: &str) -> Result<AgentEventStream, AgentError> {
        let run = self.stream_with_config(task, self.loop_config.clone())?;
        Ok(AgentEventStream::new(run))
    }

    /// Starts the explicit host-tool handoff stream for one task.
    ///
    /// This is an adapter API for bindings or a Rust host that registered
    /// low-level bridge tools. The caller must submit a complete invocation
    /// batch before requesting the next message.
    #[doc(hidden)]
    pub fn stream_with_tool_handoff(&self, task: &str) -> Result<AgentRun, AgentError> {
        self.stream_with_config(task, self.loop_config.clone())
    }

    /// Starts a live stream whose terminal result is decoded as `T`.
    ///
    /// `T` must produce an object-shaped JSON Schema. The schema is compiled
    /// before the provider is called, so an unsupported shape is reported at
    /// stream construction time.
    pub fn stream_structured<T>(
        &self,
        task: &str,
    ) -> Result<StructuredAgentEventStream<T>, AgentError>
    where
        T: DeserializeOwned + JsonSchema,
    {
        let config = self.structured_loop_config::<T>()?;
        Ok(StructuredAgentEventStream::new(AgentEventStream::new(
            self.stream_with_config(task, config)?,
        )))
    }

    /// Starts a structured-output stream with explicit host-tool handoff.
    #[doc(hidden)]
    pub fn stream_structured_with_tool_handoff<T>(
        &self,
        task: &str,
    ) -> Result<StructuredAgentRun<T>, AgentError>
    where
        T: DeserializeOwned + JsonSchema,
    {
        let config = self.structured_loop_config::<T>()?;
        Ok(StructuredAgentRun::new(
            self.stream_with_config(task, config)?,
        ))
    }

    /// Starts an interactive run with the same runtime event and loop contracts.
    pub fn start_interactive(&self) -> Result<InteractiveRun, AgentError> {
        self.runtime
            .start_interactive_agent_run(self.step_context(), self.loop_config.clone())
            .map(InteractiveRun::new)
            .map_err(AgentError::from)
    }

    /// Saves the current resume-safe session state to its configured store.
    pub async fn save_session(&self) -> Result<(), AgentError> {
        self.runtime.save_session().await.map_err(AgentError::from)
    }

    /// Saves the current resume-safe session state to an explicit store.
    pub async fn save_session_to(&self, store: FileSessionStore) -> Result<(), AgentError> {
        self.runtime
            .save_session_to(store)
            .await
            .map_err(AgentError::from)
    }

    async fn run_with_config(
        &self,
        task: &str,
        config: AgentLoopConfig,
    ) -> Result<RunResult, AgentError> {
        let input = StepInput::user_text(task).map_err(AgentError::from)?;
        let result = self
            .runtime
            .run_agent_loop(input, self.step_context(), config)
            .await
            .map_err(AgentError::from)?;
        RunResult::from_runtime(&self.runtime, result).await
    }

    fn stream_with_config(
        &self,
        task: &str,
        config: AgentLoopConfig,
    ) -> Result<AgentRun, AgentError> {
        let input = StepInput::user_text(task).map_err(AgentError::from)?;
        let stream = self
            .runtime
            .run_agent_loop_stream(input, self.step_context(), config)
            .map_err(AgentError::from)?;
        Ok(AgentRun::new(self.runtime.clone(), stream))
    }

    fn step_context(&self) -> StepContext {
        StepContext::default().with_generation_config(self.generation_config.clone())
    }

    fn structured_loop_config<T>(&self) -> Result<AgentLoopConfig, AgentError>
    where
        T: DeserializeOwned + JsonSchema,
    {
        let schema = ToolInputSchema::new(schemars::schema_for!(T)).map_err(|source| {
            AgentError::FinalOutputContract {
                source: merry_runtime::FinalOutputContractError::Core(source),
            }
        })?;
        let contract = FinalOutputContract::new(schema)
            .map_err(|source| AgentError::FinalOutputContract { source })?
            .with_output_decoder::<T>();
        Ok(self
            .loop_config
            .clone()
            .with_final_output_contract(contract))
    }
}

/// Content returned by an externally executed tool invocation.
///
/// Tool result content is deliberately limited to the content kinds currently
/// accepted by the runtime continuation contract. Binary and image results
/// can be added here only when the runtime can persist and project them as
/// tool results as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationContent {
    kind: ToolInvocationContentKind,
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolInvocationContentKind {
    Text,
    Json,
}

impl ToolInvocationContent {
    /// Creates a text result.
    #[must_use]
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            kind: ToolInvocationContentKind::Text,
            value: content.into(),
        }
    }

    /// Creates a JSON result after validating the serialized payload.
    pub fn json(content: impl Into<String>) -> Result<Self, ToolInvocationContentError> {
        let content = content.into();
        let _: serde_json::Value = serde_json::from_str(&content)
            .map_err(|source| ToolInvocationContentError::InvalidJson { source })?;
        Ok(Self {
            kind: ToolInvocationContentKind::Json,
            value: content,
        })
    }
}

/// Failure while validating tool invocation result content.
#[derive(Debug, Error)]
pub enum ToolInvocationContentError {
    /// The supplied JSON text is not a valid JSON value.
    #[error("tool invocation JSON content is invalid: {source}")]
    InvalidJson {
        /// Underlying JSON parser failure.
        #[source]
        source: serde_json::Error,
    },
}

/// One domain-level result returned for an externally executed invocation.
///
/// The caller supplies only the call id, content and, for a domain failure, a
/// stable diagnostic. The runtime generates the artifact id and
/// `ToolCallResult`, then records the result before continuing the model loop.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolInvocationResult {
    /// The invocation completed successfully.
    Succeeded {
        /// Provider/model-originated call identifier being resolved.
        call_id: ToolCallId,
        /// Exact text or JSON payload returned by the invocation executor.
        content: ToolInvocationContent,
    },
    /// The invocation ran but returned a domain-level failure.
    Failed {
        /// Provider/model-originated call identifier being resolved.
        call_id: ToolCallId,
        /// Exact text or JSON payload returned by the invocation executor.
        content: ToolInvocationContent,
        /// Stable diagnostic for the model-visible failure.
        diagnostic: ErrorInfo,
    },
}

/// Result of submitting one complete host invocation batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolInvocationSubmission {
    /// Every supplied outcome was recorded as submitted.
    Accepted,
    /// Runtime recorded the invocation(s) as failed and can continue the run.
    ///
    /// This is a successful lifecycle transition. The original host result
    /// was rejected, but returning it as an error would make callers cancel a
    /// run that runtime has already recovered.
    RejectedAndRecorded,
}

impl ToolInvocationResult {
    /// Creates a successful invocation result.
    #[must_use]
    pub fn succeeded(call_id: ToolCallId, content: ToolInvocationContent) -> Self {
        Self::Succeeded { call_id, content }
    }

    /// Creates a failed invocation result that the model can inspect.
    #[must_use]
    pub fn failed(
        call_id: ToolCallId,
        content: ToolInvocationContent,
        diagnostic: ErrorInfo,
    ) -> Self {
        Self::Failed {
            call_id,
            content,
            diagnostic,
        }
    }

    /// Returns the provider/model-originated call identifier being resolved.
    #[must_use]
    pub fn call_id(&self) -> &ToolCallId {
        match self {
            Self::Succeeded { call_id, .. } | Self::Failed { call_id, .. } => call_id,
        }
    }

    pub(crate) fn into_runtime(self) -> (ToolCallId, ToolExecutionOutcome) {
        match self {
            Self::Succeeded { call_id, content } => (call_id, content.into_success_outcome()),
            Self::Failed {
                call_id,
                content,
                diagnostic,
            } => (call_id, content.into_failure_outcome(diagnostic)),
        }
    }
}

impl ToolInvocationContent {
    fn into_success_outcome(self) -> ToolExecutionOutcome {
        match self.kind {
            ToolInvocationContentKind::Text => ToolExecutionOutcome::succeeded_text(self.value),
            ToolInvocationContentKind::Json => ToolExecutionOutcome::succeeded_json(self.value),
        }
    }

    fn into_failure_outcome(self, diagnostic: ErrorInfo) -> ToolExecutionOutcome {
        match self.kind {
            ToolInvocationContentKind::Text => {
                ToolExecutionOutcome::failed_text(self.value, diagnostic)
            }
            ToolInvocationContentKind::Json => {
                ToolExecutionOutcome::failed_json(self.value, diagnostic)
            }
        }
    }
}

pub(crate) fn order_tool_invocation_results(
    invocations: &[ToolInvocation],
    results: Vec<ToolInvocationResult>,
) -> Result<Vec<(ToolCallId, ToolExecutionOutcome)>, AgentError> {
    let expected_call_ids = invocations
        .iter()
        .map(|invocation| invocation.id().clone())
        .collect::<Vec<_>>();
    let mut received_call_ids = Vec::with_capacity(results.len());
    let mut results_by_call_id = BTreeMap::new();
    for result in results {
        let call_id = result.call_id().clone();
        received_call_ids.push(call_id.clone());
        if results_by_call_id.insert(call_id, result).is_some() {
            return Err(AgentError::ToolInvocationBatchMismatch {
                expected_call_ids,
                received_call_ids,
            });
        }
    }

    let mut expected_set = expected_call_ids.clone();
    expected_set.sort();
    let mut received_set = results_by_call_id.keys().cloned().collect::<Vec<_>>();
    received_set.sort();
    if expected_set != received_set || expected_call_ids.len() != received_call_ids.len() {
        return Err(AgentError::ToolInvocationBatchMismatch {
            expected_call_ids,
            received_call_ids,
        });
    }

    let mut ordered_results = Vec::with_capacity(invocations.len());
    for invocation in invocations {
        let Some(result) = results_by_call_id.remove(invocation.id()) else {
            return Err(AgentError::ToolInvocationBatchMismatch {
                expected_call_ids,
                received_call_ids,
            });
        };
        ordered_results.push(result.into_runtime());
    }
    Ok(ordered_results)
}

/// One externally executed tool invocation yielded by a [`AgentRun`].
///
/// The request contains only provider-neutral call data. It does not expose a
/// runtime artifact id because result artifact ownership remains in Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocation {
    call: PendingToolCall,
}

impl ToolInvocation {
    pub(crate) fn new(call: PendingToolCall) -> Self {
        Self { call }
    }

    /// Returns the provider/model-originated call identifier.
    #[must_use]
    pub fn id(&self) -> &ToolCallId {
        self.call.id()
    }

    /// Returns the provider-portable tool name.
    #[must_use]
    pub fn name(&self) -> &ToolName {
        self.call.name()
    }

    /// Returns the validated JSON object arguments.
    #[must_use]
    pub fn arguments(&self) -> &ToolCallArguments {
        self.call.arguments()
    }
}

/// Ordered externally executed tool invocations yielded for one host execution
/// wave.
///
/// A single invocation is represented as a batch of length one so bindings do
/// not need separate single-call and multi-call state machines. Invocation order
/// is the model order. The batch is only a delivery and completion boundary;
/// hosts may execute independent calls concurrently only when their own tool
/// policy allows it, and must submit one result for every invocation before the
/// runtime continues to the next execution wave or model response.
///
/// The batch borrows the run exclusively while it is unresolved. This makes
/// the runtime phase transition explicit in Rust: `next`, `result`, and
/// `cancel` cannot be called on the run until this batch is submitted or
/// cancelled. Dropping an unresolved batch requests cancellation as a final
/// guard against leaving the producer waiting for a result forever.
pub struct ToolInvocationBatch<'a> {
    invocations: Vec<ToolInvocation>,
    batch_id: ToolCallBatchId,
    run: &'a mut AgentRun,
    resolved: bool,
}

impl<'a> ToolInvocationBatch<'a> {
    fn from_batch(
        run: &'a mut AgentRun,
        batch_id: ToolCallBatchId,
        calls: Vec<PendingToolCall>,
    ) -> Option<Self> {
        if calls.is_empty() {
            return None;
        }
        Some(Self {
            invocations: calls.into_iter().map(ToolInvocation::new).collect(),
            batch_id,
            run,
            resolved: false,
        })
    }

    /// Returns invocations in model order within this host execution wave.
    #[must_use]
    pub fn invocations(&self) -> &[ToolInvocation] {
        &self.invocations
    }

    /// Returns the number of invocations in this batch.
    #[must_use]
    pub fn len(&self) -> usize {
        self.invocations.len()
    }

    /// Returns whether this batch contains no invocations.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.invocations.is_empty()
    }

    /// Submits the complete result set for this batch.
    ///
    /// Results may be supplied in any order. The runtime validates the complete
    /// set and persists it in pending-call order. A correctable rejected
    /// submission leaves this batch active so the caller can fix and retry.
    /// When runtime records the calls as failed to recover the loop, this
    /// returns [`ToolInvocationSubmission::RejectedAndRecorded`] and releases
    /// the lease.
    pub async fn submit(
        &mut self,
        results: Vec<ToolInvocationResult>,
    ) -> Result<ToolInvocationSubmission, AgentError> {
        if self.resolved {
            return Err(AgentError::ToolInvocationBatchResolved);
        }
        let result = self
            .run
            .submit_tool_invocation_results(&self.batch_id, &self.invocations, results)
            .await;
        if result.is_ok() {
            self.resolved = true;
        }
        result
    }

    /// Cancels the run while this batch is awaiting host execution.
    pub async fn cancel(mut self) -> Result<RunResult, AgentError> {
        if self.resolved {
            return Err(AgentError::ToolInvocationBatchResolved);
        }
        self.resolved = true;
        self.run.cancel().await
    }
}

impl fmt::Debug for ToolInvocationBatch<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolInvocationBatch")
            .field("invocations", &self.invocations)
            .finish()
    }
}

impl PartialEq for ToolInvocationBatch<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.invocations == other.invocations
    }
}

impl Eq for ToolInvocationBatch<'_> {}

impl Drop for ToolInvocationBatch<'_> {
    fn drop(&mut self) {
        if !self.resolved {
            self.run.inner.request_cancel();
        }
    }
}

/// Message returned by the non-interactive agent run.
///
/// A run has one consumption path: callers repeatedly invoke
/// [`AgentRun::next`] for events, and resolve a tool invocation batch before
/// the run can be used again.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentRunMessage<'a> {
    /// A durable runtime event.
    Event(Box<RuntimeEvent>),
    /// Ordered host-owned tool invocations that must be executed by the
    /// embedding caller.
    ToolInvocations {
        /// Exclusive batch lease that must be submitted or cancelled before
        /// the run can be used again.
        batch: ToolInvocationBatch<'a>,
    },
}

impl<'a> AgentRunMessage<'a> {
    /// Borrows the runtime event when this message carries one.
    #[must_use]
    pub fn as_event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(event) => Some(event.as_ref()),
            Self::ToolInvocations { .. } => None,
        }
    }

    /// Borrows the tool invocation batch when this message carries one.
    #[must_use]
    pub fn as_tool_invocations(&self) -> Option<&ToolInvocationBatch<'a>> {
        match self {
            Self::Event(_) => None,
            Self::ToolInvocations { batch } => Some(batch),
        }
    }

    /// Mutably borrows the tool invocation batch when this message carries one.
    #[must_use]
    pub fn as_tool_invocations_mut(&mut self) -> Option<&mut ToolInvocationBatch<'a>> {
        match self {
            Self::Event(_) => None,
            Self::ToolInvocations { batch } => Some(batch),
        }
    }
}

/// Single-consumer handle for one high-level non-interactive run.
///
/// This type intentionally does not implement the futures `Stream` trait. A
/// stream of only runtime events cannot safely represent a tool invocation:
/// consuming the next event could otherwise discard the handoff while the
/// runtime waits for its result. The explicit message method makes the
/// protocol visible to Rust and foreign-language bindings alike.
pub struct AgentRun {
    runtime: Runtime,
    inner: RuntimeAgentRun,
    ended: bool,
    terminal_result: Option<RunResult>,
    terminal_error: Option<AgentError>,
    terminal_error_observed: bool,
    result_consumed: bool,
}

impl AgentRun {
    fn new(runtime: Runtime, inner: RuntimeAgentRun) -> Self {
        Self {
            runtime,
            inner,
            ended: false,
            terminal_result: None,
            terminal_error: None,
            terminal_error_observed: false,
            result_consumed: false,
        }
    }

    /// Returns the next event or tool invocation batch.
    ///
    /// `Ok(None)` means the producer reached its terminal boundary. Runtime
    /// method failures are returned as `Err` instead of being hidden behind
    /// end-of-stream. A returned tool invocation batch holds an exclusive
    /// borrow of this run until it is submitted or cancelled.
    pub async fn next_message(&mut self) -> Result<Option<AgentRunMessage<'_>>, AgentError> {
        if self.ended {
            return Ok(None);
        }

        let Some(message) = self.inner.next_message().await.map_err(AgentError::from)? else {
            self.ended = true;
            match self.finish_inner().await {
                Ok(result) => self.terminal_result = Some(result),
                Err(error) => {
                    self.terminal_error = Some(error);
                    self.terminal_error_observed = true;
                    return match self.terminal_error.take() {
                        Some(error) => Err(error),
                        None => Err(AgentError::AgentRunResultMissing),
                    };
                }
            }
            return Ok(None);
        };

        match message {
            merry_runtime::AgentRunMessage::Event(event) => {
                Ok(Some(AgentRunMessage::Event(Box::new(event))))
            }
            merry_runtime::AgentRunMessage::ToolInvocations { batch } => {
                self.tool_invocation_message(batch.id().clone(), batch.calls().to_vec())
            }
            _ => Err(AgentError::AgentRunProtocol {
                message: "runtime emitted an unsupported agent run message",
            }),
        }
    }

    /// Returns the next message using the idiomatic Rust name for this
    /// message-first protocol.
    pub async fn next(&mut self) -> Result<Option<AgentRunMessage<'_>>, AgentError> {
        self.next_message().await
    }

    /// Resolves every invocation in the supplied batch.
    ///
    /// Results may be supplied in any order. The runtime persists them in the
    /// pending-call order for this host wave and only then permits the next
    /// message to be read.
    /// The batch is borrowed so a caller can retry after validation or runtime
    /// rejection.
    async fn submit_tool_invocation_results(
        &mut self,
        batch_id: &ToolCallBatchId,
        invocations: &[ToolInvocation],
        results: Vec<ToolInvocationResult>,
    ) -> Result<ToolInvocationSubmission, AgentError> {
        let ordered_results = order_tool_invocation_results(invocations, results)?;
        match self
            .inner
            .submit_bridge_tool_outcomes(batch_id, ordered_results)
            .await
        {
            Ok(()) => Ok(ToolInvocationSubmission::Accepted),
            Err(merry_runtime::RuntimeError::BridgeToolResultRejected { .. }) => {
                Ok(ToolInvocationSubmission::RejectedAndRecorded)
            }
            Err(error) => Err(AgentError::from(error)),
        }
    }

    /// Returns the terminal result after [`Self::next_message`] returned `Ok(None)`.
    pub async fn result(&mut self) -> Result<RunResult, AgentError> {
        if !self.ended {
            return Err(AgentError::AgentRunNotFinished);
        }
        self.take_terminal_result()
    }

    /// Cancels the run and waits for a durable cancelled terminal result.
    pub async fn cancel(&mut self) -> Result<RunResult, AgentError> {
        if !self.ended {
            self.inner.cancel_and_wait().await;
            self.ended = true;
            match self.finish_inner().await {
                Ok(result) => self.terminal_result = Some(result),
                Err(error) => self.terminal_error = Some(error),
            }
        }
        self.take_terminal_result()
    }

    async fn finish_inner(&mut self) -> Result<RunResult, AgentError> {
        let result = self.inner.result().await.map_err(AgentError::from)?;
        RunResult::from_runtime(&self.runtime, result).await
    }

    fn tool_invocation_message(
        &mut self,
        batch_id: ToolCallBatchId,
        calls: Vec<PendingToolCall>,
    ) -> Result<Option<AgentRunMessage<'_>>, AgentError> {
        let Some(batch) = ToolInvocationBatch::from_batch(self, batch_id, calls) else {
            return Err(AgentError::AgentRunProtocol {
                message: "runtime emitted an empty tool invocation batch",
            });
        };
        Ok(Some(AgentRunMessage::ToolInvocations { batch }))
    }

    fn take_terminal_result(&mut self) -> Result<RunResult, AgentError> {
        if self.result_consumed {
            return Err(AgentError::AgentRunResultConsumed);
        }
        self.result_consumed = true;
        if let Some(result) = self.terminal_result.take() {
            return Ok(result);
        }
        if let Some(error) = self.terminal_error.take() {
            return Err(error);
        }
        if self.terminal_error_observed {
            return Err(AgentError::AgentRunResultConsumed);
        }
        Err(AgentError::AgentRunResultMissing)
    }
}
