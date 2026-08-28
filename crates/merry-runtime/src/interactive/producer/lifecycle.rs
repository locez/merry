//! Interactive producer lifecycle and input-boundary orchestration.

use super::super::{
    commands::{BoundaryAction, CommandDecision, CommandHandlingMode},
    queue::{AcceptedQueuedInput, step_input_from_accepted},
    types::InteractiveError,
};
use super::InteractiveProducer;
use crate::StepInput;
use merry_core::{InteractiveRunState, QueuedInputLane, RuntimeEvent};

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

    pub(super) fn stop_result(&mut self) -> Result<(), InteractiveError> {
        self.terminal_error.take().map_or(Ok(()), Err)
    }

    pub(super) async fn run_next_burst(&mut self) -> bool {
        let accepted = self.queue.accept_next_burst();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted, QueuedInputLane::Next)
            .await
    }

    pub(super) async fn run_suspended_burst(&mut self) -> bool {
        self.suspended_resume_requested = false;
        let accepted = self.queue.accept_suspended_burst();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted, QueuedInputLane::Suspended)
            .await
    }

    pub(super) async fn run_one_backlog(&mut self) -> bool {
        let accepted = self.queue.accept_one_backlog();
        if accepted.is_empty() {
            return self.send_state(InteractiveRunState::WaitingForInput).await;
        }
        self.run_accepted_steps(accepted, QueuedInputLane::Backlog)
            .await
    }

    pub(super) async fn run_accepted_steps(
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

    pub(super) async fn run_continuation_steps(&mut self) -> Option<BoundaryAction> {
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
}
