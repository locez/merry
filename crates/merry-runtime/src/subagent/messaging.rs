//! Coordinator-to-subagent messaging and wake/resume lifecycle.

use super::{
    ChildRuntimeInput, ChildWorkspaceScope, ResumableChildSession, SubagentManager,
    SubagentStatusLabel,
    child::{
        ResumeChildLaunch, apply_built_task_overrides, build_child_offloaded,
        generation_config_for_child_task_with_parent,
    },
    events::child_session_id_for_agent,
    protocol::{SendSubagentMessageOutcome, SendSubagentMessageOutput, SubagentMessageDelivery},
    spec::validate_task_max_model_turns,
};
use crate::{LoadedSession, RuntimeError};
use merry_core::SubagentId;
use std::sync::Arc;

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
        let mut rehydrate_candidate = None;
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
                            text: message.clone(),
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
                launch_now = Some(ResumeChildLaunch {
                    agent_id: agent_id.clone(),
                    task_id: agent.task_id.clone(),
                    task: wake_task,
                    token,
                    runtime,
                    generation_config,
                    event_tx: agent.event_tx.clone(),
                    prompt: message.clone(),
                });
                (
                    SendSubagentMessageOutcome::Woken,
                    SubagentStatusLabel::Running,
                    format!("woke subagent {agent_id} with its retained transcript and context"),
                )
            } else if self.attached_child_session_store().is_some()
                || agent.worktree.as_ref().is_some_and(|w| !w.retained)
            {
                // Restored from parent session resume (or worktree auto-cleaned after
                // zero net changes): claim the Running state BEFORE dropping the lock
                // so concurrent wake calls queue cleanly instead of double-spawning.
                let previous_state = (
                    agent.status.clone(),
                    agent.summary.clone(),
                    agent.diagnostics.clone(),
                    agent.completion_notification_acknowledged,
                );
                agent.status = SubagentStatusLabel::Running;
                agent.summary = "waking persisted subagent".to_owned();
                agent.diagnostics = None;
                agent.completion_notification_acknowledged = false;

                let mut wake_task = agent.task.clone();
                if let Some(extra_turns) = options.extra_model_turns {
                    wake_task = wake_task.with_max_model_turns_unchecked(extra_turns);
                }
                let existing_worktree = agent.worktree.clone().filter(|worktree| worktree.retained);
                rehydrate_candidate = Some((
                    agent.task_id.clone(),
                    wake_task,
                    agent.task_anchor.clone(),
                    agent.event_tx.clone(),
                    existing_worktree,
                    previous_state,
                ));
                (
                    SendSubagentMessageOutcome::Woken,
                    SubagentStatusLabel::Running,
                    format!("woke persisted subagent {agent_id} from its saved child journal"),
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

        if let Some((
            task_id,
            wake_task,
            task_anchor,
            event_tx,
            existing_worktree,
            previous_state,
        )) = rehydrate_candidate
        {
            let store = self.attached_child_session_store();
            let child_session_id = child_session_id_for_agent(agent_id);
            let loaded_session = match store.as_ref() {
                Some(store) => LoadedSession::load(store, &child_session_id)
                    .await
                    .ok()
                    .map(ResumableChildSession::new),
                None => None,
            };
            let parent_primary_model = self.live_parent_model_snapshot();
            let parent_generation_config = self.live_parent_generation_snapshot();
            let default_generation_config =
                generation_config_for_child_task_with_parent(&wake_task, &parent_generation_config);
            let allowed_tools = if wake_task.allowed_tools_are_explicit() {
                wake_task.allowed_tools().to_vec()
            } else {
                self.parent_allowed_tools().unwrap_or_default()
            };
            let built = match build_child_offloaded(
                Arc::clone(&self.factory),
                ChildRuntimeInput {
                    parent_session_id: self.parent_session_id.clone(),
                    agent_id: Some(agent_id.clone()),
                    session_id: child_session_id,
                    session_store: store,
                    loaded_session,
                    existing_worktree,
                    task_anchor,
                    task: wake_task.clone(),
                    allowed_tools,
                    workspace_scope: ChildWorkspaceScope::from_task(&wake_task),
                    depth: self.depth.saturating_add(1),
                    generation_config: default_generation_config.clone(),
                    parent_primary_model,
                    parent_generation_config,
                    plan_subagent_control: None,
                    plan_subagent_scope: None,
                    plan_link: None,
                    plan_link_runtime: self.attached_plan_link_runtime(),
                    activity_hub: self.attached_activity_hub(),
                },
            )
            .await
            {
                Ok(built) => built,
                Err(error) => {
                    let mut state = self.state.lock().await;
                    if let Some(agent) = state.agents.get_mut(agent_id) {
                        agent.status = previous_state.0;
                        agent.summary = previous_state.1;
                        agent.diagnostics = previous_state.2;
                        agent.completion_notification_acknowledged = previous_state.3;
                    }
                    return Err(error);
                }
            };
            let wake_task = apply_built_task_overrides(
                wake_task,
                &built,
                self.default_timeout_secs(),
                self.max_total_tokens(),
            );
            let workspace_lifecycle = built.workspace_lifecycle;
            let worktree_metadata = workspace_lifecycle.as_ref().map(|hook| hook.metadata());
            let runtime = built.runtime;
            let generation_config = built.generation_config.unwrap_or(default_generation_config);
            let token = self.session_token.child_token();
            {
                let mut state = self.state.lock().await;
                if let Some(agent) = state.agents.get_mut(agent_id) {
                    agent.cancellation_token = token.clone();
                    agent.status = SubagentStatusLabel::Running;
                    agent.summary = "handling follow-up".to_owned();
                    agent.diagnostics = None;
                    agent.completion_notification_acknowledged = false;
                    agent.task = wake_task.clone();
                    agent.runtime = Some(runtime.clone());
                    agent.generation_config = Some(generation_config.clone());
                    agent.workspace_lifecycle = workspace_lifecycle;
                    if let Some(metadata) = worktree_metadata {
                        agent.worktree = Some(metadata);
                    }
                    agent.started_at = Some(tokio::time::Instant::now());
                    let _ = agent
                        .event_tx
                        .send(merry_core::RuntimeEvent::QueuedInputAccepted {
                            inputs: vec![merry_core::QueuedInputView {
                                id: 0,
                                text: message.clone(),
                                position: 0,
                            }],
                        });
                }
            }
            launch_now = Some(ResumeChildLaunch {
                agent_id: agent_id.clone(),
                task_id,
                task: wake_task,
                token,
                runtime,
                generation_config,
                event_tx,
                prompt: message,
            });
        }

        if let Some(launch) = launch_now {
            self.resume_child(launch).await;
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
