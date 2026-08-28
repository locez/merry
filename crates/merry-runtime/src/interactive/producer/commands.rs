//! Interactive command dispatch and interruption handling.

use super::super::commands::{CommandDecision, CommandHandlingMode, InteractiveCommand};
use super::super::types::{InteractiveError, InterruptReason};
use super::InteractiveProducer;
use merry_core::InteractiveRunState;
use std::sync::atomic::Ordering;

impl InteractiveProducer {
    pub(super) async fn handle_command(
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

    pub(super) async fn interrupt_running_phase(
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
}
