//! Interactive model-step execution and outcome classification.

use super::super::{
    commands::CommandHandlingMode, queue::AcceptedQueuedInput, types::InteractiveError,
};
use super::InteractiveProducer;
use crate::agent_loop::{PendingLoopToolCall, StepOutcome, classify_step_events};
use crate::{StepContext, StepInput, events::RuntimeEventProjector};
use futures_util::StreamExt;
use merry_core::{
    InteractiveRunState, QueuedInputLane, QueuedInputView, RuntimeEvent, RuntimeJournalEvent,
};

impl InteractiveProducer {
    pub(super) async fn run_model_phase(
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

    pub(super) async fn forward_step_until_boundary(
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

    pub(super) async fn handle_step_outcome(
        &mut self,
        step_events: &[RuntimeJournalEvent],
    ) -> Option<bool> {
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
}
