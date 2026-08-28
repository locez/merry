mod commands;
mod handles;
mod plan;
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
    agent_loop::{
        PendingLoopToolCall, StepOutcome, can_retry_structured_output, classify_step_events,
        record_final_output_tool_call, validate_final_output,
    },
    bridge::{BridgeToolResultCommand, resolve_bridge_tool_result_command},
    events::{ActiveStepPermit, RuntimeEventProjector},
};
use futures_util::StreamExt;
use merry_core::{
    InteractiveRunState, PendingToolCall, PendingToolCallBatch, QueuedInputLane, QueuedInputView,
    RuntimeEvent, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallBatchId,
};
use merry_llm::GenerationConfig;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

pub use self::handles::{
    AgentLoopControl, AgentLoopInput, InteractiveAgentRun, InteractiveInputItem,
    InteractiveInputSnapshot, InteractiveRunEventStream, InteractiveRunMessage,
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
        let (parent_token, generation_config, context_contract) = context.into_parts();
        let config = config
            .merge_context_final_output_contract(context_contract)
            .map_err(|source| RuntimeError::AgentLoopConfig { source })?;
        let loop_token = parent_token.child_token();
        let producer_token = loop_token.clone();
        let subagent_completion_notify = self.subagent_completion_notify();
        let run_id = next_interactive_run_id();
        let (message_sender, message_receiver) = mpsc::channel(16);
        let (command_sender, command_receiver) = mpsc::channel(16);
        let (bridge_sender, bridge_receiver) = mpsc::channel(4);
        let bridge_resolution_epoch = Arc::new(AtomicU64::new(0));
        let plan_event_receiver = self.subscribe_plan_events();
        let producer = InteractiveProducer {
            runtime: self.clone(),
            queue: InteractiveInputQueue::default(),
            command_receiver,
            plan_event_receiver,
            subagent_completion_notify,
            message_sender,
            bridge_receiver,
            bridge_resolution_epoch: Arc::clone(&bridge_resolution_epoch),
            bridge_pending: false,
            bridge_batch_sequence: 0,
            loop_token: producer_token,
            generation_config,
            config,
            loop_permit,
            suspended_resume_requested: false,
            phase_token: None,
            interrupted: false,
            seen_plan_sequences: BTreeSet::new(),
            coordinator_continuation_requested: false,
            coordinator_continuation_note: None,
            subagent_continuation_requested: false,
            model_turns_run: 0,
            structured_output_retries: 0,
            terminal_error: None,
        };
        let producer_handle = tokio::spawn(async move { producer.run().await });

        Ok(InteractiveAgentRun::new(
            InteractiveRunEventStream::new(
                run_id,
                ReceiverStream::new(message_receiver),
                loop_token,
                producer_handle,
                bridge_sender,
                bridge_resolution_epoch,
            ),
            AgentLoopInput::new(run_id, command_sender.clone()),
            AgentLoopControl::new(run_id, command_sender),
        ))
    }
}

fn next_interactive_batch_id(sequence: &mut u64) -> Result<ToolCallBatchId, merry_core::CoreError> {
    let batch_id = ToolCallBatchId::new(&format!("interactive-batch-{sequence}"))?;
    *sequence = (*sequence).saturating_add(1);
    Ok(batch_id)
}

struct InteractiveProducer {
    runtime: Runtime,
    queue: InteractiveInputQueue,
    command_receiver: mpsc::Receiver<InteractiveCommand>,
    plan_event_receiver: crate::plan::PlanControllerEventReceiver,
    subagent_completion_notify: Option<Arc<Notify>>,
    message_sender: mpsc::Sender<InteractiveRunMessage>,
    bridge_receiver: mpsc::Receiver<BridgeToolResultCommand>,
    bridge_resolution_epoch: Arc<AtomicU64>,
    bridge_pending: bool,
    bridge_batch_sequence: u64,
    loop_token: CancellationToken,
    generation_config: GenerationConfig,
    config: AgentLoopConfig,
    loop_permit: ActiveStepPermit,
    suspended_resume_requested: bool,
    phase_token: Option<CancellationToken>,
    interrupted: bool,
    seen_plan_sequences: BTreeSet<u64>,
    coordinator_continuation_requested: bool,
    coordinator_continuation_note: Option<String>,
    subagent_continuation_requested: bool,
    model_turns_run: usize,
    structured_output_retries: usize,
    terminal_error: Option<InteractiveError>,
}

enum InteractiveToolWave {
    Runtime(Vec<PendingToolCall>),
    Bridge(Vec<PendingToolCall>),
}

impl InteractiveProducer {
    async fn run(mut self) -> Result<(), InteractiveError> {
        if !self.send_state(InteractiveRunState::WaitingForInput).await {
            return Ok(());
        }
        while !self.loop_token.is_cancelled() {
            let subagent_completion_notify = self.subagent_completion_notify.clone();
            let wait_for_subagent_completion = async move {
                match subagent_completion_notify {
                    Some(notify) => notify.notified().await,
                    None => std::future::pending::<()>().await,
                }
            };
            let command = tokio::select! {
                command = self.command_receiver.recv() => command,
                plan_event = self.plan_event_receiver.recv() => {
                    if !self.forward_plan_event(plan_event).await {
                        return Ok(());
                    }
                    if self.coordinator_continuation_requested
                        && !self.run_coordinator_continuation().await
                    {
                        return self.stop_result();
                    }
                    continue;
                }
                _ = wait_for_subagent_completion => {
                    // A completion can be acknowledged by wait_subagents while
                    // this Notify permit is still pending. Only start a
                    // continuation when the queue still contains a notification.
                    if !self.runtime.has_subagent_completion_notifications().await {
                        continue;
                    }
                    if !self.run_coordinator_continuation().await {
                        return self.stop_result();
                    }
                    continue;
                }
            };
            let Some(command) = command else {
                break;
            };

            let Some(decision) = self
                .handle_command(command, CommandHandlingMode::Waiting)
                .await
            else {
                return self.stop_result();
            };
            match decision {
                CommandDecision::Continue => {}
                CommandDecision::RunContinuation => {
                    if !self.run_coordinator_continuation().await {
                        return self.stop_result();
                    }
                }
                CommandDecision::RunNext => {
                    if !self.run_next_burst().await {
                        return self.stop_result();
                    }
                }
                CommandDecision::RunSuspended => {
                    if !self.run_suspended_burst().await {
                        return self.stop_result();
                    }
                }
                CommandDecision::RunBacklog => {
                    if !self.run_one_backlog().await {
                        return self.stop_result();
                    }
                }
                CommandDecision::Close => {
                    break;
                }
            }
        }

        let _ = self
            .send_event(RuntimeEvent::InteractiveRunStateChanged {
                state: InteractiveRunState::Closed,
            })
            .await;
        self.runtime.close_trajectory();
        let event = RuntimeEvent::Closed;
        let _ = self.send_event(event).await;
        Ok(())
    }

    fn stop_result(&mut self) -> Result<(), InteractiveError> {
        self.terminal_error.take().map_or(Ok(()), Err)
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

            self.refresh_subagent_continuation_request().await;
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
        let mut first_step = true;
        loop {
            let notification_input = self.take_subagent_notification_input().await;
            let input = if let Some(input) = notification_input {
                input
            } else if first_step {
                first_step = false;
                match self.coordinator_continuation_note.take() {
                    Some(note) => match StepInput::loop_control_text(&note) {
                        Ok(input) => input,
                        Err(error) => {
                            self.remember_terminal_error(InteractiveError::Runtime {
                                source: error,
                            });
                            return None;
                        }
                    },
                    None => StepInput::no_new_user_input(),
                }
            } else {
                StepInput::no_new_user_input()
            };
            let step_events = self.run_model_phase(input, None).await?;
            let continuation_required = self.handle_step_outcome(&step_events).await?;
            if !self.drain_ready_commands().await {
                return None;
            }
            self.refresh_subagent_continuation_request().await;
            let boundary = self.boundary_action(continuation_required);
            if matches!(boundary, BoundaryAction::Continuation) {
                continue;
            }
            return Some(boundary);
        }
    }

    async fn take_subagent_notification_input(&self) -> Option<StepInput> {
        let statuses = self.runtime.take_subagent_completion_notifications().await;
        if statuses.is_empty() {
            return None;
        }
        let text = crate::subagent::completion_notification_text(&statuses);
        match StepInput::loop_control_text(&text) {
            Ok(input) => Some(input),
            Err(error) => {
                tracing::warn!(%error, "discarding invalid subagent completion notification");
                None
            }
        }
    }

    async fn run_coordinator_continuation(&mut self) -> bool {
        self.coordinator_continuation_requested = false;
        let Some(boundary) = self.run_continuation_steps().await else {
            return false;
        };
        match boundary {
            BoundaryAction::UserInput { accepted, lane } => {
                self.run_accepted_steps(accepted, lane).await
            }
            BoundaryAction::Wait => self.send_state(InteractiveRunState::WaitingForInput).await,
            BoundaryAction::Continuation => true,
        }
    }

    async fn run_model_phase(
        &mut self,
        input: StepInput,
        accepted: Option<(&[AcceptedQueuedInput], QueuedInputLane)>,
    ) -> Option<Vec<RuntimeJournalEvent>> {
        if let Some((accepted, lane)) = accepted {
            self.model_turns_run = 0;
            self.structured_output_retries = 0;
            let inputs: Vec<QueuedInputView> =
                accepted.iter().map(|item| item.view().clone()).collect();
            self.runtime.record_queued_input_accepted(&inputs);
            let event = RuntimeEvent::QueuedInputAccepted { lane, inputs };
            if !self.send_event(event).await {
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
                self.remember_terminal_error(InteractiveError::Runtime { source: error });
                return None;
            }
        };
        self.model_turns_run = self.model_turns_run.saturating_add(1);
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
                plan_event = self.plan_event_receiver.recv() => {
                    if !self.forward_plan_event(plan_event).await {
                        return None;
                    }
                }
            }
        }
    }

    async fn handle_step_outcome(&mut self, step_events: &[RuntimeJournalEvent]) -> Option<bool> {
        match classify_step_events(step_events, self.config.final_output_contract()) {
            StepOutcome::Pending(PendingLoopToolCall::Runtime(call)) if !self.interrupted => {
                self.run_runtime_tool(call).await
            }
            StepOutcome::Pending(PendingLoopToolCall::Bridge(call)) if !self.interrupted => {
                self.run_bridge_tool_batch(vec![call]).await
            }
            StepOutcome::Pending(PendingLoopToolCall::FinalOutput(call)) => {
                if self.interrupted {
                    self.settle_interrupted_tool_call(call).await
                } else {
                    self.run_final_output_tool(call).await
                }
            }
            StepOutcome::PendingBatch(calls) if !self.interrupted => {
                self.run_pending_tool_batch(calls).await
            }
            StepOutcome::ToolResultRecorded => Some(!self.interrupted),
            StepOutcome::Completed => {
                self.subagent_continuation_requested =
                    self.runtime.has_subagent_completion_notifications().await;
                Some(false)
            }
            StepOutcome::Failed(_) | StepOutcome::Cancelled(_) | StepOutcome::Blocked(_) => {
                Some(false)
            }
            StepOutcome::Pending(_) | StepOutcome::PendingBatch(_) => Some(false),
        }
    }

    async fn run_pending_tool_batch(&mut self, calls: Vec<PendingLoopToolCall>) -> Option<bool> {
        if calls
            .iter()
            .any(|call| matches!(call, PendingLoopToolCall::FinalOutput(_)))
        {
            return self.reject_mixed_final_output_batch(calls).await;
        }

        let mut waves = Vec::new();
        for call in calls {
            match call {
                PendingLoopToolCall::Runtime(call) => match waves.last_mut() {
                    Some(InteractiveToolWave::Runtime(wave)) => wave.push(call),
                    _ => waves.push(InteractiveToolWave::Runtime(vec![call])),
                },
                PendingLoopToolCall::Bridge(call) => match waves.last_mut() {
                    Some(InteractiveToolWave::Bridge(wave)) => wave.push(call),
                    _ => waves.push(InteractiveToolWave::Bridge(vec![call])),
                },
                PendingLoopToolCall::FinalOutput(_) => unreachable!(
                    "mixed final-output batches are handled before tool-wave partitioning"
                ),
            }
        }

        for wave in waves {
            let should_continue = match wave {
                InteractiveToolWave::Runtime(calls) => self.run_runtime_tool_batch(calls).await?,
                InteractiveToolWave::Bridge(calls) => self.run_bridge_tool_batch(calls).await?,
            };
            if !should_continue {
                return Some(false);
            }
        }
        Some(true)
    }

    async fn run_final_output_tool(&mut self, call: PendingToolCall) -> Option<bool> {
        let Some(contract) = self.config.final_output_contract().cloned() else {
            tracing::error!(
                call_id = %call.id(),
                "interactive final-output call has no configured contract"
            );
            return self
                .settle_tool_failure(
                    call,
                    "interactive final-output call has no configured contract",
                )
                .await;
        };

        if let Err(error) = contract.validate_call(&call) {
            let message = error.message();
            let events = match self
                .runtime
                .submit_tool_input_validation_failure_with_active_permit(
                    &call,
                    error,
                    &self.loop_permit,
                )
                .await
            {
                Ok(events) => events,
                Err(source) => {
                    self.remember_terminal_error(InteractiveError::Runtime { source });
                    return None;
                }
            };
            tracing::debug!(
                call_id = %call.id(),
                error = %message,
                "interactive structured output arguments rejected"
            );
            return self.finish_structured_output_attempt(events).await;
        }

        if let Err(message) = validate_final_output(&contract, &call) {
            let events = match self
                .runtime
                .submit_structured_output_failure_with_active_permit(
                    call.id(),
                    &message,
                    &self.loop_permit,
                )
                .await
            {
                Ok(events) => events,
                Err(source) => {
                    self.remember_terminal_error(InteractiveError::Runtime { source });
                    return None;
                }
            };
            tracing::debug!(
                call_id = %call.id(),
                error = %message,
                "interactive structured output decoder rejected the result"
            );
            return self.finish_structured_output_attempt(events).await;
        }

        let events = match record_final_output_tool_call(&self.runtime, call).await {
            Ok((_final_output, events)) => events,
            Err(source) => {
                self.remember_terminal_error(InteractiveError::Runtime { source });
                return None;
            }
        };
        if !self.send_runtime_events(events).await {
            return None;
        }
        self.structured_output_retries = 0;
        Some(false)
    }

    async fn finish_structured_output_attempt(
        &mut self,
        events: Vec<RuntimeJournalEvent>,
    ) -> Option<bool> {
        if !self.send_runtime_events(events).await {
            return None;
        }
        self.structured_output_retries = self.structured_output_retries.saturating_add(1);
        Some(can_retry_structured_output(
            &self.config,
            self.structured_output_retries,
            self.model_turns_run,
        ))
    }

    async fn reject_mixed_final_output_batch(
        &mut self,
        calls: Vec<PendingLoopToolCall>,
    ) -> Option<bool> {
        const MESSAGE: &str = "final-output tool calls must be the only call in their model batch";

        for call in calls {
            let (call, is_final_output) = match call {
                PendingLoopToolCall::FinalOutput(call) => (call, true),
                PendingLoopToolCall::Runtime(call) | PendingLoopToolCall::Bridge(call) => {
                    (call, false)
                }
            };
            let events = if is_final_output {
                self.runtime
                    .submit_structured_output_failure_with_active_permit(
                        call.id(),
                        MESSAGE,
                        &self.loop_permit,
                    )
                    .await
            } else {
                self.runtime
                    .submit_tool_execution_failure_with_active_permit(
                        call.id(),
                        MESSAGE,
                        &self.loop_permit,
                    )
                    .await
            };
            let events = match events {
                Ok(events) => events,
                Err(source) => {
                    self.remember_terminal_error(InteractiveError::Runtime { source });
                    return None;
                }
            };
            if !self.send_runtime_events(events).await {
                return None;
            }
        }
        Some(false)
    }

    async fn settle_interrupted_tool_call(&mut self, call: PendingToolCall) -> Option<bool> {
        let events = match self
            .runtime
            .submit_tool_interrupt_failure_with_active_permit(call.id(), &self.loop_permit)
            .await
        {
            Ok(events) => events,
            Err(source) => {
                self.remember_terminal_error(InteractiveError::Runtime { source });
                return None;
            }
        };
        self.interrupted = true;
        if !self.send_runtime_events(events).await {
            return None;
        }
        Some(false)
    }

    async fn settle_tool_failure(&mut self, call: PendingToolCall, message: &str) -> Option<bool> {
        let events = match self
            .runtime
            .submit_tool_execution_failure_with_active_permit(call.id(), message, &self.loop_permit)
            .await
        {
            Ok(events) => events,
            Err(source) => {
                self.remember_terminal_error(InteractiveError::Runtime { source });
                return None;
            }
        };
        if !self.send_runtime_events(events).await {
            return None;
        }
        Some(false)
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
                plan_event = self.plan_event_receiver.recv() => {
                    if !self.forward_plan_event(plan_event).await {
                        return None;
                    }
                }
            }
        };
        self.phase_token = None;

        match result {
            Ok(events) => {
                if !self.send_runtime_events(events).await {
                    return None;
                }
                self.runtime_tool_continuation().await
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
                        self.remember_terminal_error(InteractiveError::Runtime { source: error });
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
                let message = error.to_string();
                let events = match self
                    .runtime
                    .submit_tool_execution_failure_with_active_permit(
                        &call_id,
                        &message,
                        &loop_permit,
                    )
                    .await
                {
                    Ok(events) => events,
                    Err(error) => {
                        self.remember_terminal_error(InteractiveError::Runtime { source: error });
                        return None;
                    }
                };
                if !self.send_runtime_events(events).await {
                    return None;
                }
                self.runtime_tool_continuation().await
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
                plan_event = self.plan_event_receiver.recv() => {
                    if !self.forward_plan_event(plan_event).await {
                        return None;
                    }
                }
            }
        };
        self.phase_token = None;

        let (events, error) = result.into_parts();
        if !self.send_runtime_events(events).await {
            return None;
        }

        match error {
            None => self.runtime_tool_continuation().await,
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
                    let events = match self
                        .runtime
                        .submit_tool_interrupt_failure_with_active_permit(&call_id, &loop_permit)
                        .await
                    {
                        Ok(events) => events,
                        Err(error) => {
                            self.remember_terminal_error(InteractiveError::Runtime {
                                source: error,
                            });
                            return None;
                        }
                    };
                    if !self.send_runtime_events(events).await {
                        return None;
                    }
                }
                self.interrupted = true;
                Some(false)
            }
            Some(error) => {
                tracing::debug!(error = %error, "interactive runtime tool batch failed");
                let message = error.to_string();
                let pending_calls = self
                    .runtime
                    .pending_tool_calls()
                    .await
                    .into_iter()
                    .filter(|call| call_ids.contains(call.id()))
                    .collect::<Vec<_>>();
                for call in pending_calls {
                    let events = match self
                        .runtime
                        .submit_tool_execution_failure_with_active_permit(
                            call.id(),
                            &message,
                            &loop_permit,
                        )
                        .await
                    {
                        Ok(events) => events,
                        Err(error) => {
                            self.remember_terminal_error(InteractiveError::Runtime {
                                source: error,
                            });
                            return None;
                        }
                    };
                    if !self.send_runtime_events(events).await {
                        return None;
                    }
                }
                self.runtime_tool_continuation().await
            }
        }
    }

    async fn run_bridge_tool_batch(&mut self, calls: Vec<PendingToolCall>) -> Option<bool> {
        let Some(first_call) = calls.first() else {
            return Some(false);
        };
        if !self.send_state(InteractiveRunState::RunningTool).await {
            return None;
        }

        let batch_id = match next_interactive_batch_id(&mut self.bridge_batch_sequence) {
            Ok(batch_id) => batch_id,
            Err(error) => {
                tracing::debug!(error = %error, "interactive bridge batch id creation failed");
                return self
                    .settle_bridge_tool_batch(&calls, false, &error.to_string())
                    .await;
            }
        };
        let batch = match PendingToolCallBatch::new(batch_id, calls.clone()) {
            Ok(batch) => batch,
            Err(error) => {
                tracing::debug!(error = %error, "interactive bridge batch creation failed");
                return self
                    .settle_bridge_tool_batch(&calls, false, &error.to_string())
                    .await;
            }
        };

        let phase_token = self.loop_token.child_token();
        self.phase_token = Some(phase_token.clone());
        if self
            .message_sender
            .send(InteractiveRunMessage::ToolInvocations {
                batch: batch.clone(),
            })
            .await
            .is_err()
        {
            self.phase_token = None;
            return self
                .settle_bridge_tool_batch(
                    &calls,
                    self.loop_token.is_cancelled(),
                    "interactive bridge host disconnected before executing the tool",
                )
                .await;
        }
        self.bridge_pending = true;

        let mut commands_open = true;
        let result = loop {
            tokio::select! {
                command = self.bridge_receiver.recv() => {
                    let Some(command) = command else {
                        break Err(RuntimeError::AgentRunClosed {
                            session_id: self.runtime.session_id().clone(),
                            message: "interactive bridge result channel closed before invocations were resolved",
                        });
                    };
                    let (ack_sender, result) = resolve_bridge_tool_result_command(
                        &self.runtime,
                        &batch,
                        command,
                        &self.loop_permit,
                    )
                    .await;
                    match result {
                        Ok(events) => {
                            self.bridge_pending = false;
                            self.bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                            let _ = ack_sender.send(Ok(()));
                            break Ok(events);
                        }
                        Err(error) if error.is_retryable_bridge_tool_result() => {
                            let _ = ack_sender.send(Err(error));
                        }
                        Err(error) => {
                            let message = error.to_string();
                            let _ = ack_sender.send(Err(RuntimeError::BridgeToolResultRejected {
                                session_id: self.runtime.session_id().clone(),
                                message: message.clone(),
                            }));
                            break Err(RuntimeError::BridgeToolResultRejected {
                                session_id: self.runtime.session_id().clone(),
                                message,
                            });
                        }
                    }
                }
                command = self.command_receiver.recv(), if commands_open => {
                    let Some(command) = command else {
                        commands_open = false;
                        continue;
                    };
                    self.handle_command(command, CommandHandlingMode::Running).await?;
                }
                plan_event = self.plan_event_receiver.recv() => {
                    if !self.forward_plan_event(plan_event).await {
                        return None;
                    }
                }
                () = phase_token.cancelled() => {
                    break Err(RuntimeError::ToolExecutionCancelled {
                        session_id: self.runtime.session_id().clone(),
                        call_id: first_call.id().clone(),
                    });
                }
            }
        };
        self.phase_token = None;

        match result {
            Ok(events) => {
                if !self.send_runtime_events(events).await {
                    return None;
                }
                self.runtime_tool_continuation().await
            }
            Err(RuntimeError::ToolExecutionCancelled { .. }) => {
                self.settle_bridge_tool_batch(&calls, true, "bridge tool was interrupted")
                    .await
            }
            Err(error) => {
                tracing::debug!(error = %error, "interactive bridge tool failed");
                self.settle_bridge_tool_batch(&calls, false, &error.to_string())
                    .await
            }
        }
    }

    async fn settle_bridge_tool_batch(
        &mut self,
        calls: &[PendingToolCall],
        interrupted: bool,
        message: &str,
    ) -> Option<bool> {
        self.bridge_pending = false;
        let pending_ids = self
            .runtime
            .pending_tool_calls()
            .await
            .into_iter()
            .map(|call| call.id().clone())
            .collect::<BTreeSet<_>>();
        let mut settlement_failed = false;
        let mut output_open = true;
        for call in calls.iter().filter(|call| pending_ids.contains(call.id())) {
            let events = if interrupted {
                self.runtime
                    .submit_tool_interrupt_failure_with_active_permit(call.id(), &self.loop_permit)
                    .await
            } else {
                self.runtime
                    .submit_tool_execution_failure_with_active_permit(
                        call.id(),
                        message,
                        &self.loop_permit,
                    )
                    .await
            };
            let events = match events {
                Ok(events) => events,
                Err(error) => {
                    tracing::debug!(error = %error, "interactive bridge failure settlement failed");
                    self.remember_terminal_error(InteractiveError::Runtime { source: error });
                    settlement_failed = true;
                    continue;
                }
            };
            if output_open && !self.send_runtime_events(events).await {
                output_open = false;
            }
        }
        self.bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
        if settlement_failed || !output_open {
            return None;
        }
        if interrupted {
            self.interrupted = true;
            Some(false)
        } else {
            self.runtime_tool_continuation().await
        }
    }

    async fn send_runtime_events(&mut self, events: Vec<RuntimeJournalEvent>) -> bool {
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
        &mut self,
        projector: &mut RuntimeEventProjector,
        event: RuntimeJournalEvent,
    ) -> bool {
        if is_plan_payload(&event.payload) {
            if !self.seen_plan_sequences.insert(event.sequence) {
                return true;
            }
            if self.seen_plan_sequences.len() > 256
                && let Some(oldest) = self.seen_plan_sequences.first().copied()
            {
                self.seen_plan_sequences.remove(&oldest);
            }
        }
        let event = match projector.project(event, &self.runtime).await {
            Ok(Some(event)) => event,
            Ok(None) => return true,
            Err(error) => {
                self.remember_terminal_error(InteractiveError::Runtime { source: error });
                return false;
            }
        };
        self.send_event(event).await
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

    async fn refresh_subagent_continuation_request(&mut self) {
        if self.runtime.has_subagent_completion_notifications().await {
            self.subagent_continuation_requested = true;
        }
    }

    fn boundary_action(&mut self, continuation_required: bool) -> BoundaryAction {
        if self.interrupted {
            return BoundaryAction::Wait;
        }

        if self.subagent_continuation_requested {
            self.subagent_continuation_requested = false;
            return BoundaryAction::Continuation;
        }

        if self.queue.has_next() {
            self.interrupted = false;
            return BoundaryAction::UserInput {
                accepted: self.queue.accept_next_burst(),
                lane: QueuedInputLane::Next,
            };
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
            self.coordinator_continuation_requested = false;
            return BoundaryAction::Continuation;
        }

        if self.coordinator_continuation_requested {
            self.coordinator_continuation_requested = false;
            return BoundaryAction::Continuation;
        }

        BoundaryAction::Wait
    }

    fn remember_terminal_error(&mut self, error: InteractiveError) {
        if self.terminal_error.is_none() {
            self.terminal_error = Some(error);
        }
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
            InteractiveCommand::SaveSession { store, ack_sender } => {
                let result = if mode == CommandHandlingMode::Waiting {
                    self.runtime
                        .save_session_to_with_active_permit(store, &self.loop_permit)
                        .await
                        .map_err(InteractiveError::from)
                } else {
                    Err(InteractiveError::SessionSaveRequiresIdle)
                };
                let _ = ack_sender.send(result);
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::EnterPlanMode { reason, ack_sender } => {
                let result = if mode == CommandHandlingMode::Waiting {
                    self.runtime.enter_plan_mode(&reason).await.map(|_| ())
                } else {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                };
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::ApprovePlan { input, ack_sender } => {
                let result = if mode == CommandHandlingMode::Waiting {
                    self.runtime.approve_plan(input).await.map(|_| ())
                } else {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                };
                let should_continue = result.is_ok();
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
                if should_continue {
                    self.coordinator_continuation_note = Some(
                        "The user approved the current Plan through the Plan UI. Continue the approved execution now; do not ask for approval again.".to_owned(),
                    );
                    Some(CommandDecision::RunContinuation)
                } else {
                    Some(CommandDecision::Continue)
                }
            }
            InteractiveCommand::RevisePlan { reason, ack_sender } => {
                let result = if mode == CommandHandlingMode::Waiting {
                    self.runtime.revise_plan(&reason).await.map(|_| ())
                } else {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                };
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::PausePlanScheduling { reason, ack_sender } => {
                let result = if mode == CommandHandlingMode::Waiting {
                    self.runtime
                        .pause_plan_scheduling(&reason)
                        .await
                        .map(|_| ())
                } else {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                };
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::ResumePlanScheduling { reason, ack_sender } => {
                let result = if mode == CommandHandlingMode::Waiting {
                    self.runtime
                        .resume_plan_scheduling(&reason)
                        .await
                        .map(|_| ())
                } else {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                };
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::RetryInterruptedPlanNode {
                node_id,
                reason,
                ack_sender,
            } => {
                let result = if mode == CommandHandlingMode::Waiting {
                    self.runtime
                        .retry_interrupted_plan_node(node_id, &reason)
                        .await
                        .map(|_| ())
                } else {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                };
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::CancelPlan { reason, ack_sender } => {
                let result = if mode == CommandHandlingMode::Waiting {
                    self.runtime.cancel_plan(&reason).await.map(|_| ())
                } else {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                };
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
                Some(CommandDecision::Continue)
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
        if self.bridge_pending {
            self.bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
        }
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
        self.send_event(RuntimeEvent::QueuedInputsChanged {
            inputs: self.queue.snapshot(),
        })
        .await
    }

    async fn send_state(&self, state: InteractiveRunState) -> bool {
        self.send_event(RuntimeEvent::InteractiveRunStateChanged { state })
            .await
    }

    async fn send_event(&self, event: RuntimeEvent) -> bool {
        self.message_sender
            .send(InteractiveRunMessage::Event(event))
            .await
            .is_ok()
    }
}

fn is_plan_payload(payload: &RuntimeJournalPayload) -> bool {
    matches!(
        payload,
        RuntimeJournalPayload::PlanUpdated { .. }
            | RuntimeJournalPayload::PlanPhaseChanged { .. }
            | RuntimeJournalPayload::PlanNodeReady { .. }
            | RuntimeJournalPayload::PlanLeaseStarted { .. }
            | RuntimeJournalPayload::PlanProgressUpdated { .. }
            | RuntimeJournalPayload::PlanProgressReviewRequested { .. }
            | RuntimeJournalPayload::PlanAttemptProgressReported { .. }
            | RuntimeJournalPayload::PlanDirectiveUpdated { .. }
            | RuntimeJournalPayload::PlanAttemptFinished { .. }
    )
}
