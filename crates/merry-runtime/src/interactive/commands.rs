use super::{
    InteractiveError, InteractiveSettingsUpdate, InterruptReason,
    queue::AcceptedQueuedInput,
    types::{InputRecords, QueuedInputId},
};
use crate::{FileSessionStore, UserMessageInput};
use merry_core::PlanId;
use tokio::sync::oneshot;

pub(super) enum InteractiveCommand {
    Submit {
        message: UserMessageInput,
        ack_sender: oneshot::Sender<Result<QueuedInputId, InteractiveError>>,
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
        ids: Vec<QueuedInputId>,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    SetPendingPaused {
        paused: bool,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    FlushPending {
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    Snapshot {
        ack_sender: oneshot::Sender<InputRecords>,
    },
    UpdateSettings {
        update: Box<InteractiveSettingsUpdate>,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    Interrupt {
        reason: InterruptReason,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    SaveSession {
        store: FileSessionStore,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    SetPlanMode {
        enabled: bool,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    ApprovePlan {
        plan_id: PlanId,
        note: Option<String>,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    RequestPlanChanges {
        plan_id: PlanId,
        feedback: String,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    AbandonPlan {
        plan_id: PlanId,
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
    RunPending,
    Close,
}

pub(super) enum BoundaryAction {
    UserInput { accepted: Vec<AcceptedQueuedInput> },
    Continuation,
    Wait,
}
