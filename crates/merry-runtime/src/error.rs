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
use crate::session_store::SessionStoreError;
use merry_core::{ArtifactId, CoreError, SessionId, ToolCallBatchId, ToolCallId, ToolName};
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

    /// An agent-loop entry point received conflicting structured-output
    /// contracts from its context and loop configuration.
    #[error("invalid agent loop configuration: {source}")]
    AgentLoopConfig {
        /// Invalid loop configuration detail.
        #[source]
        source: crate::agent_loop::AgentLoopConfigError,
    },

    /// A child runtime factory could not materialize its runtime profile.
    #[error("child runtime construction failed: {message}")]
    ChildRuntimeBuild {
        /// Profile construction detail from the owning composition layer.
        message: String,
    },

    /// A subagent status operation did not select any child agents.
    #[error("{operation} requires at least one agent id")]
    InvalidSubagentSelection {
        /// Provider-visible operation that received the empty selection.
        operation: &'static str,
    },

    /// A mutating Plan tool could not durably record its effect before execution.
    #[error(
        "plan effect attribution failed for tool call {call_id} in session {session_id}: {message}"
    )]
    PlanEffectAttribution {
        /// Session executing the Plan tool.
        session_id: SessionId,
        /// Tool call that was not executed.
        call_id: ToolCallId,
        /// Actionable controller or persistence detail.
        message: String,
    },

    /// A runtime bound to one Plan subagent no longer owns a live attempt.
    #[error(
        "plan subagent attempt is not live for tool call {call_id} in session {session_id}: {message}"
    )]
    PlanSubagentAttemptInactive {
        /// Session whose bound attempt is no longer admissible.
        session_id: SessionId,
        /// Tool call that was not executed.
        call_id: ToolCallId,
        /// Actionable controller or persisted-state detail.
        message: String,
    },

    /// A user image or image-bearing message failed runtime validation.
    #[error("invalid user image input: {reason}")]
    InvalidUserImageInput {
        /// Actionable image validation detail.
        reason: String,
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

    /// A bridge result submission did not contain any outcomes.
    #[error("bridge tool result batch is empty in session {session_id}")]
    BridgeToolResultBatchEmpty {
        /// Session receiving the empty result batch.
        session_id: SessionId,
    },

    /// A bridge result submission did not resolve exactly the requested calls.
    #[error(
        "bridge tool result batch does not match pending calls in session {session_id}: expected {expected_call_ids:?}, received {received_call_ids:?}"
    )]
    BridgeToolResultBatchMismatch {
        /// Session receiving the mismatched result batch.
        session_id: SessionId,
        /// Call ids currently waiting for external execution.
        expected_call_ids: Vec<ToolCallId>,
        /// Call ids supplied by the external executor.
        received_call_ids: Vec<ToolCallId>,
    },

    /// A bridge result was submitted for a different runtime-owned batch.
    #[error(
        "bridge tool result batch id does not match the pending batch in session {session_id}: expected {expected_batch_id}, received {received_batch_id}"
    )]
    BridgeToolResultBatchIdMismatch {
        /// Session receiving the stale or mismatched result.
        session_id: SessionId,
        /// Runtime-owned batch id currently awaiting results.
        expected_batch_id: ToolCallBatchId,
        /// Batch id supplied by the host.
        received_batch_id: ToolCallBatchId,
    },

    /// A host result could not be recorded, but the pending call can still be
    /// resolved as a failed tool result so the model loop can recover.
    #[error(
        "bridge tool result was rejected in session {session_id}; the tool call was recorded as failed: {message}"
    )]
    BridgeToolResultRejected {
        /// Session receiving the host result.
        session_id: SessionId,
        /// Original runtime validation or persistence detail.
        message: String,
    },

    /// The run cannot read another message until its current host batch is resolved.
    #[error(
        "agent run has unresolved tool invocations in session {session_id} for batch {batch_id}"
    )]
    AgentRunToolInvocationsPending {
        /// Session whose run is waiting for host results.
        session_id: SessionId,
        /// Batch that must be resolved first.
        batch_id: ToolCallBatchId,
    },

    /// A host submitted results when the run has no active host batch.
    #[error("agent run has no pending tool invocations in session {session_id}")]
    NoPendingAgentRunToolInvocations {
        /// Session receiving the submission.
        session_id: SessionId,
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

    /// Runtime-reserved provider-visible tool name was registered by an application.
    #[error("tool name {name} is reserved by the runtime")]
    ReservedToolName {
        /// Rejected reserved tool name.
        name: ToolName,
    },

    /// Runtime construction received a tool schema that JSON Schema validation cannot compile.
    #[error("tool {name} input schema is invalid: {message}")]
    InvalidToolInputSchema {
        /// Tool with the invalid input schema.
        name: ToolName,
        /// Actionable schema compiler detail.
        message: String,
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

    /// A runtime-owned agent run stopped before producing a terminal result.
    #[error("agent run closed for session {session_id}: {message}")]
    AgentRunClosed {
        /// Session whose run ended unexpectedly.
        session_id: SessionId,
        /// Actionable stream lifecycle detail.
        message: &'static str,
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

    /// Model turn ids are exhausted for this session.
    #[error("model turn id space is exhausted")]
    ModelTurnIdExhausted,

    /// A transcript mutation referenced a model turn that is not recorded.
    #[error("model turn {model_turn_id} is not recorded")]
    UnknownModelTurn { model_turn_id: u64 },

    /// A transcript mutation attempted to change a terminal or incompatible turn.
    #[error("model turn {model_turn_id} cannot {attempted} from its current status")]
    InvalidModelTurnTransition {
        model_turn_id: u64,
        attempted: &'static str,
    },

    /// A pending tool call was not represented in the durable transcript.
    #[error("tool call {call_id} has no originating transcript item")]
    TranscriptToolCallMissing { call_id: ToolCallId },

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

    /// Session persistence failed.
    #[error("session store error: {source}")]
    SessionStore {
        /// Source session store error.
        #[from]
        source: SessionStoreError,
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

    /// The configured compaction model cannot accept the primary model's context window.
    #[error(
        "compaction model input window {compactor_window_tokens} tokens is smaller than primary context window {primary_window_tokens} tokens"
    )]
    CompactionModelWindowTooSmall {
        /// Resolved primary model context window.
        primary_window_tokens: u64,
        /// Provider-reported compaction model input window.
        compactor_window_tokens: u64,
    },

    /// The compiled compaction request cannot fit in the compaction model input window.
    #[error(
        "compaction model request estimated input {estimated_input_tokens} tokens exceeds compaction model input window {compactor_window_tokens} tokens"
    )]
    CompactionModelInputTooLarge {
        /// Deterministic estimate of the compiled provider-neutral request input.
        estimated_input_tokens: u64,
        /// Reported or primary-window-assumed compaction input window.
        compactor_window_tokens: u64,
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

impl RuntimeError {
    /// Returns the stable provider-neutral code for this runtime failure.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::StepAlreadyActive { .. } => "step_already_active",
            Self::InvalidStepInput { .. } => "invalid_step_input",
            Self::AgentLoopConfig { .. } => "agent_loop_config",
            Self::ChildRuntimeBuild { .. } => "child_runtime_build",
            Self::InvalidSubagentSelection { .. } => "invalid_subagent_selection",
            Self::PlanEffectAttribution { .. } => "plan_effect_attribution_failed",
            Self::PlanSubagentAttemptInactive { .. } => "plan_subagent_attempt_inactive",
            Self::InvalidUserImageInput { .. } => "invalid_user_image_input",
            Self::ReservedArtifactId { .. } => "reserved_artifact_id",
            Self::UnknownToolCall { .. } => "unknown_tool_call",
            Self::BridgeToolResultBatchEmpty { .. } => "bridge_tool_result_batch_empty",
            Self::BridgeToolResultBatchMismatch { .. } => "bridge_tool_result_batch_mismatch",
            Self::BridgeToolResultBatchIdMismatch { .. } => "bridge_tool_result_batch_id_mismatch",
            Self::BridgeToolResultRejected { .. } => "bridge_tool_result_rejected",
            Self::AgentRunToolInvocationsPending { .. } => "agent_run_tool_invocations_pending",
            Self::NoPendingAgentRunToolInvocations { .. } => {
                "no_pending_agent_run_tool_invocations"
            }
            Self::ToolCallAlreadyResolved { .. } => "tool_call_already_resolved",
            Self::DuplicateToolRegistration { .. } => "duplicate_tool_registration",
            Self::ReservedToolName { .. } => "reserved_tool_name",
            Self::InvalidToolInputSchema { .. } => "invalid_tool_input_schema",
            Self::BridgeToolsNotAllowed { .. } => "bridge_tools_not_allowed",
            Self::ToolExecutionCancelled { .. } => "tool_execution_cancelled",
            Self::ToolExecutionFailed { .. } => "tool_execution_failed",
            Self::AgentRunClosed { .. } => "agent_run_closed",
            Self::MissingActionExecutionEvidence { .. } => "missing_action_execution_evidence",
            Self::MutatingActionCommitLifecycleRequired { .. } => {
                "mutating_action_commit_lifecycle_required"
            }
            Self::UnsupportedToolResultContent { .. } => "unsupported_tool_result_content",
            Self::TranscriptItemIdExhausted => "transcript_item_id_exhausted",
            Self::ModelTurnIdExhausted => "model_turn_id_exhausted",
            Self::UnknownModelTurn { .. } => "unknown_model_turn",
            Self::InvalidModelTurnTransition { .. } => "invalid_model_turn_transition",
            Self::TranscriptToolCallMissing { .. } => "transcript_tool_call_missing",
            Self::Core { .. } => "core_error",
            Self::Artifact { .. } => "artifact_error",
            Self::Context { .. } => "context_error",
            Self::Checkpoint { .. } => "checkpoint_error",
            Self::Compaction { .. } => "compaction_error",
            Self::SessionStore { .. } => "session_store",
            Self::MissingModelProvider { .. } => "missing_model_provider",
            Self::CompactionModelRequest { .. } => "compaction_model_request",
            Self::CompactionModelWindowTooSmall { .. } => "compaction_model_window_too_small",
            Self::CompactionModelInputTooLarge { .. } => "compaction_model_input_too_large",
            Self::CompactionModelSetup { .. } => "compaction_model_setup",
            Self::CompactionModelStream { .. } => "compaction_model_stream",
        }
    }

    /// Returns whether a bridge result error can be corrected by resubmitting
    /// the same active batch. This classification is shared by runtime and
    /// language bindings so a binding cannot terminate a run on a validation
    /// error that the host is expected to fix.
    #[must_use]
    pub fn is_retryable_bridge_tool_result(&self) -> bool {
        matches!(
            self,
            Self::BridgeToolResultBatchEmpty { .. }
                | Self::BridgeToolResultBatchMismatch { .. }
                | Self::BridgeToolResultBatchIdMismatch { .. }
                | Self::UnsupportedToolResultContent { .. }
                | Self::Core {
                    source: CoreError::InvalidToolCallResult { .. }
                }
                | Self::Artifact {
                    source: ArtifactError::IncompatibleContent { .. }
                }
        )
    }
}
