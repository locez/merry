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
            InteractiveCommand::Submit {
                message,
                ack_sender,
            } => {
                let receipt = self.queue.submit_message(message);
                let queue_changed = receipt.is_ok();
                let should_run =
                    mode == CommandHandlingMode::Waiting && self.queue.can_auto_dispatch();
                let _ = ack_sender.send(receipt);
                if queue_changed && !self.send_queue_changed().await {
                    return None;
                }
                if should_run {
                    return Some(CommandDecision::RunPending);
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
            InteractiveCommand::ReplacePendingOrder { ids, ack_sender } => {
                let result = self.queue.replace_pending_order(ids);
                let queue_changed = result.is_ok();
                let _ = ack_sender.send(result);
                if queue_changed && !self.send_queue_changed().await {
                    return None;
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::SetPendingPaused { paused, ack_sender } => {
                let changed = self.queue.set_paused(paused);
                let should_run = !paused
                    && mode == CommandHandlingMode::Waiting
                    && self.queue.can_auto_dispatch();
                let _ = ack_sender.send(Ok(()));
                if changed && !self.send_queue_changed().await {
                    return None;
                }
                if should_run {
                    return Some(CommandDecision::RunPending);
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::FlushPending { ack_sender } => {
                let changed = self.queue.set_paused(false);
                let should_run = mode == CommandHandlingMode::Waiting && self.queue.has_pending();
                let _ = ack_sender.send(Ok(()));
                if changed && !self.send_queue_changed().await {
                    return None;
                }
                if should_run {
                    return Some(CommandDecision::RunPending);
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
                        self.runtime
                            .update_interactive_generation_config(generation_config.clone());
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
                    for (role, model) in update.role_models {
                        self.runtime
                            .update_interactive_role_model(role, model)
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
                    if let Some(limit) = update.max_parallel_tool_calls {
                        self.runtime
                            .update_interactive_max_parallel_tool_calls(limit);
                    }
                    if let Some(source) = update.permission_admission_source {
                        self.runtime
                            .update_interactive_permission_admission_source(source)
                            .await;
                    }
                    if let Some(mode) = update.permission_review_mode {
                        self.runtime
                            .update_interactive_permission_review_mode(mode)
                            .await;
                    }
                }
                let _ = ack_sender.send(result.map_err(InteractiveError::from));
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
            InteractiveCommand::CompactContext { focus, ack_sender } => {
                if mode != CommandHandlingMode::Waiting {
                    let _ = ack_sender.send(Err(InteractiveError::CompactRequiresIdle));
                    return Some(CommandDecision::Continue);
                }
                Some(CommandDecision::RunCompaction { focus, ack_sender })
            }
            InteractiveCommand::SetPlanMode {
                enabled,
                ack_sender,
            } => {
                let turn_in_flight = mode == CommandHandlingMode::Running;
                match self.runtime.set_plan_mode(enabled, turn_in_flight).await {
                    Ok(event) => {
                        let _ = ack_sender.send(Ok(()));
                        if let Some(event) = event
                            && !self.send_runtime_events(vec![event]).await
                        {
                            return None;
                        }
                    }
                    Err(error) => {
                        let _ = ack_sender.send(Err(InteractiveError::from(error)));
                    }
                }
                Some(CommandDecision::Continue)
            }
            InteractiveCommand::ApprovePlan {
                plan_id,
                note,
                ack_sender,
            } => {
                if mode != CommandHandlingMode::Waiting {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                }
                match self.runtime.approve_plan(plan_id, note.clone()).await {
                    Ok((doc, events)) => {
                        let _ = ack_sender.send(Ok(()));
                        if !self.send_runtime_events(events).await {
                            return None;
                        }
                        let mut message = format!(
                            "The user approved plan \"{}\" (`{}`). Plan Mode is now off and workspace edits are enabled. Proceed with implementing the approved plan.",
                            doc.summary.title, doc.summary.file_name
                        );
                        if let Some(note) = note.as_deref().map(str::trim).filter(|n| !n.is_empty())
                        {
                            message.push_str("\n\nUser notes attached to approval:\n");
                            message.push_str(note);
                        }
                        self.coordinator_continuation_note = Some(message);
                        Some(CommandDecision::RunContinuation)
                    }
                    Err(error) => {
                        let _ = ack_sender.send(Err(InteractiveError::from(error)));
                        Some(CommandDecision::Continue)
                    }
                }
            }
            InteractiveCommand::RequestPlanChanges {
                plan_id,
                feedback,
                ack_sender,
            } => {
                if mode != CommandHandlingMode::Waiting {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                }
                match self
                    .runtime
                    .request_plan_changes(plan_id, feedback.clone())
                    .await
                {
                    Ok((doc, events)) => {
                        let _ = ack_sender.send(Ok(()));
                        if !self.send_runtime_events(events).await {
                            return None;
                        }
                        self.coordinator_continuation_note = Some(format!(
                            "The user requested changes to plan \"{}\" (`{}`) and kept Plan Mode active. Revise the plan with `write_plan` and call `exit_plan_mode` when ready for review again.\n\nUser feedback:\n{}",
                            doc.summary.title,
                            doc.summary.file_name,
                            feedback.trim()
                        ));
                        Some(CommandDecision::RunContinuation)
                    }
                    Err(error) => {
                        let _ = ack_sender.send(Err(InteractiveError::from(error)));
                        Some(CommandDecision::Continue)
                    }
                }
            }
            InteractiveCommand::AbandonPlan {
                plan_id,
                ack_sender,
            } => {
                if mode != CommandHandlingMode::Waiting {
                    let _ = ack_sender.send(Err(InteractiveError::PlanControlRequiresIdle));
                    return Some(CommandDecision::Continue);
                }
                match self.runtime.abandon_plan(plan_id).await {
                    Ok((_doc, events)) => {
                        let _ = ack_sender.send(Ok(()));
                        if !self.send_runtime_events(events).await {
                            return None;
                        }
                    }
                    Err(error) => {
                        let _ = ack_sender.send(Err(InteractiveError::from(error)));
                    }
                }
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
        let queue_changed = self.queue.pause_on_interrupt();
        if self.bridge_pending {
            self.bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
        }
        if let Some(token) = self.phase_token.as_ref() {
            token.cancel();
        }
        if queue_changed && !self.send_queue_changed().await {
            return None;
        }
        if !self.send_state(InteractiveRunState::Interrupting).await {
            return None;
        }
        Some(CommandDecision::Continue)
    }
}
