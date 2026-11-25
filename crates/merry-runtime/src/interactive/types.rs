use crate::{PlanError, RuntimeError};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

static NEXT_INTERACTIVE_RUN_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn next_interactive_run_id() -> InteractiveRunId {
    InteractiveRunId(NEXT_INTERACTIVE_RUN_ID.fetch_add(1, Ordering::Relaxed))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InteractiveRunId(pub(super) u64);

impl InteractiveRunId {
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptReason {
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InputRecord {
    pub id: QueuedInputId,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct InputRecords {
    pub items: Vec<InputRecord>,
    pub paused: bool,
}

/// Stable runtime-assigned identifier for one pending interactive input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueuedInputId(u64);

impl QueuedInputId {
    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Error)]
pub enum InteractiveError {
    /// The interactive producer ended without emitting the terminal closed event.
    #[error("interactive run {run_id:?} is closed")]
    RunClosed { run_id: InteractiveRunId },
    /// The producer stopped before the stream's normal terminal event.
    #[error(
        "interactive run {run_id:?} producer stopped before the terminal closed event: {message}"
    )]
    ProducerStopped {
        run_id: InteractiveRunId,
        message: &'static str,
    },
    /// The producer task could not be joined successfully.
    #[error("interactive run {run_id:?} producer task failed: {reason}")]
    ProducerTaskFailed {
        run_id: InteractiveRunId,
        reason: String,
    },
    #[error("interactive run {run_id:?} command channel is closed")]
    CommandChannelClosed { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} has unresolved tool invocations")]
    ToolInvocationsPending { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} has no pending tool invocations")]
    NoPendingToolInvocations { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} has an unresolved hook invocation")]
    HookInvocationPending { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} has no pending hook invocation")]
    NoPendingHookInvocation { run_id: InteractiveRunId },
    #[error(
        "interactive run {run_id:?} emitted a hook invocation; consume it with the message protocol"
    )]
    HookInvocationRequiresMessageProtocol { run_id: InteractiveRunId },
    #[error(
        "interactive run {run_id:?} emitted {count} tool invocations; consume them with the message protocol"
    )]
    ToolInvocationsRequireMessageProtocol {
        run_id: InteractiveRunId,
        count: usize,
    },
    #[error("interactive run {run_id:?} produced an invalid tool invocation batch")]
    InvalidToolInvocationBatch { run_id: InteractiveRunId },
    #[error("invalid interactive input: {reason}")]
    InvalidInput { reason: &'static str },
    #[error("interactive input is unknown")]
    UnknownInput,
    #[error("interactive input is already accepted")]
    AlreadyAccepted,
    #[error("interactive input is already removed")]
    AlreadyRemoved,
    #[error("interactive pending input order is invalid: {reason}")]
    InvalidPendingOrder { reason: &'static str },
    #[error("interactive pending input order is stale: {reason}")]
    StalePendingOrder { reason: &'static str },
    #[error("runtime error while running interactive loop: {source}")]
    Runtime {
        #[from]
        source: RuntimeError,
    },
    #[error("plan control failed: {source}")]
    Plan {
        #[from]
        source: PlanError,
    },
    #[error("plan controls require an idle interactive boundary")]
    PlanControlRequiresIdle,
    #[error("session save requires an idle interactive boundary")]
    SessionSaveRequiresIdle,
    #[error("context compaction requires an idle interactive boundary")]
    CompactRequiresIdle,
}
