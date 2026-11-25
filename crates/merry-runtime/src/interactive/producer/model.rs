//! Interactive model-step execution and outcome classification.

use super::super::{
    commands::CommandHandlingMode, queue::AcceptedQueuedInput, types::InteractiveError,
};
use super::InteractiveProducer;
use crate::agent_loop::{
    PendingLoopToolCall, StepOutcome, classify_step_events, final_assistant_output_from_step,
};
use crate::{
    AfterModelDecision, AfterModelStepContext, BeforeModelDecision, BeforeModelStepContext,
    RuntimeError, StepContext, StepInput,
    events::RuntimeEventProjector,
    hooks::bridge::{
        StreamHookBridge, evaluate_after_model_step, evaluate_before_model_step,
        has_after_model_hook, has_before_model_hook,
    },
};
use futures_util::StreamExt;
use merry_core::{InteractiveRunState, QueuedInputView, RuntimeEvent, RuntimeJournalEvent};

impl InteractiveProducer {
    pub(super) async fn run_model_phase(
        &mut self,
        mut input: StepInput,
        accepted: Option<&[AcceptedQueuedInput]>,
    ) -> Option<Vec<RuntimeJournalEvent>> {
        self.hook_stopped_turn = false;
        if let Some(accepted) = accepted {
            self.model_turns_run = 0;
            self.structured_output_retries = 0;
            let inputs: Vec<QueuedInputView> =
                accepted.iter().map(|item| item.view().clone()).collect();
            self.runtime.record_queued_input_accepted(&inputs);
            let event = RuntimeEvent::QueuedInputAccepted { inputs };
            if !self.send_event(event).await {
                return None;
            }
            if !self.send_queue_changed().await {
                return None;
            }
        }

        let phase_token = self.loop_token.child_token();
        self.phase_token = Some(phase_token.clone());

        let mut step_generation_config = self.generation_config.clone();
        if has_before_model_hook(&self.runtime, true) {
            let context = BeforeModelStepContext {
                session_id: self.runtime.session_id().clone(),
                model_turn: self.model_turns_run.saturating_add(1),
                max_model_turns: self.config.max_model_turns(),
                session_usage: self.runtime.usage().await,
                generation_config: step_generation_config.clone(),
            };
            let mut hook_bridge = StreamHookBridge {
                sender: &self.message_sender,
                receiver: &mut self.hook_receiver,
                resolution_epoch: &self.hook_resolution_epoch,
                sequence: &mut self.hook_sequence,
            };
            match evaluate_before_model_step(
                &self.runtime,
                context,
                &phase_token,
                Some(&mut hook_bridge),
            )
            .await
            {
                Ok(BeforeModelDecision::Continue {
                    generation_override,
                    dynamic_steering_note,
                    force_compact,
                }) => {
                    if let Some(override_cfg) = generation_override {
                        step_generation_config = override_cfg;
                    }
                    if let Some(note) = dynamic_steering_note
                        && !note.trim().is_empty()
                        && let Err(source) = input.append_control_note(&note)
                    {
                        self.phase_token = None;
                        self.remember_terminal_error(InteractiveError::Runtime { source });
                        return None;
                    }
                    if force_compact
                        && let Err(source) = self
                            .runtime
                            .compact_context_for_hook_with_active_permit(
                                phase_token.clone(),
                                &self.loop_permit,
                            )
                            .await
                    {
                        self.phase_token = None;
                        self.remember_terminal_error(InteractiveError::Runtime { source });
                        return None;
                    }
                }
                Ok(BeforeModelDecision::StopTurn { .. }) => {
                    self.phase_token = None;
                    self.hook_stopped_turn = true;
                    return Some(Vec::new());
                }
                Err(RuntimeError::HookCancelled { .. }) => {
                    self.phase_token = None;
                    self.interrupted = true;
                    self.hook_stopped_turn = true;
                    return Some(Vec::new());
                }
                Err(source) => {
                    self.phase_token = None;
                    self.remember_terminal_error(InteractiveError::Runtime { source });
                    return None;
                }
            }
        }

        if !self.send_state(InteractiveRunState::RunningModel).await {
            self.phase_token = None;
            return None;
        }

        let mut step_context =
            StepContext::new(phase_token).with_generation_config(step_generation_config);
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
                    if !event.payload.is_transient() {
                        events.push(event.clone());
                    }
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
                subagent_event = self.subagent_event_receiver.recv() => {
                    if !self.forward_subagent_event(subagent_event).await {
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
        if self.hook_stopped_turn {
            self.hook_stopped_turn = false;
            return Some(false);
        }

        let outcome = classify_step_events(step_events, self.config.final_output_contract());

        if !self.interrupted
            && has_after_model_hook(&self.runtime, true)
            && matches!(
                outcome,
                StepOutcome::Completed
                    | StepOutcome::Pending(
                        PendingLoopToolCall::Runtime(_) | PendingLoopToolCall::Bridge(_)
                    )
                    | StepOutcome::PendingBatch(_)
            )
        {
            let pending_tool_names = match &outcome {
                StepOutcome::Pending(
                    PendingLoopToolCall::Runtime(call) | PendingLoopToolCall::Bridge(call),
                ) => vec![call.name().clone()],
                StepOutcome::PendingBatch(calls) => calls
                    .iter()
                    .map(|call| match call {
                        PendingLoopToolCall::Runtime(c)
                        | PendingLoopToolCall::Bridge(c)
                        | PendingLoopToolCall::FinalOutput(c) => c.name().clone(),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let assistant_text = final_assistant_output_from_step(&self.runtime, step_events).await;
            let context = AfterModelStepContext {
                session_id: self.runtime.session_id().clone(),
                model_turn: self.model_turns_run,
                max_model_turns: self.config.max_model_turns(),
                assistant_text,
                pending_tool_names,
                is_final_turn: matches!(outcome, StepOutcome::Completed),
                session_usage: self.runtime.usage().await,
            };
            let phase_token = self.loop_token.child_token();
            self.phase_token = Some(phase_token.clone());
            let mut hook_bridge = StreamHookBridge {
                sender: &self.message_sender,
                receiver: &mut self.hook_receiver,
                resolution_epoch: &self.hook_resolution_epoch,
                sequence: &mut self.hook_sequence,
            };
            let decision = evaluate_after_model_step(
                &self.runtime,
                context,
                &phase_token,
                Some(&mut hook_bridge),
            )
            .await;
            self.phase_token = None;
            match decision {
                Ok(AfterModelDecision::Accept) => {}
                Ok(AfterModelDecision::ContinueWithInput { text }) => {
                    let settled = match self
                        .runtime
                        .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                            "skipped by after_model_step hook continuation",
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
                    if !settled.is_empty() && !self.send_runtime_events(settled).await {
                        return None;
                    }
                    if self.model_turns_run >= self.config.max_model_turns() {
                        return Some(false);
                    }
                    self.hook_continuation_note = Some(text);
                    return Some(true);
                }
                Ok(AfterModelDecision::StopTurn { reason, .. }) => {
                    let settled = match self
                        .runtime
                        .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                            &reason,
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
                    if !settled.is_empty() && !self.send_runtime_events(settled).await {
                        return None;
                    }
                    return Some(false);
                }
                Err(RuntimeError::HookCancelled { .. }) => {
                    let _ = self
                        .runtime
                        .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                            "cancelled during after_model_step hook",
                            &self.loop_permit,
                        )
                        .await;
                    self.interrupted = true;
                    return Some(false);
                }
                Err(source) => {
                    let _ = self
                        .runtime
                        .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                            "failed during after_model_step hook",
                            &self.loop_permit,
                        )
                        .await;
                    self.remember_terminal_error(InteractiveError::Runtime { source });
                    return None;
                }
            }
        }

        match outcome {
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
