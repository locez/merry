//! Runtime error types.
//!
//! Errors in this module describe runtime construction, admission, artifact,
//! and tool execution contracts. Provider failures are normalized before they
//! become runtime events; provider wire errors are not exposed as runtime state.

use crate::ToolActionKind;
use crate::artifact::{ArtifactContentKind, ArtifactError};
use crate::checkpoint::CheckpointError;
use crate::compaction::CompactionError;
use crate::context::ContextError;
use merry_core::{ArtifactId, CoreError, SessionId, ToolCallId, ToolName};
use thiserror::Error;

/// Errors raised by Merry runtime construction and step admission.
///
/// Variants are actionable contract failures at the runtime facade boundary.
/// Cancellation and executor infrastructure failures leave pending tool calls
/// unresolved unless a durable result was already recorded.
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

    /// External artifact recording tried to use a runtime-owned artifact id prefix.
    #[error("artifact id {artifact_id} uses a runtime-reserved prefix")]
    ReservedArtifactId {
        /// Rejected artifact identifier.
        artifact_id: ArtifactId,
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

    /// Runtime construction tried to register the same tool name more than once.
    #[error("tool {name} is already registered")]
    DuplicateToolRegistration {
        /// Duplicate tool name.
        name: ToolName,
    },

    /// Runtime construction tried to register a bridge tool without explicit opt-in.
    #[error("bridge tool {name} requires explicit bridge tool opt-in")]
    BridgeToolsNotAllowed {
        /// Bridge tool name.
        name: ToolName,
    },

    /// Tool execution was cancelled before producing a durable result.
    #[error("tool call {call_id} execution was cancelled in session {session_id}")]
    ToolExecutionCancelled {
        /// Session executing the tool call.
        session_id: SessionId,
        /// Tool call id that remained pending.
        call_id: ToolCallId,
    },

    /// Tool executor infrastructure failed before producing a durable result.
    #[error("tool call {call_id} executor failed in session {session_id}: {message}")]
    ToolExecutionFailed {
        /// Session executing the tool call.
        session_id: SessionId,
        /// Tool call id that remained pending.
        call_id: ToolCallId,
        /// Actionable executor failure detail.
        message: String,
    },

    /// An admitted mutating action reported success without internal execution evidence.
    #[error(
        "tool call {call_id} admitted for {action_kind:?} in session {session_id} succeeded without required execution evidence"
    )]
    MissingActionExecutionEvidence {
        /// Session executing the tool call.
        session_id: SessionId,
        /// Tool call id that remained pending.
        call_id: ToolCallId,
        /// Action kind that required internal execution evidence.
        action_kind: ToolActionKind,
    },

    /// A mutating action reached the generic executor path before a commit lifecycle exists.
    #[error(
        "mutating tool action {action_kind:?} for tool call {call_id} in session {session_id} requires an explicit commit lifecycle before generic execution"
    )]
    MutatingActionCommitLifecycleRequired {
        /// Session executing the tool call.
        session_id: SessionId,
        /// Tool call id that remained pending.
        call_id: ToolCallId,
        /// Mutating action kind that must use a commit lifecycle.
        action_kind: ToolActionKind,
    },

    /// A tool result used content that the MVP submit path does not accept.
    #[error("tool result artifact {artifact_id} uses unsupported content kind {content_kind:?}")]
    UnsupportedToolResultContent {
        /// Artifact identifier for the submitted tool result.
        artifact_id: ArtifactId,
        /// Submitted content category.
        content_kind: ArtifactContentKind,
    },

    /// Transcript item ids are exhausted for this session.
    #[error("transcript item id space is exhausted")]
    TranscriptItemIdExhausted,

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

    /// Context state could not be constructed or compiled.
    #[error("context state error: {source}")]
    Context {
        /// Source context error.
        #[from]
        source: ContextError,
    },

    /// Checkpoint state could not be inspected or validated.
    #[error("checkpoint state error: {source}")]
    Checkpoint {
        /// Source checkpoint error.
        #[from]
        source: CheckpointError,
    },

    /// Compaction state could not be constructed or applied.
    #[error("compaction state error: {source}")]
    Compaction {
        /// Source compaction error.
        #[from]
        source: CompactionError,
    },

    /// A runtime operation needed a model provider for a role that is not configured.
    #[error("missing model provider for runtime role {role}")]
    MissingModelProvider {
        /// Missing model role.
        role: &'static str,
    },

    /// Compaction model request construction failed.
    #[error("compaction model request error: {message}")]
    CompactionModelRequest {
        /// Actionable model request error.
        message: String,
    },

    /// Compaction model setup failed before a stream was returned.
    #[error("compaction model setup error: {message}")]
    CompactionModelSetup {
        /// Actionable model setup error.
        message: String,
    },

    /// Compaction model stream failed or returned an invalid candidate shape.
    #[error("compaction model stream error: {message}")]
    CompactionModelStream {
        /// Actionable model stream error.
        message: String,
    },
}
