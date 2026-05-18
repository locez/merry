//! Runtime error types.

use crate::artifact::{ArtifactContentKind, ArtifactError};
use merry_core::{ArtifactId, CoreError, SessionId, ToolCallId};
use thiserror::Error;

/// Errors raised by Merry runtime construction and step admission.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// A new step was requested while another step still owns the runtime.
    #[error("runtime step already active for session {session_id}")]
    StepAlreadyActive {
        /// Session with an active step.
        session_id: SessionId,
    },

    /// Step input failed runtime validation.
    #[error("invalid step input: {reason}")]
    InvalidStepInput {
        /// Actionable validation detail.
        reason: &'static str,
    },

    /// A submitted tool result does not match any pending or resolved call.
    #[error("tool call {call_id} is not pending in session {session_id}")]
    UnknownToolCall {
        /// Session receiving the result.
        session_id: SessionId,
        /// Tool call id that could not be found.
        call_id: ToolCallId,
    },

    /// A tool result was submitted after the call had already been resolved.
    #[error("tool call {call_id} is already resolved in session {session_id}")]
    ToolCallAlreadyResolved {
        /// Session receiving the duplicate result.
        session_id: SessionId,
        /// Tool call id that already has a result.
        call_id: ToolCallId,
    },

    /// A tool result used content that the MVP submit path does not accept.
    #[error("tool result artifact {artifact_id} uses unsupported content kind {content_kind:?}")]
    UnsupportedToolResultContent {
        /// Artifact identifier for the submitted tool result.
        artifact_id: ArtifactId,
        /// Submitted content category.
        content_kind: ArtifactContentKind,
    },

    /// A core protocol value could not be constructed.
    #[error("core protocol error while constructing runtime state: {source}")]
    Core {
        /// Source core validation error.
        #[from]
        source: CoreError,
    },

    /// Artifact state could not be recorded or read.
    #[error("artifact state error: {source}")]
    Artifact {
        /// Source artifact error.
        #[from]
        source: ArtifactError,
    },
}
