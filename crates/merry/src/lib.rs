//! Public Rust facade for Merry.
//!
//! This crate is the interface layer for applications embedding Merry. It
//! exposes the high-level semantic agent API plus small components for runtime
//! construction: providers, profiles, tools, events, and stable errors.
//! Lower-level crates still own their domains.

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
        RuntimeModelRole, RuntimeProfile, RuntimeProfileBuilder,
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
pub use merry_core::RuntimeEvent;
pub use merry_core::SessionId;
pub use merry_llm::{GenerationConfig, ModelName, ModelProvider, ModelRetryPolicy};
pub use merry_runtime::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopConfigError, AgentLoopStatus,
    AutomaticCompactionConfig, FINAL_OUTPUT_TOOL_NAME, FileSessionStore, FinalOutput,
    InteractivePrimaryModel, StructuredOutputRetryPolicy,
};
pub use profile::{AgentProfile, AgentProfileContext};
pub use stream::{AgentEventStream, StructuredAgentEventStream, StructuredRunResult};
pub use tools::{Tool, ToolBuildError};
