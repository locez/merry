//! Interactive producer lifecycle and input-boundary orchestration.

use super::super::{
    commands::{BoundaryAction, CommandDecision, CommandHandlingMode},
    queue::{AcceptedQueuedInput, step_input_from_accepted},
    types::InteractiveError,
};
use super::InteractiveProducer;
use crate::StepInput;
use merry_core::{InteractiveRunState, RuntimeEvent};

impl InteractiveProducer {
    pub(crate) async fn run(mut self) -> Result<(), InteractiveError> {
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
                subagent_event = self.subagent_event_receiver.recv() => {
                    if !self.forward_subagent_event(subagent_event).await {
                        return Ok(());
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
                CommandDecision::RunPending => {
                    if !self.run_pending_burst().await {
                        return self.stop_result();
                    }
                }
                CommandDecision::RunCompaction { focus, ack_sender } => {
                    if !self.run_manual_compaction(focus, ack_sender).await {
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

    pub(super) fn stop_result(&mut self) -> Result<(), InteractiveError> {
        self.terminal_error.take().map_or(Ok(()), Err)
    }

    pub(super) async fn run_pending_burst(&mut self) -> bool {
        let accepted = self.queue.accept_all_pending();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted).await
    }

    pub(super) async fn run_accepted_steps(
        &mut self,
        mut accepted: Vec<AcceptedQueuedInput>,
    ) -> bool {
        self.interrupted = false;
        loop {
            if let Some(event) = self.runtime.activate_pending_plan_mode_for_prompt().await
                && !self.send_runtime_events(vec![event]).await
            {
                return false;
            }
            let Some(input) = step_input_from_accepted(&accepted) else {
                return false;
            };
            let Some(step_events) = self.run_model_phase(input, Some(&accepted)).await else {
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
                } => {
                    accepted = next_accepted;
                }
                BoundaryAction::Continuation => {
                    let Some(boundary) = self.run_continuation_steps().await else {
                        return false;
                    };
                    match boundary {
                        BoundaryAction::UserInput {
                            accepted: next_accepted,
                        } => {
                            accepted = next_accepted;
                        }
                        BoundaryAction::Continuation => continue,
                        BoundaryAction::Wait => {
                            if let Some(event) =
                                self.runtime.complete_deferred_plan_mode_exit().await
                                && !self.send_runtime_events(vec![event]).await
                            {
                                return false;
                            }
                            return self.send_state(InteractiveRunState::WaitingForInput).await;
                        }
                    }
                }
                BoundaryAction::Wait => {
                    if let Some(event) = self.runtime.complete_deferred_plan_mode_exit().await
                        && !self.send_runtime_events(vec![event]).await
                    {
                        return false;
                    }
                    return self.send_state(InteractiveRunState::WaitingForInput).await;
                }
            }
        }
    }

    pub(super) async fn run_continuation_steps(&mut self) -> Option<BoundaryAction> {
        let mut first_step = true;
        loop {
            let notification_input = self.take_subagent_notification_input().await;
            let input = if let Some(input) = notification_input {
                input
            } else if let Some(note) = self.hook_continuation_note.take() {
                match StepInput::loop_control_text(&note) {
                    Ok(input) => input,
                    Err(error) => {
                        self.remember_terminal_error(InteractiveError::Runtime { source: error });
                        return None;
                    }
                }
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

    pub(super) async fn take_subagent_notification_input(&self) -> Option<StepInput> {
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

    pub(super) async fn run_coordinator_continuation(&mut self) -> bool {
        let Some(boundary) = self.run_continuation_steps().await else {
            return false;
        };
        match boundary {
            BoundaryAction::UserInput { accepted } => self.run_accepted_steps(accepted).await,
            BoundaryAction::Wait => {
                if let Some(event) = self.runtime.complete_deferred_plan_mode_exit().await
                    && !self.send_runtime_events(vec![event]).await
                {
                    return false;
                }
                self.send_state(InteractiveRunState::WaitingForInput).await
            }
            BoundaryAction::Continuation => true,
        }
    }

    pub(super) async fn run_manual_compaction(
        &mut self,
        focus: Option<String>,
        ack_sender: tokio::sync::oneshot::Sender<
            Result<Option<crate::CompactionOutcome>, InteractiveError>,
        >,
    ) -> bool {
        self.interrupted = false;
        let phase_token = self.loop_token.child_token();
        self.phase_token = Some(phase_token.clone());
        if !self.send_state(InteractiveRunState::RunningModel).await {
            self.phase_token = None;
            return false;
        }

        let (started_tx, mut started_rx) = tokio::sync::oneshot::channel();
        let runtime = self.runtime.clone();
        let permit = self.loop_permit.clone();
        let compact_fut = runtime.compact_context_with_active_permit(
            focus,
            phase_token.clone(),
            permit,
            started_tx,
        );
        tokio::pin!(compact_fut);

        let mut started_emitted = false;
        let mut commands_open = true;
        let result = loop {
            tokio::select! {
                result = &mut compact_fut => break result,
                started = &mut started_rx, if !started_emitted => {
                    started_emitted = true;
                    if started.is_ok() {
                        let event = self.runtime.record_manual_compaction_started().await;
                        if !self.send_runtime_events(vec![event]).await {
                            self.phase_token = None;
                            return false;
                        }
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
                        self.phase_token = None;
                        return false;
                    }
                }
                subagent_event = self.subagent_event_receiver.recv() => {
                    if !self.forward_subagent_event(subagent_event).await {
                        self.phase_token = None;
                        return false;
                    }
                }
            }
        };

        self.phase_token = None;
        if !started_emitted && started_rx.try_recv().is_ok() {
            let event = self.runtime.record_manual_compaction_started().await;
            if !self.send_runtime_events(vec![event]).await {
                return false;
            }
        }

        match result {
            Ok(Some(outcome)) => {
                let event = self
                    .runtime
                    .record_manual_compaction_completed(&outcome)
                    .await;
                if !self.send_runtime_events(vec![event]).await {
                    return false;
                }
                let _ = ack_sender.send(Ok(Some(outcome)));
            }
            Ok(None) => {
                let _ = ack_sender.send(Ok(None));
            }
            Err(error) => {
                if self.interrupted
                    || (phase_token.is_cancelled() && !self.loop_token.is_cancelled())
                {
                    let event = self
                        .runtime
                        .record_manual_compaction_cancelled("compaction cancelled by user")
                        .await;
                    if !self.send_runtime_events(vec![event]).await {
                        return false;
                    }
                } else if !self.loop_token.is_cancelled() {
                    let event = self
                        .runtime
                        .record_manual_compaction_failed(&error.to_string())
                        .await;
                    if !self.send_runtime_events(vec![event]).await {
                        return false;
                    }
                }
                let _ = ack_sender.send(Err(InteractiveError::from(error)));
            }
        }

        if !self.drain_ready_commands().await {
            return false;
        }
        self.refresh_subagent_continuation_request().await;
        match self.boundary_action(false) {
            BoundaryAction::UserInput { accepted } => self.run_accepted_steps(accepted).await,
            BoundaryAction::Continuation => self.run_coordinator_continuation().await,
            BoundaryAction::Wait => self.send_state(InteractiveRunState::WaitingForInput).await,
        }
    }

    pub(super) async fn drain_ready_commands(&mut self) -> bool {
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

    pub(super) async fn refresh_subagent_continuation_request(&mut self) {
        if self.runtime.has_subagent_completion_notifications().await {
            self.subagent_continuation_requested = true;
        }
    }

    pub(super) fn boundary_action(&mut self, continuation_required: bool) -> BoundaryAction {
        if self.interrupted {
            return BoundaryAction::Wait;
        }

        if self.subagent_continuation_requested {
            self.subagent_continuation_requested = false;
            return BoundaryAction::Continuation;
        }

        if self.queue.can_auto_dispatch() {
            self.interrupted = false;
            return BoundaryAction::UserInput {
                accepted: self.queue.accept_all_pending(),
            };
        }

        if continuation_required || self.coordinator_continuation_note.is_some() {
            return BoundaryAction::Continuation;
        }

        BoundaryAction::Wait
    }
}
