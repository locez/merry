//! Application-level agent profile contract.
//!
//! Profiles own application composition and policy. The facade only defines
//! how a profile contributes runtime configuration and loop policy; concrete
//! profiles remain in their owning crates.

use crate::{AgentLoopConfig, AgentProfileError, FileSessionStore, Tool};
use merry_coding::CodingAgentProfile;
use merry_runtime::{RuntimeBuilder, RuntimeProfile};
use std::num::NonZeroUsize;

/// Mutable SDK-owned construction context supplied to an [`AgentProfile`].
///
/// The context exposes application-facing composition operations while keeping
/// the runtime builder and its internal registration details private. Profiles
/// that need a complete Merry-owned runtime profile are adapted inside this
/// facade rather than receiving a low-level builder directly.
pub struct AgentProfileContext {
    runtime_builder: Option<RuntimeBuilder>,
    loop_config: AgentLoopConfig,
}

impl AgentProfileContext {
    pub(crate) fn new(runtime_builder: RuntimeBuilder, loop_config: AgentLoopConfig) -> Self {
        Self {
            runtime_builder: Some(runtime_builder),
            loop_config,
        }
    }

    /// Registers one trusted in-process application tool for this profile.
    pub fn tool(&mut self, tool: Tool) -> Result<(), AgentProfileError> {
        let Some(builder) = self.runtime_builder.take() else {
            return Err(AgentProfileError::ContextUnavailable);
        };
        self.runtime_builder = Some(builder.register_tool(tool.into_registered_tool()));
        Ok(())
    }

    /// Sets the session store used by this agent profile.
    pub fn session_store(&mut self, store: FileSessionStore) -> Result<(), AgentProfileError> {
        let Some(builder) = self.runtime_builder.take() else {
            return Err(AgentProfileError::ContextUnavailable);
        };
        self.runtime_builder = Some(builder.session_store(store));
        Ok(())
    }

    /// Sets the bounded event buffer used by this agent profile.
    pub fn event_buffer_size(&mut self, size: NonZeroUsize) -> Result<(), AgentProfileError> {
        let Some(builder) = self.runtime_builder.take() else {
            return Err(AgentProfileError::ContextUnavailable);
        };
        self.runtime_builder = Some(builder.event_buffer_size(size));
        Ok(())
    }

    /// Sets the model-loop policy contributed by this profile.
    pub fn loop_config(&mut self, config: AgentLoopConfig) {
        self.loop_config = config;
    }

    pub(crate) fn apply_runtime_profile(
        &mut self,
        profile: RuntimeProfile,
    ) -> Result<(), AgentProfileError> {
        let Some(builder) = self.runtime_builder.take() else {
            return Err(AgentProfileError::ContextUnavailable);
        };
        self.runtime_builder = Some(
            builder
                .with_profile(profile)
                .map_err(|source| AgentProfileError::Runtime { source })?,
        );
        Ok(())
    }

    pub(crate) fn into_parts(self) -> Result<(RuntimeBuilder, AgentLoopConfig), AgentProfileError> {
        let Some(runtime_builder) = self.runtime_builder else {
            return Err(AgentProfileError::ContextUnavailable);
        };
        Ok((runtime_builder, self.loop_config))
    }
}

/// Application-level composition and loop policy for a Merry agent.
///
/// Implementations configure the SDK-owned context. They do not receive or
/// own runtime session state, and they do not need to understand runtime
/// executors, ledgers, artifacts, or provider wire formats.
pub trait AgentProfile: Send + Sync {
    /// Configures application-facing tools, options, and loop policy.
    fn configure(&self, context: &mut AgentProfileContext) -> Result<(), AgentProfileError>;
}

impl AgentProfile for CodingAgentProfile {
    fn configure(&self, context: &mut AgentProfileContext) -> Result<(), AgentProfileError> {
        context.apply_runtime_profile(self.runtime_profile())?;
        let loop_config = CodingAgentProfile::loop_config(self)
            .map_err(|source| AgentProfileError::LoopConfig { source })?;
        context.loop_config(loop_config);
        Ok(())
    }
}
