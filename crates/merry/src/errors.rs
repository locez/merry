//! Stable Merry error types.

use merry_core::ToolCallId;
use merry_runtime::{
    AgentLoopConfigError, AgentLoopError, FinalOutputContractError, InteractiveError, RuntimeError,
};
use thiserror::Error;

/// Failure while an application profile configures the SDK-owned context.
#[derive(Debug, Error)]
pub enum AgentProfileError {
    /// The profile attempted to use a context after a failed configuration.
    #[error("agent profile configuration context is unavailable")]
    ContextUnavailable,
    /// The profile's runtime-owned composition was rejected.
    #[error("agent profile runtime configuration failed: {source}")]
    Runtime {
        /// Underlying runtime construction failure.
        #[source]
        source: RuntimeError,
    },
    /// The profile's loop policy was rejected.
    #[error("agent profile loop configuration is invalid: {source}")]
    LoopConfig {
        /// Underlying loop configuration failure.
        #[source]
        source: AgentLoopConfigError,
    },
}

/// Failure while constructing a high-level [`crate::Agent`].
#[derive(Debug, Error)]
pub enum AgentBuildError {
    /// A primary model provider is required before an agent can be built.
    #[error("a primary model provider is required")]
    MissingPrimaryProvider,
    /// The runtime builder rejected the supplied provider-neutral configuration.
    #[error("runtime build failed: {source}")]
    Runtime {
        /// Underlying runtime construction failure.
        #[source]
        source: RuntimeError,
    },
    /// The selected agent profile's loop policy is invalid.
    #[error("agent profile loop configuration is invalid: {source}")]
    LoopConfig {
        /// Underlying loop configuration failure.
        #[source]
        source: AgentLoopConfigError,
    },
    /// The selected application profile could not configure the SDK context.
    #[error("agent profile configuration failed: {source}")]
    Profile {
        /// Underlying profile configuration failure.
        #[source]
        source: AgentProfileError,
    },
}

impl From<RuntimeError> for AgentBuildError {
    fn from(source: RuntimeError) -> Self {
        Self::Runtime { source }
    }
}

impl From<AgentLoopConfigError> for AgentBuildError {
    fn from(source: AgentLoopConfigError) -> Self {
        Self::LoopConfig { source }
    }
}

impl From<AgentProfileError> for AgentBuildError {
    fn from(source: AgentProfileError) -> Self {
        Self::Profile { source }
    }
}

/// Failure while starting or completing a high-level agent operation.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The task text or another provider-neutral runtime input was invalid.
    #[error("agent input or runtime operation failed: {source}")]
    Runtime {
        /// Underlying runtime validation or operation failure.
        #[source]
        source: RuntimeError,
    },
    /// The runtime loop stopped on a method error.
    #[error("agent loop failed: {source}")]
    Loop {
        /// Underlying loop failure, including preserved runtime context.
        #[source]
        source: AgentLoopError,
    },
    /// The requested structured-output schema was not accepted by runtime.
    #[error("structured output contract is invalid: {source}")]
    FinalOutputContract {
        /// Underlying schema or final-output contract failure.
        #[source]
        source: FinalOutputContractError,
    },
    /// The loop completed without recording the requested final output.
    ///
    /// The run is retained because a blocked or cancelled run is still useful
    /// for inspecting events, usage, and the terminal status.
    #[error("structured output was not recorded by the agent loop")]
    StructuredOutputNotRecorded {
        /// Authoritative run result produced before decoding was attempted.
        run: Box<crate::RunResult>,
    },
    /// The runtime JSON payload could not be decoded as the requested Rust type.
    ///
    /// The run is retained so callers can inspect the exact terminal state and
    /// decide whether to retry the higher-level operation.
    #[error("structured output could not be decoded: {source}")]
    StructuredOutputDecode {
        /// Authoritative run result produced before decoding failed.
        run: Box<crate::RunResult>,
        /// Underlying serde decoding failure.
        #[source]
        source: serde_json::Error,
    },
    /// The runtime rejected an interactive operation or ended its interactive
    /// producer before the requested handoff completed.
    #[error("interactive agent operation failed: {source}")]
    Interactive {
        /// Underlying interactive runtime failure.
        #[source]
        source: InteractiveError,
    },
    /// A tool invocation result set does not match the active batch.
    #[error(
        "tool invocation results do not match active calls: expected {expected_call_ids:?}, received {received_call_ids:?}"
    )]
    ToolInvocationBatchMismatch {
        /// Call ids expected by the active invocation batch.
        expected_call_ids: Vec<ToolCallId>,
        /// Call ids supplied by the host.
        received_call_ids: Vec<ToolCallId>,
    },
    /// A resolved or cancelled invocation batch was used again.
    #[error("tool invocation batch has already been resolved")]
    ToolInvocationBatchResolved,
    /// The event-only stream encountered a host-owned tool handoff.
    #[error(
        "event-only agent stream encountered a host tool handoff; use stream_with_tool_handoff"
    )]
    ToolHandoffRequired,
    /// The terminal result is only available after the agent run reaches EOF.
    #[error("agent run has not reached its terminal boundary")]
    AgentRunNotFinished,
    /// The terminal result was already consumed.
    #[error("agent run terminal result was already consumed")]
    AgentRunResultConsumed,
    /// The agent run reached EOF without a terminal result.
    #[error("agent run reached EOF without a terminal result")]
    AgentRunResultMissing,
    /// The runtime emitted a message outside the facade agent-run contract.
    #[error("agent run protocol error: {message}")]
    AgentRunProtocol {
        /// Stable protocol failure detail.
        message: &'static str,
    },
    /// The runtime emitted an unsupported interactive message.
    #[error("interactive protocol error: {message}")]
    InteractiveProtocol {
        /// Stable protocol failure detail.
        message: &'static str,
    },
}

impl AgentError {
    /// Borrows the retained run when structured decoding failed after the
    /// runtime reached a terminal result.
    #[must_use]
    pub fn structured_run(&self) -> Option<&crate::RunResult> {
        match self {
            Self::StructuredOutputNotRecorded { run }
            | Self::StructuredOutputDecode { run, .. } => Some(run),
            _ => None,
        }
    }

    /// Recovers the retained run from a structured-output failure.
    #[must_use]
    pub fn into_structured_run(self) -> Option<crate::RunResult> {
        match self {
            Self::StructuredOutputNotRecorded { run }
            | Self::StructuredOutputDecode { run, .. } => Some(*run),
            _ => None,
        }
    }
}

impl From<RuntimeError> for AgentError {
    fn from(source: RuntimeError) -> Self {
        Self::Runtime { source }
    }
}

impl From<AgentLoopError> for AgentError {
    fn from(source: AgentLoopError) -> Self {
        Self::Loop { source }
    }
}

impl From<InteractiveError> for AgentError {
    fn from(source: InteractiveError) -> Self {
        Self::Interactive { source }
    }
}
