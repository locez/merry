use super::{
    InteractiveError, InteractiveSettingsUpdate, InterruptReason,
    queue::AcceptedQueuedInput,
    types::{InputReceipt, InputRecords, QueuedInputId},
};
use crate::{PlanApprovalInput, UserMessageInput};
use merry_core::{PlanNodeId, QueuedInputLane};
use tokio::sync::oneshot;

pub(super) enum InteractiveCommand {
    SubmitNext {
        message: UserMessageInput,
        ack_sender: oneshot::Sender<Result<InputReceipt, InteractiveError>>,
    },
    Enqueue {
        message: UserMessageInput,
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
    UpdateSettings {
        update: Box<InteractiveSettingsUpdate>,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
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
    EnterPlanMode {
        reason: String,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    ApprovePlan {
        input: PlanApprovalInput,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    RevisePlan {
        reason: String,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    PausePlanScheduling {
        reason: String,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    ResumePlanScheduling {
        reason: String,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    RetryInterruptedPlanNode {
        node_id: PlanNodeId,
        reason: String,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    CancelPlan {
        reason: String,
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
    RunContinuation,
    RunNext,
    RunSuspended,
    RunBacklog,
    Close,
}

pub(super) enum BoundaryAction {
    UserInput {
        accepted: Vec<AcceptedQueuedInput>,
        lane: QueuedInputLane,
    },
    Continuation,
    Wait,
}
