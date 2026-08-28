use crate::{PlanControllerError, RuntimeError};
use merry_core::QueuedInputLane;
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
pub(super) struct InputReceipt {
    pub id: QueuedInputId,
    pub lane: QueuedInputLane,
    pub position: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InputRecord {
    pub receipt: InputReceipt,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct InputRecords {
    pub next: Vec<InputRecord>,
    pub suspended: Vec<InputRecord>,
    pub backlog: Vec<InputRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct QueuedInputId(u64);

impl QueuedInputId {
    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
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
    #[error("interactive run {run_id:?} producer task failed")]
    ProducerTaskFailed { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} command channel is closed")]
    CommandChannelClosed { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} has unresolved tool invocations")]
    ToolInvocationsPending { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} has no pending tool invocations")]
    NoPendingToolInvocations { run_id: InteractiveRunId },
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
    #[error("interactive input is in {actual:?}, expected {expected:?}")]
    WrongQueue {
        expected: QueuedInputLane,
        actual: QueuedInputLane,
    },
    #[error("interactive pending input order for {lane:?} is invalid: {reason}")]
    InvalidPendingOrder {
        lane: QueuedInputLane,
        reason: &'static str,
    },
    #[error("interactive pending input order for {lane:?} is stale: {reason}")]
    StalePendingOrder {
        lane: QueuedInputLane,
        reason: &'static str,
    },
    #[error("interactive input lane {lane:?} is full")]
    QueueFull { lane: QueuedInputLane },
    #[error("runtime error while running interactive loop: {source}")]
    Runtime {
        #[from]
        source: RuntimeError,
    },
    #[error("plan control failed: {source}")]
    Plan {
        #[from]
        source: PlanControllerError,
    },
    #[error("plan controls require an idle interactive boundary")]
    PlanControlRequiresIdle,
    #[error("session save requires an idle interactive boundary")]
    SessionSaveRequiresIdle,
}
