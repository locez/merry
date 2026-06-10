use super::{
    InteractiveError, InterruptReason,
    types::{InputReceipt, InputRecords, QueuedInputId},
};
use merry_core::{QueuedInputLane, QueuedInputView};
use tokio::sync::oneshot;

pub(super) enum InteractiveCommand {
    SubmitNext {
        text: String,
        ack_sender: oneshot::Sender<Result<InputReceipt, InteractiveError>>,
    },
    Enqueue {
        text: String,
        ack_sender: oneshot::Sender<Result<InputReceipt, InteractiveError>>,
    },
    Update {
        id: QueuedInputId,
        text: String,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    Remove {
        id: QueuedInputId,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    ReplacePendingOrder {
        lane: QueuedInputLane,
        ids: Vec<QueuedInputId>,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    Snapshot {
        ack_sender: oneshot::Sender<InputRecords>,
    },
    ResumeSuspended {
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    DiscardSuspended {
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    Interrupt {
        reason: InterruptReason,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    Close {
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandHandlingMode {
    Waiting,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandDecision {
    Continue,
    RunNext,
    RunSuspended,
    RunBacklog,
    Close,
}

pub(super) enum BoundaryAction {
    UserInput {
        accepted: Vec<QueuedInputView>,
        lane: QueuedInputLane,
    },
    Continuation,
    Wait,
}
