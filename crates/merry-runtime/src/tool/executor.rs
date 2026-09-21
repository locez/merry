use crate::tool::{
    ToolExecutionOutcome, patch_evidence::WorkspacePatchProposal, proposal::ActionProposal,
};
use merry_core::PendingToolCall;
use std::{future::Future, pin::Pin};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Boxed tool executor future used for object-safe async tool boundaries.
///
/// Public tool boundaries use an explicit boxed future so registered executors
/// can be stored behind [`ToolExecutor`].
pub type ToolExecutorFuture<'a> = Pin<Box<dyn Future<Output = ToolExecutionResult> + Send + 'a>>;

/// Boxed action proposal future used for object-safe async tool boundaries.
///
/// The proposal hook is runtime-owned and provider-neutral. It is intentionally
/// not rendered into provider tool specs or runtime events.
pub type ToolActionProposalFuture<'a> =
    Pin<Box<dyn Future<Output = ToolActionProposalResult> + Send + 'a>>;

/// Result returned by a runtime-owned tool executor.
///
/// [`ToolExecutionError`] represents executor infrastructure failure or
/// cooperative cancellation. Tool-domain failures should be returned as a
/// failed [`ToolExecutionOutcome`] so runtime can durably resolve the pending
/// tool call.
pub type ToolExecutionResult = Result<ToolExecutionOutcome, ToolExecutionError>;

/// Optional proposal returned before a write-classified tool can be resolved by policy.
///
/// `NoProposal` means the executor cannot provide deterministic proposal
/// evidence for this call and policy should continue with its normal decision
/// path.
pub type ToolActionProposalResult = Result<ToolActionPreflight, ToolExecutionError>;

/// Context passed to a tool executor.
///
/// The context is intentionally small for the MVP: cancellation is cooperative
/// and runtime state mutation stays owned by [`crate::Runtime::execute_tool_call`].
#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    pub(super) cancellation_token: CancellationToken,
    pub(super) approved_apply_patch: Option<WorkspacePatchProposal>,
}

impl ToolExecutionContext {
    /// Creates a tool execution context with the provided cancellation token.
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self {
            cancellation_token,
            approved_apply_patch: None,
        }
    }

    /// Returns the cancellation token for this tool execution.
    ///
    /// Executors should check this token at cancellation points and return
    /// [`ToolExecutionError::Cancelled`] when no durable result was produced.
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Borrows the approved workspace patch proposal for this execution.
    ///
    /// This is executor-internal runtime state. It is never rendered into
    /// provider-visible tool specs, tool result artifacts, or continuations.
    #[must_use]
    pub fn approved_apply_patch(&self) -> Option<&WorkspacePatchProposal> {
        self.approved_apply_patch.as_ref()
    }

    pub(crate) fn with_approved_apply_patch(mut self, patch: WorkspacePatchProposal) -> Self {
        self.approved_apply_patch = Some(patch);
        self
    }
}

impl Default for ToolExecutionContext {
    fn default() -> Self {
        Self {
            cancellation_token: CancellationToken::new(),
            approved_apply_patch: None,
        }
    }
}

/// Object-safe runtime tool executor boundary.
///
/// The executor returns content and status only. Runtime code records the
/// artifact, emits events, updates the ledger, and resolves the pending call.
///
/// Implementations should not call runtime mutation APIs from inside
/// [`ToolExecutor::execute`]. The runtime already owns the active-step permit
/// while this method runs.
pub trait ToolExecutor: Send + Sync {
    /// Builds read-only deterministic evidence for a proposed action.
    ///
    /// This hook must not mutate workspace, runtime, network, or process state.
    /// It exists so runtime can record a provider-neutral proposal before a
    /// mutating action is denied, approved, or otherwise reviewed. Existing
    /// tools can omit this method; the default returns no proposal.
    fn propose<'a>(
        &'a self,
        _call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async { Ok(ToolActionPreflight::NoProposal) })
    }

    /// Executes one pending model-requested tool call.
    ///
    /// The pending call uses Merry-owned ids, tool names, and arguments rather
    /// than provider response structs.
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a>;
}

/// Result of a tool action preflight/proposal hook.
///
/// Mutating tools use this hook before runtime policy decides whether execution
/// is allowed. Most tools return a proposal or no proposal. Tools may return a
/// durable failed outcome for provider-supplied argument errors discovered
/// during preflight, so the model receives actionable feedback instead of an
/// infrastructure failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolActionPreflight {
    /// The tool did not provide deterministic proposal evidence.
    NoProposal,
    /// The tool provided proposal evidence for policy review.
    Proposal(ActionProposal),
    /// The tool preflight produced a durable tool outcome.
    Outcome(ToolExecutionOutcome),
}

/// Infrastructure-level errors raised by tool executors.
///
/// Use this for cancellation or infrastructure failures only. If the tool ran
/// and produced a domain-level failure, return a failed [`ToolExecutionOutcome`]
/// instead.
#[derive(Debug, Error)]
pub enum ToolExecutionError {
    /// Tool execution was cancelled cooperatively.
    #[error("tool execution cancelled")]
    Cancelled,

    /// Tool execution could not complete because the executor infrastructure failed.
    #[error("tool execution infrastructure error: {message}")]
    Infrastructure {
        /// Actionable executor error detail.
        message: String,
    },
}

impl ToolExecutionError {
    /// Creates an infrastructure error.
    ///
    /// Infrastructure errors leave the pending tool call unresolved.
    #[must_use]
    pub fn infrastructure(message: impl Into<String>) -> Self {
        Self::Infrastructure {
            message: message.into(),
        }
    }
}
