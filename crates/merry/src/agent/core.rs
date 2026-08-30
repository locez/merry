//! High-level agent builder and session facade.

use super::run::AgentRun;
use crate::{
    errors::{AgentBuildError, AgentError},
    interactive::InteractiveRun,
    profile::{AgentProfile, AgentProfileContext},
    providers::{ConfiguredModelProvider, RuntimeBuilderProviderExt},
    run_result::RunResult,
    stream::{
        AgentEventStream, StructuredAgentEventStream, StructuredAgentRun, StructuredRunResult,
    },
    tools::{Tool, ToolBuildError},
};
use merry_core::{SessionId, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{GenerationConfig, ModelName, ModelProvider};
use merry_runtime::{
    AgentLoopConfig, FileSessionStore, FinalOutputContract, Runtime, RuntimeBuilder,
    RuntimeModelRole, RuntimeProfile, StepContext, StepInput, StructuredOutputRetryPolicy,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use std::{num::NonZeroUsize, sync::Arc};

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

    /// Registers a tool whose invocation is handed to the embedding host.
    ///
    /// The facade owns bridge registration and explicit opt-in. Bindings and
    /// other foreign-language hosts only need to provide the provider-neutral
    /// name, description, and object input schema; runtime admission and
    /// result persistence remain in Rust.
    pub fn bridge_tool(
        self,
        name: ToolName,
        description: &str,
        input_schema: ToolInputSchema,
    ) -> Result<Self, AgentBuildError> {
        let spec = ToolSpec::new(name, description, input_schema)
            .map_err(ToolBuildError::from)
            .map_err(|source| AgentBuildError::Tool { source })?;
        Ok(self
            .allow_bridge_tools()
            .register_tool(merry_runtime::RegisteredTool::bridge(spec)))
    }

    /// Registers an already validated bridge tool specification.
    #[doc(hidden)]
    #[must_use]
    pub fn bridge_tool_spec(self, spec: ToolSpec) -> Self {
        self.allow_bridge_tools()
            .register_tool(merry_runtime::RegisteredTool::bridge(spec))
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

    /// Starts an owned host-tool handoff stream for a foreign-language binding.
    ///
    /// The returned handle keeps the same message-first and batch-resolution
    /// contract as [`Self::stream_with_tool_handoff`] without exposing Rust
    /// borrow lifetimes to the binding layer.
    pub fn stream_with_owned_tool_handoff(
        &self,
        task: &str,
    ) -> Result<crate::binding::OwnedAgentRun, AgentError> {
        self.stream_with_tool_handoff(task)
            .map(crate::binding::OwnedAgentRun::from)
    }

    /// Starts an owned host-tool handoff stream with a runtime-owned final
    /// structured-output contract.
    pub fn stream_with_owned_tool_handoff_and_schema(
        &self,
        task: &str,
        schema: ToolInputSchema,
    ) -> Result<crate::binding::OwnedAgentRun, AgentError> {
        let contract = FinalOutputContract::new(schema)
            .map_err(|source| AgentError::FinalOutputContract { source })?;
        let config = self
            .loop_config
            .clone()
            .with_final_output_contract(contract);
        self.stream_with_config(task, config)
            .map(crate::binding::OwnedAgentRun::from)
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
