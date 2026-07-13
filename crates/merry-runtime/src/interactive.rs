mod commands;
mod handles;
mod queue;
mod settings;
mod types;

use self::{
    commands::{BoundaryAction, CommandDecision, CommandHandlingMode, InteractiveCommand},
    queue::{AcceptedQueuedInput, InteractiveInputQueue, step_input_from_accepted},
    types::next_interactive_run_id,
};
use crate::{
    AgentLoopConfig, Runtime, RuntimeError, StepContext, StepInput, ToolExecutionContext,
    agent_loop::{PendingLoopToolCall, StepOutcome, classify_step_events},
    events::{ActiveStepPermit, RuntimeEventProjector},
};
use futures_util::StreamExt;
use merry_core::{
    InteractiveRunState, PendingToolCall, QueuedInputLane, RuntimeEvent, RuntimeJournalEvent,
};
use merry_llm::GenerationConfig;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

pub use self::handles::{
    AgentLoopControl, AgentLoopInput, InteractiveAgentRun, InteractiveInputItem,
    InteractiveInputSnapshot, InteractiveRunEventStream,
};
pub use self::settings::{
    InteractivePrimaryModel, InteractiveSettingsUpdate, InteractiveSubagentSettings,
};
pub use self::types::{InteractiveError, InteractiveRunId, InterruptReason};

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
            queue: InteractiveInputQueue::default(),
            command_receiver,
            event_sender,
            loop_token: producer_token,
            generation_config,
            config,
            loop_permit,
            suspended_resume_requested: false,
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
    queue: InteractiveInputQueue,
    command_receiver: mpsc::Receiver<InteractiveCommand>,
    event_sender: mpsc::Sender<RuntimeEvent>,
    loop_token: CancellationToken,
    generation_config: GenerationConfig,
    config: AgentLoopConfig,
    loop_permit: ActiveStepPermit,
    suspended_resume_requested: bool,
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
                CommandDecision::RunSuspended => {
                    if !self.run_suspended_burst().await {
                        return;
                    }
                }
                CommandDecision::RunBacklog => {
                    if !self.run_one_backlog().await {
                        return;
                    }
                }
                CommandDecision::Close => {
                    break;
                }
            }
        }

        let _ = self
            .event_sender
            .send(RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::Closed,
            })
            .await;
        let _ = self.event_sender.send(RuntimeEvent::Closed).await;
    }

    async fn run_next_burst(&mut self) -> bool {
        let accepted = self.queue.accept_next_burst();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted, QueuedInputLane::Next)
            .await
    }

    async fn run_suspended_burst(&mut self) -> bool {
        self.suspended_resume_requested = false;
        let accepted = self.queue.accept_suspended_burst();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted, QueuedInputLane::Suspended)
            .await
    }

    async fn run_one_backlog(&mut self) -> bool {
        let accepted = self.queue.accept_one_backlog();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted, QueuedInputLane::Backlog)
            .await
    }

    async fn run_accepted_steps(
        &mut self,
        mut accepted: Vec<AcceptedQueuedInput>,
        mut lane: QueuedInputLane,
    ) -> bool {
        self.interrupted = false;
        loop {
            let Some(input) = step_input_from_accepted(&accepted) else {
                return false;
            };
            let Some(step_events) = self.run_model_phase(input, Some((&accepted, lane))).await
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
                    lane: next_lane,
                } => {
                    accepted = next_accepted;
                    lane = next_lane;
                }
                BoundaryAction::Continuation => {
                    let Some(boundary) = self.run_continuation_steps().await else {
                        return false;
                    };
                    match boundary {
                        BoundaryAction::UserInput {
                            accepted: next_accepted,
                            lane: next_lane,
                        } => {
                            accepted = next_accepted;
                            lane = next_lane;
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
        accepted: Option<(&[AcceptedQueuedInput], QueuedInputLane)>,
    ) -> Option<Vec<RuntimeJournalEvent>> {
        if let Some((accepted, lane)) = accepted {
            if self
                .event_sender
                .send(RuntimeEvent::QueuedInputAccepted {
                    lane,
                    inputs: accepted.iter().map(|item| item.view().clone()).collect(),
                })
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
        stream: crate::RuntimeJournalEventStream,
    ) -> Option<Vec<RuntimeJournalEvent>> {
        let mut events = Vec::new();
        tokio::pin!(stream);
        let mut commands_open = true;
        let mut projector = RuntimeEventProjector::new();
        loop {
            tokio::select! {
                event = stream.next() => {
                    let Some(event) = event else {
                        return Some(events);
                    };
                    events.push(event.clone());
                    if !self.project_and_send_runtime_event(&mut projector, event).await
                    {
                        return None;
                    }
                }
                command = self.command_receiver.recv(), if commands_open => {
                    let Some(command) = command else {
                        commands_open = false;
                        continue;
                    };
                    self
                        .handle_command(command, CommandHandlingMode::Running)
                        .await?;
                }
            }
        }
    }

    async fn handle_step_outcome(&mut self, step_events: &[RuntimeJournalEvent]) -> Option<bool> {
        match classify_step_events(step_events, self.config.final_output_contract()) {
            StepOutcome::Pending(PendingLoopToolCall::Runtime(call)) if !self.interrupted => {
                self.run_runtime_tool(call).await
            }
            StepOutcome::PendingBatch(calls)
                if !self.interrupted
                    && calls
                        .iter()
                        .all(|call| matches!(call, PendingLoopToolCall::Runtime(_))) =>
            {
                self.run_runtime_tool_batch(
                    calls
                        .into_iter()
                        .map(|call| match call {
                            PendingLoopToolCall::Runtime(call) => call,
                            PendingLoopToolCall::Bridge(_)
                            | PendingLoopToolCall::FinalOutput(_) => {
                                unreachable!("guard accepts runtime tool calls only")
                            }
                        })
                        .collect(),
                )
                .await
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

    async fn run_runtime_tool_batch(&mut self, calls: Vec<PendingToolCall>) -> Option<bool> {
        if !self.send_state(InteractiveRunState::RunningTool).await {
            return None;
        }

        let phase_token = self.loop_token.child_token();
        self.phase_token = Some(phase_token.clone());
        let runtime = self.runtime.clone();
        let loop_permit = self.loop_permit.clone();
        let call_ids = calls
            .iter()
            .map(|call| call.id().clone())
            .collect::<Vec<_>>();
        let execution_permit = loop_permit.clone();
        let execution = async move {
            runtime
                .execute_tool_call_batch_with_active_permit(
                    calls,
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

        let (events, error) = result.into_parts();
        if !self.send_runtime_events(events).await {
            return None;
        }

        match error {
            None => Some(!self.interrupted),
            Some(RuntimeError::ToolExecutionCancelled { .. }) => {
                let pending_ids = self
                    .runtime
                    .pending_tool_calls()
                    .await
                    .into_iter()
                    .map(|call| call.id().clone())
                    .collect::<std::collections::BTreeSet<_>>();
                for call_id in call_ids
                    .into_iter()
                    .filter(|call_id| pending_ids.contains(call_id))
                {
                    let events = self
                        .runtime
                        .submit_tool_interrupt_failure_with_active_permit(&call_id, &loop_permit)
                        .await
                        .ok()?;
                    if !self.send_runtime_events(events).await {
                        return None;
                    }
                }
                self.interrupted = true;
                Some(false)
            }
            Some(error) => {
                tracing::debug!(error = %error, "interactive runtime tool batch failed");
                None
            }
        }
    }

    async fn send_runtime_events(&self, events: Vec<RuntimeJournalEvent>) -> bool {
        let mut projector = RuntimeEventProjector::new();
        for event in events {
            if !self
                .project_and_send_runtime_event(&mut projector, event)
                .await
            {
                return false;
            }
        }
        true
    }

    async fn project_and_send_runtime_event(
        &self,
        projector: &mut RuntimeEventProjector,
        event: RuntimeJournalEvent,
    ) -> bool {
        let projected = projector.project(event, &self.runtime).await;
        let Ok(Some(event)) = projected else {
            return true;
        };
        self.event_sender.send(event).await.is_ok()
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
        if self.queue.has_next() {
            self.interrupted = false;
            return BoundaryAction::UserInput {
                accepted: self.queue.accept_next_burst(),
                lane: QueuedInputLane::Next,
            };
        }

        if self.interrupted {
            return BoundaryAction::Wait;
        }

        if self.suspended_resume_requested {
            self.suspended_resume_requested = false;
            let accepted = self.queue.accept_suspended_burst();
            if !accepted.is_empty() {
                return BoundaryAction::UserInput {
                    accepted,
                    lane: QueuedInputLane::Suspended,
                };
            }
        }

        let accepted = self.queue.accept_one_backlog();
        if !accepted.is_empty() {
            return BoundaryAction::UserInput {
                accepted,
                lane: QueuedInputLane::Backlog,
            };
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
            InteractiveCommand::SubmitNext {
                message,
                ack_sender,
            } => {
                let receipt = self.queue.submit_next_message(message);
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
            InteractiveCommand::Enqueue {
                message,
                ack_sender,
            } => {
                let receipt = self.queue.enqueue_message(message);
                let should_run = mode == CommandHandlingMode::Waiting && receipt.is_ok();
                let queue_changed = receipt.is_ok();
                let _ = ack_sender.send(receipt);
                if queue_changed && !self.send_queue_changed().await {
                    return None;
                }
                if should_run {
                    return Some(CommandDecision::RunBacklog);
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::Update {
                id,
                text,
                ack_sender,
            } => {
                let result = self.queue.update(id, &text);
                let queue_changed = result.is_ok();
                let _ = ack_sender.send(result);
                if queue_changed && !self.send_queue_changed().await {
                    return None;
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::Remove { id, ack_sender } => {
                let result = self.queue.remove(id);
                let queue_changed = result.is_ok();
                let _ = ack_sender.send(result);
                if queue_changed && !self.send_queue_changed().await {
                    return None;
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::ReplacePendingOrder {
                lane,
                ids,
                ack_sender,
            } => {
                let result = self.queue.replace_pending_order(lane, ids);
                let queue_changed = result.is_ok();
                let _ = ack_sender.send(result);
                if queue_changed && !self.send_queue_changed().await {
                    return None;
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::Snapshot { ack_sender } => {
                let _ = ack_sender.send(self.queue.input_records());
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::UpdateSettings { update, ack_sender } => {
                let result = if let Some(subagents) = update.subagents {
                    self.runtime
                        .update_interactive_subagents(subagents.enabled, subagents.config)
                        .await
                } else {
                    Ok(())
                };
                if result.is_ok() {
                    if let Some(generation_config) = update.generation_config {
                        self.generation_config = generation_config;
                    }
                    if let Some(primary_model) = update.primary_model {
                        self.runtime
                            .update_interactive_primary_model(
                                primary_model.provider,
                                primary_model.model,
                                primary_model.retry_policy,
                            )
                            .await;
                    }
                    if let Some(automatic_compaction) = update.automatic_compaction {
                        self.runtime
                            .update_interactive_automatic_compaction(automatic_compaction)
                            .await;
                    }
                    if let Some(context_window_tokens) = update.context_window_tokens {
                        self.runtime
                            .update_interactive_context_window_tokens(context_window_tokens)
                            .await;
                    }
                }
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::ResumeSuspended { ack_sender } => {
                self.suspended_resume_requested = true;
                let _ = ack_sender.send(Ok(()));
                if mode == CommandHandlingMode::Waiting {
                    return Some(CommandDecision::RunSuspended);
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::DiscardSuspended { ack_sender } => {
                self.suspended_resume_requested = false;
                let discarded = self.queue.discard_suspended();
                let _ = ack_sender.send(Ok(()));
                if !discarded.is_empty() && !self.send_queue_changed().await {
                    return None;
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
            InteractiveCommand::Close { ack_sender } => {
                let _ = ack_sender.send(Ok(()));
                if mode == CommandHandlingMode::Running {
                    self.loop_token.cancel();
                    if let Some(token) = self.phase_token.as_ref() {
                        token.cancel();
                    }
                    return Some(CommandDecision::Continue);
                }
                Some(CommandDecision::Close)
            }
        }
    }

    async fn interrupt_running_phase(
        &mut self,
        _reason: InterruptReason,
    ) -> Option<CommandDecision> {
        self.interrupted = true;
        self.suspended_resume_requested = false;
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
            .send(RuntimeEvent::QueuedInputsChanged {
                inputs: self.queue.snapshot(),
            })
            .await
            .is_ok()
    }

    async fn send_state(&self, state: InteractiveRunState) -> bool {
        self.event_sender
            .send(RuntimeEvent::InteractiveRunStateChanged { state })
            .await
            .is_ok()
    }
}
