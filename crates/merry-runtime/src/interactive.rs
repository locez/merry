#![allow(dead_code)]

use crate::{
    AgentLoopConfig, Runtime, RuntimeError, StepContext, StepInput, ToolExecutionContext,
    agent_loop::{PendingLoopToolCall, StepOutcome, classify_step_events},
    event_stream::ActiveStepPermit,
};
use futures_core::Stream;
use futures_util::StreamExt;
use merry_core::RuntimeEvent;
use merry_llm::GenerationConfig;
use std::{
    collections::{HashMap, VecDeque},
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

static NEXT_INTERACTIVE_RUN_ID: AtomicU64 = AtomicU64::new(1);

fn next_interactive_run_id() -> InteractiveRunId {
    InteractiveRunId(NEXT_INTERACTIVE_RUN_ID.fetch_add(1, Ordering::Relaxed))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InteractiveRunId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InteractiveInputId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    Next,
    Suspended,
    Backlog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveRunState {
    WaitingForInput,
    RunningModel,
    RunningTool,
    Interrupting,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptReason {
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputReceipt {
    pub id: InteractiveInputId,
    pub queue: QueueKind,
    pub position: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInputSnapshot {
    pub id: InteractiveInputId,
    pub text: String,
    pub queue: QueueKind,
    pub position: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub next: Vec<QueuedInputSnapshot>,
    pub suspended: Vec<QueuedInputSnapshot>,
    pub backlog: Vec<QueuedInputSnapshot>,
}

#[derive(Debug, Error)]
pub enum InteractiveError {
    #[error("interactive run {run_id:?} is closed")]
    RunClosed { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} command channel is closed")]
    CommandChannelClosed { run_id: InteractiveRunId },
    #[error("invalid interactive input: {reason}")]
    InvalidInput { reason: &'static str },
    #[error("interactive input {id:?} is unknown")]
    UnknownInput { id: InteractiveInputId },
    #[error("interactive input {id:?} is already accepted")]
    AlreadyAccepted { id: InteractiveInputId },
    #[error("interactive input {id:?} is already removed")]
    AlreadyRemoved { id: InteractiveInputId },
    #[error("interactive input {id:?} is in {actual:?}, expected {expected:?}")]
    WrongQueue {
        id: InteractiveInputId,
        expected: QueueKind,
        actual: QueueKind,
    },
    #[error("interactive queue {queue:?} is full")]
    QueueFull { queue: QueueKind },
    #[error("runtime error while running interactive loop: {source}")]
    Runtime {
        #[from]
        source: RuntimeError,
    },
}

#[derive(Debug)]
pub enum InteractiveRunEvent {
    StateChanged {
        state: InteractiveRunState,
    },
    InputAccepted {
        ids: Vec<InteractiveInputId>,
        queue: QueueKind,
    },
    QueueChanged {
        snapshot: QueueSnapshot,
    },
    Runtime(RuntimeEvent),
    Closed,
}

pub struct InteractiveRunEventStream {
    inner: Option<ReceiverStream<InteractiveRunEvent>>,
    cancellation_token: CancellationToken,
    producer_handle: Option<JoinHandle<()>>,
}

impl InteractiveRunEventStream {
    pub(crate) fn new(
        inner: ReceiverStream<InteractiveRunEvent>,
        cancellation_token: CancellationToken,
        producer_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            inner: Some(inner),
            cancellation_token,
            producer_handle: Some(producer_handle),
        }
    }
}

impl Stream for InteractiveRunEventStream {
    type Item = InteractiveRunEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(None);
        };

        match Pin::new(inner).poll_next(cx) {
            Poll::Ready(None) => {
                self.producer_handle.take();
                Poll::Ready(None)
            }
            poll => poll,
        }
    }
}

impl Drop for InteractiveRunEventStream {
    fn drop(&mut self) {
        self.inner.take();
        self.cancellation_token.cancel();

        if let Some(handle) = self.producer_handle.take() {
            handle.abort();
        }
    }
}

pub struct InteractiveAgentRun {
    stream: InteractiveRunEventStream,
    input: AgentLoopInput,
    control: AgentLoopControl,
}

impl InteractiveAgentRun {
    pub(crate) fn new(
        stream: InteractiveRunEventStream,
        input: AgentLoopInput,
        control: AgentLoopControl,
    ) -> Self {
        Self {
            stream,
            input,
            control,
        }
    }

    #[must_use]
    pub fn split(self) -> (InteractiveRunEventStream, AgentLoopInput, AgentLoopControl) {
        (self.stream, self.input, self.control)
    }
}

#[derive(Clone)]
pub struct AgentLoopInput {
    run_id: InteractiveRunId,
    command_sender: mpsc::Sender<InteractiveCommand>,
}

impl AgentLoopInput {
    fn new(run_id: InteractiveRunId, command_sender: mpsc::Sender<InteractiveCommand>) -> Self {
        Self {
            run_id,
            command_sender,
        }
    }

    #[must_use]
    pub fn run_id(&self) -> InteractiveRunId {
        self.run_id
    }

    pub async fn submit_next(&self, text: &str) -> Result<InputReceipt, InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::SubmitNext {
                text: text.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn enqueue(&self, text: &str) -> Result<InputReceipt, InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Enqueue {
                text: text.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn snapshot(&self) -> Result<QueueSnapshot, InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Snapshot { ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })
    }
}

#[derive(Clone)]
pub struct AgentLoopControl {
    run_id: InteractiveRunId,
    command_sender: mpsc::Sender<InteractiveCommand>,
}

impl AgentLoopControl {
    fn new(run_id: InteractiveRunId, command_sender: mpsc::Sender<InteractiveCommand>) -> Self {
        Self {
            run_id,
            command_sender,
        }
    }

    #[must_use]
    pub fn run_id(&self) -> InteractiveRunId {
        self.run_id
    }

    pub async fn resume_backlog(&self) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::ResumeBacklog { ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn interrupt(&self, reason: InterruptReason) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Interrupt { reason, ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }
}

enum InteractiveCommand {
    SubmitNext {
        text: String,
        ack_sender: oneshot::Sender<Result<InputReceipt, InteractiveError>>,
    },
    Enqueue {
        text: String,
        ack_sender: oneshot::Sender<Result<InputReceipt, InteractiveError>>,
    },
    Snapshot {
        ack_sender: oneshot::Sender<QueueSnapshot>,
    },
    ResumeBacklog {
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
    Interrupt {
        reason: InterruptReason,
        ack_sender: oneshot::Sender<Result<(), InteractiveError>>,
    },
}

impl Runtime {
    pub fn start_interactive_agent_run(
        &self,
        context: StepContext,
        config: AgentLoopConfig,
    ) -> Result<InteractiveAgentRun, RuntimeError> {
        let loop_permit = self.acquire_active_step_permit()?;
        let (parent_token, generation_config, _final_output_contract) = context.into_parts();
        let loop_token = parent_token.child_token();
        let producer_token = loop_token.clone();
        let run_id = next_interactive_run_id();
        let (event_sender, event_receiver) = mpsc::channel(16);
        let (command_sender, command_receiver) = mpsc::channel(16);
        let producer = InteractiveProducer {
            runtime: self.clone(),
            run_id,
            queue: InteractiveInputQueue::default(),
            command_receiver,
            event_sender,
            loop_token: producer_token,
            generation_config,
            config,
            loop_permit,
            backlog_resume_requested: false,
            phase_token: None,
            interrupted: false,
        };
        let producer_handle = tokio::spawn(async move {
            producer.run().await;
        });

        Ok(InteractiveAgentRun::new(
            InteractiveRunEventStream::new(
                ReceiverStream::new(event_receiver),
                loop_token,
                producer_handle,
            ),
            AgentLoopInput::new(run_id, command_sender.clone()),
            AgentLoopControl::new(run_id, command_sender),
        ))
    }
}

struct InteractiveProducer {
    runtime: Runtime,
    run_id: InteractiveRunId,
    queue: InteractiveInputQueue,
    command_receiver: mpsc::Receiver<InteractiveCommand>,
    event_sender: mpsc::Sender<InteractiveRunEvent>,
    loop_token: CancellationToken,
    generation_config: GenerationConfig,
    config: AgentLoopConfig,
    loop_permit: ActiveStepPermit,
    backlog_resume_requested: bool,
    phase_token: Option<CancellationToken>,
    interrupted: bool,
}

impl InteractiveProducer {
    async fn run(mut self) {
        if !self.send_state(InteractiveRunState::WaitingForInput).await {
            return;
        }

        while !self.loop_token.is_cancelled() {
            let Some(command) = self.command_receiver.recv().await else {
                break;
            };

            let Some(decision) = self
                .handle_command(command, CommandHandlingMode::Waiting)
                .await
            else {
                return;
            };
            match decision {
                CommandDecision::Continue => {}
                CommandDecision::RunNext => {
                    if !self.run_next_burst().await {
                        return;
                    }
                }
                CommandDecision::RunBacklog => {
                    if !self.run_one_backlog().await {
                        return;
                    }
                }
            }
        }

        let _ = self
            .event_sender
            .send(InteractiveRunEvent::StateChanged {
                state: InteractiveRunState::Closed,
            })
            .await;
        let _ = self.event_sender.send(InteractiveRunEvent::Closed).await;
    }

    async fn run_next_burst(&mut self) -> bool {
        let accepted = self.queue.accept_next_burst();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted, QueueKind::Next).await
    }

    async fn run_one_backlog(&mut self) -> bool {
        self.backlog_resume_requested = false;
        let accepted = self.queue.accept_one_backlog();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted, QueueKind::Backlog).await
    }

    async fn run_accepted_steps(
        &mut self,
        mut accepted: Vec<QueuedInputSnapshot>,
        mut queue: QueueKind,
    ) -> bool {
        loop {
            let Some(input) = step_input_from_accepted(&accepted) else {
                return false;
            };
            let Some(step_events) = self.run_model_phase(input, Some((&accepted, queue))).await
            else {
                return false;
            };

            let Some(continuation_required) = self.handle_step_outcome(&step_events).await else {
                return false;
            };

            if !self.drain_ready_commands().await {
                return false;
            }

            match self.boundary_action(continuation_required) {
                BoundaryAction::UserInput {
                    accepted: next_accepted,
                    queue: next_queue,
                } => {
                    accepted = next_accepted;
                    queue = next_queue;
                }
                BoundaryAction::Continuation => {
                    let Some(boundary) = self.run_continuation_steps().await else {
                        return false;
                    };
                    match boundary {
                        BoundaryAction::UserInput {
                            accepted: next_accepted,
                            queue: next_queue,
                        } => {
                            accepted = next_accepted;
                            queue = next_queue;
                        }
                        BoundaryAction::Continuation => continue,
                        BoundaryAction::Wait => {
                            return self.send_state(InteractiveRunState::WaitingForInput).await;
                        }
                    }
                }
                BoundaryAction::Wait => {
                    return self.send_state(InteractiveRunState::WaitingForInput).await;
                }
            }
        }
    }

    async fn run_continuation_steps(&mut self) -> Option<BoundaryAction> {
        loop {
            let step_events = self
                .run_model_phase(StepInput::no_new_user_input(), None)
                .await?;
            let continuation_required = self.handle_step_outcome(&step_events).await?;
            if !self.drain_ready_commands().await {
                return None;
            }
            let boundary = self.boundary_action(continuation_required);
            if matches!(boundary, BoundaryAction::Continuation) {
                continue;
            }
            return Some(boundary);
        }
    }

    async fn run_model_phase(
        &mut self,
        input: StepInput,
        accepted: Option<(&[QueuedInputSnapshot], QueueKind)>,
    ) -> Option<Vec<RuntimeEvent>> {
        if let Some((accepted, queue)) = accepted {
            let ids = accepted.iter().map(|item| item.id).collect::<Vec<_>>();
            if self
                .event_sender
                .send(InteractiveRunEvent::InputAccepted { ids, queue })
                .await
                .is_err()
            {
                return None;
            }
            if !self.send_queue_changed().await {
                return None;
            }
        }

        if !self.send_state(InteractiveRunState::RunningModel).await {
            return None;
        }

        let phase_token = self.loop_token.child_token();
        self.phase_token = Some(phase_token.clone());
        let mut step_context =
            StepContext::new(phase_token).with_generation_config(self.generation_config.clone());
        if let Some(contract) = self.config.final_output_contract().cloned() {
            step_context = step_context.with_final_output_contract(contract);
        }

        let stream = match self.runtime.step_with_active_permit(
            input,
            step_context,
            self.loop_permit.clone(),
        ) {
            Ok(stream) => stream,
            Err(error) => {
                self.phase_token = None;
                tracing::debug!(error = %error, "interactive step start failed");
                return None;
            }
        };
        let events = self.forward_step_until_boundary(stream).await;
        self.phase_token = None;
        events
    }

    async fn forward_step_until_boundary(
        &mut self,
        stream: crate::RuntimeEventStream,
    ) -> Option<Vec<RuntimeEvent>> {
        let mut events = Vec::new();
        tokio::pin!(stream);
        let mut commands_open = true;
        loop {
            tokio::select! {
                event = stream.next() => {
                    let Some(event) = event else {
                        return Some(events);
                    };
                    events.push(event.clone());
                    if self
                        .event_sender
                        .send(InteractiveRunEvent::Runtime(event))
                        .await
                        .is_err()
                    {
                        return None;
                    }
                }
                command = self.command_receiver.recv(), if commands_open => {
                    let Some(command) = command else {
                        commands_open = false;
                        continue;
                    };
                    if self
                        .handle_command(command, CommandHandlingMode::Running)
                        .await
                        .is_none()
                    {
                        return None;
                    }
                }
            }
        }
    }

    async fn handle_step_outcome(&mut self, step_events: &[RuntimeEvent]) -> Option<bool> {
        match classify_step_events(step_events, self.config.final_output_contract()) {
            StepOutcome::Pending(PendingLoopToolCall::Runtime(call)) if !self.interrupted => {
                self.run_runtime_tool(call).await
            }
            StepOutcome::ToolResultRecorded => Some(!self.interrupted),
            _ => Some(false),
        }
    }

    async fn run_runtime_tool(&mut self, call: merry_core::PendingToolCall) -> Option<bool> {
        if !self.send_state(InteractiveRunState::RunningTool).await {
            return None;
        }

        let phase_token = self.loop_token.child_token();
        self.phase_token = Some(phase_token.clone());
        let runtime = self.runtime.clone();
        let loop_permit = self.loop_permit.clone();
        let call_id = call.id().clone();
        let execution_call_id = call_id.clone();
        let execution_permit = loop_permit.clone();
        let execution = async move {
            runtime
                .execute_tool_call_with_active_permit(
                    &execution_call_id,
                    ToolExecutionContext::new(phase_token),
                    &execution_permit,
                )
                .await
        };
        tokio::pin!(execution);
        let mut commands_open = true;

        let result = loop {
            tokio::select! {
                result = &mut execution => break result,
                command = self.command_receiver.recv(), if commands_open => {
                    let Some(command) = command else {
                        commands_open = false;
                        continue;
                    };
                    self.handle_command(command, CommandHandlingMode::Running).await?;
                }
            }
        };
        self.phase_token = None;

        match result {
            Ok(events) => {
                if !self.send_runtime_events(events).await {
                    return None;
                }
                Some(!self.interrupted)
            }
            Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
                let runtime = self.runtime.clone();
                let events = match runtime
                    .submit_tool_interrupt_failure_with_active_permit(&call_id, &loop_permit)
                    .await
                {
                    Ok(events) => events,
                    Err(error) => {
                        tracing::debug!(error = %error, "interactive tool interrupt result failed");
                        return None;
                    }
                };
                self.interrupted = true;
                if !self.send_runtime_events(events).await {
                    return None;
                }
                Some(false)
            }
            Err(error) => {
                tracing::debug!(error = %error, "interactive runtime tool failed");
                None
            }
        }
    }

    async fn send_runtime_events(&self, events: Vec<RuntimeEvent>) -> bool {
        for event in events {
            if self
                .event_sender
                .send(InteractiveRunEvent::Runtime(event))
                .await
                .is_err()
            {
                return false;
            }
        }
        true
    }

    async fn drain_ready_commands(&mut self) -> bool {
        while let Ok(command) = self.command_receiver.try_recv() {
            if self
                .handle_command(command, CommandHandlingMode::Running)
                .await
                .is_none()
            {
                return false;
            }
        }
        true
    }

    fn boundary_action(&mut self, continuation_required: bool) -> BoundaryAction {
        if !self.queue.next.is_empty() {
            self.interrupted = false;
            return BoundaryAction::UserInput {
                accepted: self.queue.accept_next_burst(),
                queue: QueueKind::Next,
            };
        }

        if self.interrupted {
            return BoundaryAction::Wait;
        }

        if self.backlog_resume_requested {
            self.backlog_resume_requested = false;
            let accepted = self.queue.accept_one_backlog();
            if !accepted.is_empty() {
                return BoundaryAction::UserInput {
                    accepted,
                    queue: QueueKind::Backlog,
                };
            }
        }

        if continuation_required {
            return BoundaryAction::Continuation;
        }

        BoundaryAction::Wait
    }

    async fn handle_command(
        &mut self,
        command: InteractiveCommand,
        mode: CommandHandlingMode,
    ) -> Option<CommandDecision> {
        match command {
            InteractiveCommand::SubmitNext { text, ack_sender } => {
                let receipt = self.queue.submit_next(&text);
                let should_run = mode == CommandHandlingMode::Waiting && receipt.is_ok();
                let queue_changed = receipt.is_ok();
                let _ = ack_sender.send(receipt);
                if queue_changed && !self.send_queue_changed().await {
                    return None;
                }
                if should_run {
                    return Some(CommandDecision::RunNext);
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::Enqueue { text, ack_sender } => {
                let receipt = self.queue.enqueue(&text);
                let queue_changed = receipt.is_ok();
                let _ = ack_sender.send(receipt);
                if queue_changed && !self.send_queue_changed().await {
                    return None;
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::Snapshot { ack_sender } => {
                let _ = ack_sender.send(self.queue.snapshot());
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::ResumeBacklog { ack_sender } => {
                self.backlog_resume_requested = true;
                let _ = ack_sender.send(Ok(()));
                if mode == CommandHandlingMode::Waiting {
                    return Some(CommandDecision::RunBacklog);
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::Interrupt { reason, ack_sender } => {
                let _ = ack_sender.send(Ok(()));
                if mode == CommandHandlingMode::Running {
                    self.interrupt_running_phase(reason).await
                } else {
                    Some(CommandDecision::Continue)
                }
            }
        }
    }

    async fn interrupt_running_phase(
        &mut self,
        _reason: InterruptReason,
    ) -> Option<CommandDecision> {
        self.interrupted = true;
        self.backlog_resume_requested = false;
        self.queue.suspend_next();
        if let Some(token) = self.phase_token.as_ref() {
            token.cancel();
        }
        if !self.send_queue_changed().await {
            return None;
        }
        if !self.send_state(InteractiveRunState::Interrupting).await {
            return None;
        }
        Some(CommandDecision::Continue)
    }

    async fn send_queue_changed(&self) -> bool {
        self.event_sender
            .send(InteractiveRunEvent::QueueChanged {
                snapshot: self.queue.snapshot(),
            })
            .await
            .is_ok()
    }

    async fn send_state(&self, state: InteractiveRunState) -> bool {
        self.event_sender
            .send(InteractiveRunEvent::StateChanged { state })
            .await
            .is_ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandHandlingMode {
    Waiting,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandDecision {
    Continue,
    RunNext,
    RunBacklog,
}

enum BoundaryAction {
    UserInput {
        accepted: Vec<QueuedInputSnapshot>,
        queue: QueueKind,
    },
    Continuation,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuedInputState {
    Pending,
    Accepted,
    Removed,
}

#[derive(Debug, Clone)]
struct QueuedInput {
    text: String,
    queue: QueueKind,
    state: QueuedInputState,
}

#[derive(Debug, Default)]
pub(crate) struct InteractiveInputQueue {
    next_id: u64,
    next: VecDeque<InteractiveInputId>,
    suspended: VecDeque<InteractiveInputId>,
    backlog: VecDeque<InteractiveInputId>,
    items: HashMap<InteractiveInputId, QueuedInput>,
}

impl InteractiveInputQueue {
    pub(crate) fn submit_next(&mut self, text: &str) -> Result<InputReceipt, InteractiveError> {
        self.push(text, QueueKind::Next)
    }

    pub(crate) fn enqueue(&mut self, text: &str) -> Result<InputReceipt, InteractiveError> {
        self.push(text, QueueKind::Backlog)
    }

    pub(crate) fn update(
        &mut self,
        id: InteractiveInputId,
        text: &str,
    ) -> Result<(), InteractiveError> {
        validate_interactive_text(text)?;
        let item = self.pending_item_mut(id)?;
        item.text = text.to_owned();
        Ok(())
    }

    pub(crate) fn remove(&mut self, id: InteractiveInputId) -> Result<(), InteractiveError> {
        let queue = self.pending_item(id)?.queue;
        self.remove_from_queue(id, queue)?;
        self.pending_item_mut(id)?.state = QueuedInputState::Removed;
        Ok(())
    }

    pub(crate) fn move_before(
        &mut self,
        id: InteractiveInputId,
        anchor: InteractiveInputId,
    ) -> Result<(), InteractiveError> {
        let queue = self.same_queue(id, anchor)?;
        let ids = self.queue_mut(queue);
        remove_id(ids, id);
        let index = ids
            .iter()
            .position(|candidate| *candidate == anchor)
            .ok_or(InteractiveError::UnknownInput { id: anchor })?;
        ids.insert(index, id);
        Ok(())
    }

    pub(crate) fn move_after(
        &mut self,
        id: InteractiveInputId,
        anchor: InteractiveInputId,
    ) -> Result<(), InteractiveError> {
        let queue = self.same_queue(id, anchor)?;
        let ids = self.queue_mut(queue);
        remove_id(ids, id);
        let index = ids
            .iter()
            .position(|candidate| *candidate == anchor)
            .ok_or(InteractiveError::UnknownInput { id: anchor })?;
        ids.insert(index + 1, id);
        Ok(())
    }

    pub(crate) fn suspend_next(&mut self) {
        while let Some(id) = self.next.pop_front() {
            if let Some(item) = self.items.get_mut(&id) {
                item.queue = QueueKind::Suspended;
            }
            self.suspended.push_back(id);
        }
    }

    pub(crate) fn discard_suspended(&mut self) -> Vec<InteractiveInputId> {
        self.suspended
            .drain(..)
            .inspect(|id| {
                if let Some(item) = self.items.get_mut(id) {
                    item.state = QueuedInputState::Removed;
                }
            })
            .collect()
    }

    pub(crate) fn accept_next_burst(&mut self) -> Vec<QueuedInputSnapshot> {
        self.accept_queue_burst(QueueKind::Next)
    }

    pub(crate) fn accept_suspended_burst(&mut self) -> Vec<QueuedInputSnapshot> {
        self.accept_queue_burst(QueueKind::Suspended)
    }

    pub(crate) fn accept_one_backlog(&mut self) -> Vec<QueuedInputSnapshot> {
        let Some(id) = self.backlog.pop_front() else {
            return Vec::new();
        };
        self.mark_accepted(id, QueueKind::Backlog, 0)
            .into_iter()
            .collect()
    }

    pub(crate) fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            next: self.snapshots_for(QueueKind::Next),
            suspended: self.snapshots_for(QueueKind::Suspended),
            backlog: self.snapshots_for(QueueKind::Backlog),
        }
    }

    fn push(&mut self, text: &str, queue: QueueKind) -> Result<InputReceipt, InteractiveError> {
        validate_interactive_text(text)?;
        let id = InteractiveInputId(self.next_id);
        self.next_id += 1;
        let ids = self.queue_mut(queue);
        let position = ids.len();
        ids.push_back(id);
        self.items.insert(
            id,
            QueuedInput {
                text: text.to_owned(),
                queue,
                state: QueuedInputState::Pending,
            },
        );
        Ok(InputReceipt {
            id,
            queue,
            position,
        })
    }

    fn pending_item(&self, id: InteractiveInputId) -> Result<&QueuedInput, InteractiveError> {
        let item = self
            .items
            .get(&id)
            .ok_or(InteractiveError::UnknownInput { id })?;

        match item.state {
            QueuedInputState::Pending => Ok(item),
            QueuedInputState::Accepted => Err(InteractiveError::AlreadyAccepted { id }),
            QueuedInputState::Removed => Err(InteractiveError::AlreadyRemoved { id }),
        }
    }

    fn pending_item_mut(
        &mut self,
        id: InteractiveInputId,
    ) -> Result<&mut QueuedInput, InteractiveError> {
        let item = self
            .items
            .get_mut(&id)
            .ok_or(InteractiveError::UnknownInput { id })?;

        match item.state {
            QueuedInputState::Pending => Ok(item),
            QueuedInputState::Accepted => Err(InteractiveError::AlreadyAccepted { id }),
            QueuedInputState::Removed => Err(InteractiveError::AlreadyRemoved { id }),
        }
    }

    fn queue_mut(&mut self, queue: QueueKind) -> &mut VecDeque<InteractiveInputId> {
        match queue {
            QueueKind::Next => &mut self.next,
            QueueKind::Suspended => &mut self.suspended,
            QueueKind::Backlog => &mut self.backlog,
        }
    }

    fn queue(&self, queue: QueueKind) -> &VecDeque<InteractiveInputId> {
        match queue {
            QueueKind::Next => &self.next,
            QueueKind::Suspended => &self.suspended,
            QueueKind::Backlog => &self.backlog,
        }
    }

    fn same_queue(
        &self,
        id: InteractiveInputId,
        anchor: InteractiveInputId,
    ) -> Result<QueueKind, InteractiveError> {
        let queue = self.pending_item(id)?.queue;
        let anchor_queue = self.pending_item(anchor)?.queue;
        if queue != anchor_queue {
            return Err(InteractiveError::WrongQueue {
                id,
                expected: anchor_queue,
                actual: queue,
            });
        }
        Ok(queue)
    }

    fn accept_queue_burst(&mut self, queue: QueueKind) -> Vec<QueuedInputSnapshot> {
        let ids = std::mem::take(self.queue_mut(queue));
        ids.into_iter()
            .enumerate()
            .filter_map(|(position, id)| self.mark_accepted(id, queue, position))
            .collect()
    }

    fn mark_accepted(
        &mut self,
        id: InteractiveInputId,
        queue: QueueKind,
        position: usize,
    ) -> Option<QueuedInputSnapshot> {
        let item = self.items.get_mut(&id)?;
        if item.state != QueuedInputState::Pending {
            return None;
        }
        item.state = QueuedInputState::Accepted;
        item.queue = queue;
        Some(QueuedInputSnapshot {
            id,
            text: item.text.clone(),
            queue,
            position,
        })
    }

    fn snapshots_for(&self, queue: QueueKind) -> Vec<QueuedInputSnapshot> {
        self.queue(queue)
            .iter()
            .enumerate()
            .filter_map(|(position, id)| {
                let item = self.items.get(id)?;
                (item.state == QueuedInputState::Pending).then(|| QueuedInputSnapshot {
                    id: *id,
                    text: item.text.clone(),
                    queue,
                    position,
                })
            })
            .collect()
    }

    fn remove_from_queue(
        &mut self,
        id: InteractiveInputId,
        queue: QueueKind,
    ) -> Result<(), InteractiveError> {
        if remove_id(self.queue_mut(queue), id) {
            Ok(())
        } else {
            Err(InteractiveError::UnknownInput { id })
        }
    }
}

fn remove_id(ids: &mut VecDeque<InteractiveInputId>, id: InteractiveInputId) -> bool {
    let Some(index) = ids.iter().position(|candidate| *candidate == id) else {
        return false;
    };
    ids.remove(index);
    true
}

fn validate_interactive_text(text: &str) -> Result<(), InteractiveError> {
    if text.trim().is_empty() {
        return Err(InteractiveError::InvalidInput {
            reason: "input text must not be blank",
        });
    }

    if text
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(InteractiveError::InvalidInput {
            reason: "input text must not contain control characters other than newline or tab",
        });
    }

    Ok(())
}

fn step_input_from_accepted(accepted: &[QueuedInputSnapshot]) -> Option<StepInput> {
    StepInput::user_texts(accepted.iter().map(|item| item.text.as_str())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_submit_next_preempts_backlog_without_reordering_backlog() {
        let mut queue = InteractiveInputQueue::default();
        let backlog = queue.enqueue("backlog").expect("valid backlog");
        let next = queue.submit_next("next").expect("valid next");

        assert_eq!(queue.snapshot().next[0].id, next.id);
        assert_eq!(queue.snapshot().backlog[0].id, backlog.id);
    }

    #[test]
    fn queue_update_remove_and_reorder_pending_items() {
        let mut queue = InteractiveInputQueue::default();
        let first = queue.enqueue("first").expect("valid first").id;
        let second = queue.enqueue("second").expect("valid second").id;

        queue
            .update(first, "updated")
            .expect("pending item updates");
        queue
            .move_before(second, first)
            .expect("pending item reorders");
        queue.remove(first).expect("pending item removes");

        let snapshot = queue.snapshot();
        assert_eq!(snapshot.backlog[0].id, second);
        assert_eq!(snapshot.backlog[0].text, "second");
        assert_eq!(snapshot.backlog.len(), 1);
    }

    #[test]
    fn queue_interrupt_moves_next_to_suspended_and_leaves_backlog() {
        let mut queue = InteractiveInputQueue::default();
        let first = queue.submit_next("x").expect("valid next").id;
        let second = queue.submit_next("y").expect("valid next").id;
        let backlog = queue.enqueue("later").expect("valid backlog").id;

        queue.suspend_next();

        let snapshot = queue.snapshot();
        assert!(snapshot.next.is_empty());
        assert_eq!(
            snapshot
                .suspended
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert_eq!(snapshot.backlog[0].id, backlog);
    }

    #[test]
    fn queue_rejects_edit_after_acceptance() {
        let mut queue = InteractiveInputQueue::default();
        let id = queue.submit_next("x").expect("valid next").id;
        let accepted = queue.accept_next_burst();
        assert_eq!(
            accepted.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![id]
        );

        let err = queue
            .update(id, "changed")
            .expect_err("accepted item should not update");
        assert!(matches!(err, InteractiveError::AlreadyAccepted { .. }));
    }
}
