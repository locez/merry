//! Interactive runtime, bridge, and structured-output tool execution.

use super::super::{
    commands::CommandHandlingMode, handles::InteractiveRunMessage, types::InteractiveError,
};
use super::InteractiveProducer;
use crate::agent_loop::{
    PendingLoopToolCall, can_retry_structured_output, record_final_output_tool_call,
    validate_final_output,
};
use crate::{RuntimeError, ToolExecutionContext, bridge::resolve_bridge_tool_result_command};
use merry_core::{
    InteractiveRunState, PendingToolCall, PendingToolCallBatch, RuntimeJournalEvent,
    ToolCallBatchId,
};
use std::{collections::BTreeSet, sync::atomic::Ordering};

enum InteractiveToolWave {
    Runtime(Vec<PendingToolCall>),
    Bridge(Vec<PendingToolCall>),
}

fn next_interactive_batch_id(sequence: &mut u64) -> Result<ToolCallBatchId, merry_core::CoreError> {
    let batch_id = ToolCallBatchId::new(&format!("interactive-batch-{sequence}"))?;
    *sequence = (*sequence).saturating_add(1);
    Ok(batch_id)
}

impl InteractiveProducer {
    pub(super) async fn run_pending_tool_batch(
        &mut self,
        calls: Vec<PendingLoopToolCall>,
    ) -> Option<bool> {
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

    pub(super) async fn run_final_output_tool(&mut self, call: PendingToolCall) -> Option<bool> {
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

    pub(super) async fn finish_structured_output_attempt(
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

    pub(super) async fn reject_mixed_final_output_batch(
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

    pub(super) async fn settle_interrupted_tool_call(
        &mut self,
        call: PendingToolCall,
    ) -> Option<bool> {
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

    pub(super) async fn settle_tool_failure(
        &mut self,
        call: PendingToolCall,
        message: &str,
    ) -> Option<bool> {
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

    pub(super) async fn run_runtime_tool(
        &mut self,
        call: merry_core::PendingToolCall,
    ) -> Option<bool> {
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

    pub(super) async fn run_runtime_tool_batch(
        &mut self,
        calls: Vec<PendingToolCall>,
    ) -> Option<bool> {
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

    pub(super) async fn run_bridge_tool_batch(
        &mut self,
        calls: Vec<PendingToolCall>,
    ) -> Option<bool> {
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

    pub(super) async fn settle_bridge_tool_batch(
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
}
