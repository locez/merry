//! Coordinator-to-subagent messaging and wake/resume lifecycle.

use super::{
    SubagentManager, SubagentStatusLabel,
    protocol::{SendSubagentMessageOutcome, SendSubagentMessageOutput, SubagentMessageDelivery},
    spec::validate_task_max_model_turns,
};
use crate::RuntimeError;
use merry_core::SubagentId;

/// Maximum queued follow-up messages buffered per active child before reporting `Saturated`.
const MAX_PENDING_FOLLOW_UPS: usize = 16;

/// Delivery options for [`SubagentManager::send_message_with_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendSubagentMessageOptions {
    /// Whether to steer at the next safe checkpoint (front of queue) or append.
    pub delivery: SubagentMessageDelivery,
    /// Optional additional/override turn budget when waking a terminal child.
    pub extra_model_turns: Option<u32>,
}

impl Default for SendSubagentMessageOptions {
    fn default() -> Self {
        Self {
            delivery: SubagentMessageDelivery::Steer,
            extra_model_turns: None,
        }
    }
}

impl SubagentManager {
    /// Sends a follow-up message directly to a subagent session (used by TUI Takeover).
    ///
    /// If the subagent is currently running or queued, the message is queued and
    /// executes after the current turn settles. If the subagent has reached a
    /// terminal state with a retained runtime, a new turn is launched on that
    /// same child session.
    pub async fn send_message(
        &self,
        agent_id: &SubagentId,
        message: String,
    ) -> Result<(), RuntimeError> {
        let output = self
            .send_message_with_options(agent_id, message, SendSubagentMessageOptions::default())
            .await?;
        match output.outcome {
            SendSubagentMessageOutcome::Accepted | SendSubagentMessageOutcome::Woken => Ok(()),
            SendSubagentMessageOutcome::NotFoundOrNotOwned
            | SendSubagentMessageOutcome::Saturated
            | SendSubagentMessageOutcome::SessionClosing => {
                Err(RuntimeError::InvalidSubagentSelection {
                    operation: "send_subagent_message",
                })
            }
        }
    }

    /// Sends a follow-up instruction to a running child or wakes a terminal
    /// child with its retained transcript and `agent_id`, returning a typed,
    /// non-ambiguous delivery outcome.
    pub async fn send_message_with_options(
        &self,
        agent_id: &SubagentId,
        message: String,
        options: SendSubagentMessageOptions,
    ) -> Result<SendSubagentMessageOutput, RuntimeError> {
        let trimmed = message.trim();
        if trimmed.is_empty() {
            return Err(RuntimeError::InvalidSubagentSelection {
                operation: "send_subagent_message",
            });
        }
        if self.session_token.is_cancelled() {
            return Ok(SendSubagentMessageOutput {
                agent_id: agent_id.clone(),
                outcome: SendSubagentMessageOutcome::SessionClosing,
                status: None,
                wait_result: None,
                message: "parent session is closing; subagent cannot accept new messages"
                    .to_owned(),
            });
        }
        if let Some(extra_turns) = options.extra_model_turns {
            validate_task_max_model_turns(extra_turns, 1, self.max_model_turns()).map_err(
                |_| RuntimeError::InvalidSubagentSelection {
                    operation: "send_subagent_message",
                },
            )?;
        }

        let message = trimmed.to_owned();
        let mut launch_now = None;
        let outcome = {
            let mut state = self.state.lock().await;
            let Some(agent) = state.agents.get_mut(agent_id) else {
                return Ok(SendSubagentMessageOutput {
                    agent_id: agent_id.clone(),
                    outcome: SendSubagentMessageOutcome::NotFoundOrNotOwned,
                    status: None,
                    wait_result: None,
                    message: format!("unknown subagent id: {agent_id}"),
                });
            };
            if !agent.status.is_terminal() {
                if agent.pending_follow_ups.len() >= MAX_PENDING_FOLLOW_UPS {
                    return Ok(SendSubagentMessageOutput {
                        agent_id: agent_id.clone(),
                        outcome: SendSubagentMessageOutcome::Saturated,
                        status: Some(agent.status.clone()),
                        wait_result: None,
                        message: format!(
                            "subagent {agent_id} already has {MAX_PENDING_FOLLOW_UPS} queued messages"
                        ),
                    });
                }
                let position = match options.delivery {
                    SubagentMessageDelivery::Steer => {
                        agent.pending_follow_ups.push_front(message.clone());
                        0
                    }
                    SubagentMessageDelivery::Queue => {
                        let pos = agent.pending_follow_ups.len();
                        agent.pending_follow_ups.push_back(message.clone());
                        pos
                    }
                };
                if let Some(extra_turns) = options.extra_model_turns {
                    agent.resume_turn_override = Some(extra_turns);
                }
                let _ = agent
                    .event_tx
                    .send(merry_core::RuntimeEvent::QueuedInputAccepted {
                        inputs: vec![merry_core::QueuedInputView {
                            id: 0,
                            text: message,
                            position,
                        }],
                    });
                (
                    SendSubagentMessageOutcome::Accepted,
                    agent.status.clone(),
                    match options.delivery {
                        SubagentMessageDelivery::Steer => {
                            "steering message accepted for next safe checkpoint".to_owned()
                        }
                        SubagentMessageDelivery::Queue => {
                            "follow-up message queued after current turn".to_owned()
                        }
                    },
                )
            } else if let (Some(runtime), Some(generation_config)) =
                (agent.runtime.clone(), agent.generation_config.clone())
            {
                let token = self.session_token.child_token();
                agent.cancellation_token = token.clone();
                agent.status = SubagentStatusLabel::Running;
                agent.summary = "handling follow-up".to_owned();
                agent.diagnostics = None;
                agent.completion_notification_acknowledged = false;
                let mut wake_task = agent.task.clone();
                if let Some(extra_turns) = options.extra_model_turns {
                    wake_task = wake_task.with_max_model_turns_unchecked(extra_turns);
                }
                let _ = agent
                    .event_tx
                    .send(merry_core::RuntimeEvent::QueuedInputAccepted {
                        inputs: vec![merry_core::QueuedInputView {
                            id: 0,
                            text: message.clone(),
                            position: 0,
                        }],
                    });
                launch_now = Some((
                    agent.task_id.clone(),
                    wake_task,
                    token,
                    runtime,
                    generation_config,
                    agent.event_tx.clone(),
                    message,
                ));
                (
                    SendSubagentMessageOutcome::Woken,
                    SubagentStatusLabel::Running,
                    format!("woke subagent {agent_id} with its retained transcript and context"),
                )
            } else {
                return Ok(SendSubagentMessageOutput {
                    agent_id: agent_id.clone(),
                    outcome: SendSubagentMessageOutcome::NotFoundOrNotOwned,
                    status: Some(agent.status.clone()),
                    wait_result: None,
                    message: format!(
                        "subagent {agent_id} never started a child runtime and cannot be woken"
                    ),
                });
            }
        };

        if let Some((task_id, task, token, runtime, generation_config, event_tx, prompt)) =
            launch_now
        {
            self.resume_child_with_prompt(
                agent_id.clone(),
                task_id,
                task,
                token,
                runtime,
                generation_config,
                event_tx,
                prompt,
            )
            .await;
        }

        Ok(SendSubagentMessageOutput {
            agent_id: agent_id.clone(),
            outcome: outcome.0,
            status: Some(outcome.1),
            wait_result: None,
            message: outcome.2,
        })
    }
}
