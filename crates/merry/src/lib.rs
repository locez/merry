//! Public Rust facade for Merry.
//!
//! This crate is the interface layer for applications embedding Merry. It
//! exposes the high-level semantic agent API plus small components for runtime
//! construction: providers, profiles, tools, lifecycle hooks, events, and
//! stable errors. Lower-level crates still own their domains.
//!
//! # Context Windows And Lifecycle Hooks
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use merry::{
//!     AfterModelDecision, AgentBuilder, BeforeModelDecision, BeforeToolDecision,
//!     CompactionConfig, HookRegistry, SessionId, ToolName, providers::openai_compatible,
//! };
//! use std::num::NonZeroU64;
//!
//! let provider = openai_compatible()
//!     .api_key("sk-...")?
//!     .model_name("gpt-4.1-mini")?
//!     .context_window_tokens(NonZeroU64::new(128_000).expect("non-zero"))
//!     .build()?;
//!
//! let hooks = HookRegistry::new()
//!     .before_model_step(|ctx| async move {
//!         if ctx.session_usage.is_some_and(|u| u.total.total_tokens >= 100_000) {
//!             return BeforeModelDecision::stop_turn("token budget exceeded", None);
//!         }
//!         BeforeModelDecision::continue_normally()
//!     })
//!     .before_tool_call_for([ToolName::new("run_shell")?], |_ctx| async move {
//!         BeforeToolDecision::deny("shell execution disabled")
//!     })
//!     .after_model_step(|_ctx| async move { AfterModelDecision::accept() });
//!
//! let agent = AgentBuilder::new(SessionId::new("demo")?)
//!     .provider(provider)
//!     .compaction(CompactionConfig::default())
//!     .hooks(hooks)
//!     .build()?;
//!
//! let _result = agent.run("Summarize the workspace.").await?;
//! # Ok(())
//! # }
//! ```

mod agent;
mod agent_loop;
pub mod binding;
mod errors;
mod interactive;
mod profile;
pub mod profiles;
pub mod providers;
#[doc(hidden)]
pub mod runtime {
    //! Explicit low-level runtime escape hatch for advanced integrations.

    pub use merry_runtime::{
        AgentLoopResult, AgentRun, AgentRunMessage, Runtime, RuntimeBuilder, RuntimeError,
        RuntimeModelRole, RuntimeProfile, RuntimeProfileBuilder, SessionStoreError,
    };
}
/// Rust-only lifetime-bearing adapter types for advanced hosts.
///
/// Foreign-language bindings must use [`binding`], whose owned protocol does
/// not expose Rust lifetimes. This module is intentionally separate from that
/// FFI contract and is hidden from generated API documentation.
#[doc(hidden)]
pub mod rust {
    pub use super::agent::{
        AgentRun, AgentRunMessage, ToolInvocation, ToolInvocationBatch, ToolInvocationContent,
        ToolInvocationContentError, ToolInvocationResult, ToolInvocationSubmission,
    };
    pub use super::interactive::{
        InteractiveMessage, InteractiveToolInvocation, InteractiveToolInvocationBatch,
    };
    pub use super::stream::StructuredAgentRun;
}
mod run_result;
mod stream;
pub mod tools;

pub use agent::{Agent, AgentBuilder, RunResult};
pub use agent_loop::{coding_agent_loop_config, generic_agent_loop_config};
pub use errors::{AgentBuildError, AgentError, AgentProfileError};
pub use interactive::{
    InteractiveControl, InteractiveEventStream, InteractiveInput, InteractiveInputItem,
    InteractiveInputSnapshot, InteractiveRun,
};
pub use merry_core::{
    ContextWindowSource, RuntimeEvent, SessionId, ToolCallArguments, ToolName, ToolOutput,
};
pub use merry_llm::{
    GenerationConfig, ModelName, ModelProvider, ModelRetryPolicy, ParallelToolCalls,
    ReasoningEffort, ServiceTier,
};
pub use merry_runtime::{
    AfterModelDecision, AfterModelStepContext, AfterToolCallContext, AfterToolDecision,
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopConfigError, AgentLoopStatus, ArchiveConfig,
    BeforeModelDecision, BeforeModelStepContext, BeforeToolCallContext, BeforeToolDecision,
    CompactionConfig, CompactionOutcome, FINAL_OUTPUT_TOOL_NAME, FileSessionStore, FinalOutput,
    HistoryRetention, HookDecision, HookError, HookFuture, HookInvocation, HookInvocationId,
    HookPoint, HookRegistry, HookRequest, HookSubscriptions, InteractivePrimaryModel,
    InteractiveSettingsUpdate, InterruptReason, PromptBlock, PromptError, PromptProfile,
    QueuedInputId, RuntimeHookHandler, StructuredOutputRetryPolicy, ToolHookFilter,
};
pub use profile::{AgentProfile, AgentProfileContext};
pub use stream::{AgentEventStream, StructuredAgentEventStream, StructuredRunResult};
pub use tools::{Tool, ToolBuildError, ToolSpec, tool};
