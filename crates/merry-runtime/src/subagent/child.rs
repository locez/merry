use crate::{
    AgentLoopConfig, AgentRunMessage, Runtime, StepContext, StepInput, TaskAnchor,
    subagent::{
        ChildRuntimeInput, ChildWorkspaceScope, PlanLinkRuntime, PlanSubagentScope,
        SubagentActivityHub, SubagentManager, SubagentStatusLabel, SubagentTaskSpec,
        activity::SubagentActivityReducer,
        scheduler::{
            ChildScheduler, ReservedChildStart, ReservedChildStartError, ReservedChildStartOutcome,
            reserve_queued_starts_locked, start_reserved_children_iteratively,
        },
    },
};
use merry_core::{ErrorInfo, PlanLinkSnapshot, PlanLinkStatus, SubagentId, SubagentTaskId};
use merry_llm::GenerationConfig;
pub(super) use projection::{ChildLoopProjection, apply_loop_result};
use settle::publish_terminal_activity;
pub(super) use settle::{
    finish_child_with_status, publish_terminal_activity_snapshot, update_plan_link_with_scheduler,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

mod projection;
mod settle;

pub(super) struct ChildLoopLaunch {
    pub(super) agent_id: SubagentId,
    pub(super) task_id: SubagentTaskId,
    pub(super) task: SubagentTaskSpec,
    pub(super) prompt: String,
    pub(super) token: CancellationToken,
    pub(super) runtime: Runtime,
    pub(super) generation_config: GenerationConfig,
    pub(super) activity_hub: Option<Arc<SubagentActivityHub>>,
    pub(super) event_tx: broadcast::Sender<merry_core::RuntimeEvent>,
}

pub(super) struct ResumeChildLaunch {
    pub(super) agent_id: SubagentId,
    pub(super) task_id: SubagentTaskId,
    pub(super) task: SubagentTaskSpec,
    pub(super) token: CancellationToken,
    pub(super) runtime: Runtime,
    pub(super) generation_config: GenerationConfig,
    pub(super) event_tx: broadcast::Sender<merry_core::RuntimeEvent>,
    pub(super) prompt: String,
}

/// Runs synchronous [`super::ChildRuntimeFactory::build_child_with_overrides`] on
/// Tokio's blocking pool so worktree Git commands and handoff file reads never
/// stall async worker threads or the single-threaded CLI/TUI executor.
pub(super) async fn build_child_offloaded(
    factory: Arc<dyn super::ChildRuntimeFactory>,
    input: ChildRuntimeInput,
) -> Result<super::BuiltChildRuntime, crate::RuntimeError> {
    tokio::task::spawn_blocking(move || factory.build_child_with_overrides(input))
        .await
        .map_err(|error| crate::RuntimeError::ChildRuntimeBuild {
            message: format!("child runtime factory task panicked: {error}"),
        })?
}

enum PlanScopeLookupError {
    Cancelled,
    Lookup(String),
}

async fn lookup_plan_subagent_scope(
    plan_link: Option<&PlanLinkSnapshot>,
    plan_link_runtime: Option<&Arc<dyn PlanLinkRuntime>>,
    token: &CancellationToken,
) -> Result<Option<PlanSubagentScope>, PlanScopeLookupError> {
    if token.is_cancelled() {
        return Err(PlanScopeLookupError::Cancelled);
    }
    let Some((link, runtime)) = plan_link.zip(plan_link_runtime) else {
        return Ok(None);
    };
    let lookup = runtime.scope_for_link(link);
    let result = tokio::select! {
        _ = token.cancelled() => return Err(PlanScopeLookupError::Cancelled),
        result = lookup => result,
    };
    if token.is_cancelled() {
        return Err(PlanScopeLookupError::Cancelled);
    }
    result.map_err(PlanScopeLookupError::Lookup)
}

impl SubagentManager {
    pub(super) async fn start_child(
        &self,
        agent_id: SubagentId,
        task_id: SubagentTaskId,
        task: SubagentTaskSpec,
        task_anchor: TaskAnchor,
        token: CancellationToken,
        plan_link: Option<PlanLinkSnapshot>,
    ) {
        let child_session_id = super::events::child_session_id_for_agent(&agent_id);
        let parent_primary_model = self.live_parent_model_snapshot();
        let parent_generation_config = self.live_parent_generation_snapshot();
        let default_generation_config =
            generation_config_for_child_task_with_parent(&task, &parent_generation_config);
        let plan_link_runtime = self.attached_plan_link_runtime();
        let activity_hub = self.attached_activity_hub();
        let child_session_store = self.attached_child_session_store();
        let plan_subagent_scope = match lookup_plan_subagent_scope(
            plan_link.as_ref(),
            plan_link_runtime.as_ref(),
            &token,
        )
        .await
        {
            Ok(scope) => scope,
            Err(PlanScopeLookupError::Cancelled) => {
                self.mark_cancelled_and_schedule(&agent_id).await;
                return;
            }
            Err(PlanScopeLookupError::Lookup(error)) => {
                self.mark_failed_and_schedule(
                    &agent_id,
                    "child Plan scope lookup failed",
                    error_info("subagent_plan_scope_error", error),
                )
                .await;
                return;
            }
        };
        if token.is_cancelled() {
            self.mark_cancelled_and_schedule(&agent_id).await;
            return;
        };
        let built = match build_child_offloaded(
            Arc::clone(&self.factory),
            ChildRuntimeInput {
                parent_session_id: self.parent_session_id.clone(),
                agent_id: Some(agent_id.clone()),
                session_id: child_session_id,
                session_store: child_session_store,
                loaded_session: None,
                existing_worktree: None,
                task_anchor,
                task: task.clone(),
                allowed_tools: task.allowed_tools().to_vec(),
                workspace_scope: ChildWorkspaceScope::from_task(&task),
                depth: self.depth.saturating_add(1),
                generation_config: default_generation_config.clone(),
                parent_primary_model,
                parent_generation_config,
                plan_subagent_control: None,
                plan_subagent_scope,
                plan_link: plan_link.clone(),
                plan_link_runtime,
                activity_hub: activity_hub.clone(),
            },
        )
        .await
        {
            Ok(built) => built,
            Err(error) => {
                self.mark_failed_and_schedule(
                    &agent_id,
                    "child runtime factory failed",
                    error_info("subagent_factory_error", error.to_string()),
                )
                .await;
                return;
            }
        };
        if token.is_cancelled() {
            super::events::finalize_child_workspace(built.workspace_lifecycle, true).await;
            self.mark_cancelled_and_schedule(&agent_id).await;
            return;
        }
        let task = apply_built_task_overrides(
            task,
            &built,
            self.default_timeout_secs(),
            self.max_total_tokens(),
        );
        let workspace_lifecycle = built.workspace_lifecycle;
        let worktree_metadata = workspace_lifecycle.as_ref().map(|hook| hook.metadata());
        let runtime = built.runtime;
        let generation_config = built.generation_config.unwrap_or(default_generation_config);
        let event_tx = {
            let mut state = self.state.lock().await;
            let Some(agent) = state.agents.get_mut(&agent_id) else {
                return;
            };
            agent.task = task.clone();
            agent.runtime = Some(runtime.clone());
            agent.generation_config = Some(generation_config.clone());
            agent.workspace_lifecycle = workspace_lifecycle;
            if let Some(metadata) = worktree_metadata {
                agent.worktree = Some(metadata);
            }
            agent.event_tx.clone()
        };

        let prompt = task.task().to_owned();
        let handle_id = agent_id.clone();
        let handle = spawn_child_loop(
            self.child_scheduler(),
            ChildLoopLaunch {
                agent_id,
                task_id,
                task,
                prompt,
                token,
                runtime,
                generation_config,
                activity_hub,
                event_tx,
            },
        );
        self.state.lock().await.handles.insert(handle_id, handle);
    }

    pub(super) async fn resume_child(&self, launch: ResumeChildLaunch) {
        let activity_hub = self.attached_activity_hub();
        let handle_id = launch.agent_id.clone();
        let handle = spawn_child_loop(
            self.child_scheduler(),
            ChildLoopLaunch {
                agent_id: launch.agent_id,
                task_id: launch.task_id,
                task: launch.task,
                prompt: launch.prompt,
                token: launch.token,
                runtime: launch.runtime,
                generation_config: launch.generation_config,
                activity_hub,
                event_tx: launch.event_tx,
            },
        );
        self.state.lock().await.handles.insert(handle_id, handle);
    }

    pub(super) async fn mark_failed_and_schedule(
        &self,
        agent_id: &SubagentId,
        summary: &str,
        diagnostics: ErrorInfo,
    ) {
        let mut state = self.state.lock().await;
        let mut link = None;
        let mut terminal_activity = None;
        let mut terminal_notification = None;
        let mut terminal_task = None;
        if let Some(agent) = state.agents.get_mut(agent_id) {
            if agent.status.is_terminal() {
                return;
            }
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = summary.to_owned();
            agent.diagnostics = Some(diagnostics);
            link = agent.plan_link.clone();
            terminal_activity = terminal_activity_for_agent(agent);
            terminal_task = Some(agent.task.task().to_owned());
            terminal_notification = Some(agent.status_view());
        }
        let to_start = self.reserve_queued_starts_locked(&mut state);
        drop(state);
        self.update_plan_links(link.into_iter().collect(), PlanLinkStatus::Failed)
            .await;
        if let Some((task_id, phase, summary)) = terminal_activity {
            publish_terminal_activity_snapshot(
                self.attached_activity_hub(),
                agent_id.clone(),
                task_id,
                phase,
                &summary,
            );
        }
        if let Some(status) = terminal_notification {
            let sink = self.attached_event_sink();
            super::events::emit_terminal_event_and_persist(
                sink.as_ref(),
                terminal_task.unwrap_or_default(),
                &status,
            )
            .await;
            self.enqueue_completion_notification(status).await;
        }
        self.notify.notify_waiters();
        let _ = self.start_reserved_children(to_start).await;
    }

    pub(super) async fn mark_cancelled_and_schedule(&self, agent_id: &SubagentId) {
        let mut state = self.state.lock().await;
        let mut link = None;
        let mut terminal_activity = None;
        let mut terminal_notification = None;
        let mut terminal_task = None;
        if let Some(agent) = state.agents.get_mut(agent_id)
            && !agent.status.is_terminal()
        {
            agent.status = SubagentStatusLabel::Cancelled;
            agent.summary = "child cancelled before runtime start".to_owned();
            agent.diagnostics = Some(error_info(
                "subagent_cancelled",
                "child cancellation requested before runtime start",
            ));
            link = agent.plan_link.clone();
            terminal_activity = terminal_activity_for_agent(agent);
            terminal_task = Some(agent.task.task().to_owned());
            terminal_notification = Some(agent.status_view());
        }
        let to_start = self.reserve_queued_starts_locked(&mut state);
        drop(state);
        self.update_plan_links(link.into_iter().collect(), PlanLinkStatus::Cancelled)
            .await;
        if let Some((task_id, phase, summary)) = terminal_activity {
            publish_terminal_activity_snapshot(
                self.attached_activity_hub(),
                agent_id.clone(),
                task_id,
                phase,
                &summary,
            );
        }
        if let Some(status) = terminal_notification {
            let sink = self.attached_event_sink();
            super::events::emit_terminal_event_and_persist(
                sink.as_ref(),
                terminal_task.unwrap_or_default(),
                &status,
            )
            .await;
            self.enqueue_completion_notification(status).await;
        }
        self.notify.notify_waiters();
        let _ = self.start_reserved_children(to_start).await;
    }
}

pub(super) async fn spawn_reserved_child(
    scheduler: ChildScheduler,
    start: &ReservedChildStart,
) -> Result<ReservedChildStartOutcome, ReservedChildStartError> {
    let plan_link_runtime = scheduler
        .plan_link_runtime
        .lock()
        .expect("subagent plan link runtime mutex is not poisoned")
        .clone();
    let plan_subagent_scope = match lookup_plan_subagent_scope(
        start.plan_link.as_ref(),
        plan_link_runtime.as_ref(),
        &start.cancellation_token,
    )
    .await
    {
        Ok(scope) => scope,
        Err(PlanScopeLookupError::Cancelled) => {
            return Ok(ReservedChildStartOutcome::Cancelled);
        }
        Err(PlanScopeLookupError::Lookup(error)) => {
            return Err(ReservedChildStartError::PlanScope(error));
        }
    };
    if start.cancellation_token.is_cancelled() {
        return Ok(ReservedChildStartOutcome::Cancelled);
    }
    let child_session_id = super::events::child_session_id_for_agent(&start.agent_id);
    let parent_primary_model = scheduler.live_parent_model_snapshot();
    let parent_generation_config = scheduler.live_parent_generation_snapshot();
    let default_generation_config =
        generation_config_for_child_task_with_parent(&start.task, &parent_generation_config);
    let built = build_child_offloaded(
        Arc::clone(&scheduler.factory),
        ChildRuntimeInput {
            parent_session_id: scheduler.parent_session_id.clone(),
            agent_id: Some(start.agent_id.clone()),
            session_id: child_session_id,
            session_store: scheduler.attached_child_session_store(),
            loaded_session: None,
            existing_worktree: None,
            task_anchor: start.task_anchor.clone(),
            task: start.task.clone(),
            allowed_tools: start.task.allowed_tools().to_vec(),
            workspace_scope: ChildWorkspaceScope::from_task(&start.task),
            depth: scheduler.depth.saturating_add(1),
            generation_config: default_generation_config.clone(),
            parent_primary_model,
            parent_generation_config,
            plan_subagent_control: None,
            plan_subagent_scope,
            plan_link: start.plan_link.clone(),
            plan_link_runtime,
            activity_hub: scheduler.attached_activity_hub(),
        },
    )
    .await
    .map_err(ReservedChildStartError::Factory)?;
    if start.cancellation_token.is_cancelled() {
        super::events::finalize_child_workspace(built.workspace_lifecycle, true).await;
        return Ok(ReservedChildStartOutcome::Cancelled);
    }

    let task = apply_built_task_overrides(
        start.task.clone(),
        &built,
        scheduler.default_timeout_secs(),
        scheduler.max_total_tokens(),
    );
    let workspace_lifecycle = built.workspace_lifecycle;
    let worktree_metadata = workspace_lifecycle.as_ref().map(|hook| hook.metadata());
    let runtime = built.runtime;
    let generation_config = built.generation_config.unwrap_or(default_generation_config);
    let event_tx = {
        let mut state = scheduler.state.lock().await;
        let Some(agent) = state.agents.get_mut(&start.agent_id) else {
            return Ok(ReservedChildStartOutcome::Cancelled);
        };
        agent.task = task.clone();
        agent.runtime = Some(runtime.clone());
        agent.generation_config = Some(generation_config.clone());
        agent.workspace_lifecycle = workspace_lifecycle;
        if let Some(metadata) = worktree_metadata {
            agent.worktree = Some(metadata);
        }
        if agent.started_at.is_none() {
            agent.started_at = Some(tokio::time::Instant::now());
        }
        agent.event_tx.clone()
    };

    let activity_hub = scheduler.attached_activity_hub();
    let handle_id = start.agent_id.clone();
    let handles_state = Arc::clone(&scheduler.state);
    let handle = spawn_child_loop(
        scheduler,
        ChildLoopLaunch {
            agent_id: start.agent_id.clone(),
            task_id: start.task_id.clone(),
            prompt: task.task().to_owned(),
            task,
            token: start.cancellation_token.clone(),
            runtime,
            generation_config,
            activity_hub,
            event_tx,
        },
    );
    handles_state.lock().await.handles.insert(handle_id, handle);

    Ok(ReservedChildStartOutcome::Started)
}

fn spawn_child_loop(
    scheduler: ChildScheduler,
    launch: ChildLoopLaunch,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut activity_reducer =
            SubagentActivityReducer::new(launch.agent_id.clone(), launch.task_id.clone());
        if let Some(hub) = launch.activity_hub.as_deref() {
            hub.publish(activity_reducer.starting(crate::plan::unix_time_ms()));
        }
        let sink = scheduler.attached_event_sink();
        if let Some(sink) = sink.as_ref() {
            sink.emit_journal_payload(merry_core::RuntimeJournalPayload::SubagentStarted {
                agent_id: launch.agent_id.clone(),
                task_id: launch.task_id.clone(),
            })
            .await;
        }
        let mut reported_usage = launch.runtime.usage().await.map(|u| u.total);

        let input = match StepInput::user_text(&launch.prompt) {
            Ok(input) => input,
            Err(error) => {
                let cancelled = launch.token.is_cancelled();
                finish_child_with_status(
                    scheduler,
                    &launch,
                    &mut activity_reducer,
                    if cancelled {
                        SubagentStatusLabel::Cancelled
                    } else {
                        SubagentStatusLabel::Failed
                    },
                    if cancelled {
                        "child cancelled before runtime stream"
                    } else {
                        "child task input was rejected"
                    },
                    if cancelled {
                        error_info(
                            "subagent_cancelled",
                            "child cancellation requested before runtime stream",
                        )
                    } else {
                        error_info("subagent_input_error", error.to_string())
                    },
                )
                .await;
                return;
            }
        };
        let config = match AgentLoopConfig::new(launch.task.max_model_turns() as usize) {
            Ok(config) => config,
            Err(error) => {
                let cancelled = launch.token.is_cancelled();
                finish_child_with_status(
                    scheduler,
                    &launch,
                    &mut activity_reducer,
                    if cancelled {
                        SubagentStatusLabel::Cancelled
                    } else {
                        SubagentStatusLabel::Failed
                    },
                    if cancelled {
                        "child cancelled before runtime stream"
                    } else {
                        "child loop configuration was rejected"
                    },
                    if cancelled {
                        error_info(
                            "subagent_cancelled",
                            "child cancellation requested before runtime stream",
                        )
                    } else {
                        error_info("subagent_config_error", error.to_string())
                    },
                )
                .await;
                return;
            }
        };

        // If waking/resuming after a turn limit, timeout, or cancellation left
        // unresolved tool calls in the child session, settle them cleanly so
        // the next provider turn never sends an orphaned tool_use block.
        let dangling = launch.runtime.pending_tool_calls().await;
        for call in dangling {
            let output = crate::ToolOutput::text(
                "previous subagent turn ended before this tool call executed; continue with the coordinator's follow-up instruction",
            );
            let result = merry_core::ToolCallResult::failed(
                call.id().clone(),
                error_info(
                    "subagent_turn_interrupted",
                    "previous subagent turn ended before this tool call executed",
                ),
            );
            let _ = launch.runtime.submit_tool_result(result, output).await;
        }

        let loop_token = launch.token.clone();
        let mut stream = match launch.runtime.run_agent_loop_stream(
            input,
            StepContext::new(loop_token.clone())
                .with_generation_config(launch.generation_config.clone()),
            config,
        ) {
            Ok(stream) => stream,
            Err(error) => {
                let cancelled = loop_token.is_cancelled();
                finish_child_with_status(
                    scheduler,
                    &launch,
                    &mut activity_reducer,
                    if cancelled {
                        SubagentStatusLabel::Cancelled
                    } else {
                        SubagentStatusLabel::Failed
                    },
                    if cancelled {
                        "child cancelled before runtime stream"
                    } else {
                        "child runtime stream failed to start"
                    },
                    if cancelled {
                        error_info(
                            "subagent_cancelled",
                            "child cancellation requested before runtime stream",
                        )
                    } else {
                        error_info("subagent_stream_start_error", error.to_string())
                    },
                )
                .await;
                return;
            }
        };

        let timeout_deadline = launch
            .task
            .timeout_secs()
            .map(|secs| tokio::time::Instant::now() + std::time::Duration::from_secs(secs));
        let mut timed_out = false;
        let mut token_budget_exceeded = false;
        let mut bridge_request = None;
        loop {
            let next_msg = match timeout_deadline {
                Some(deadline) => {
                    match tokio::time::timeout_at(deadline, stream.next_message()).await {
                        Ok(msg) => msg,
                        Err(_) => {
                            timed_out = true;
                            loop_token.cancel();
                            break;
                        }
                    }
                }
                None => stream.next_message().await,
            };
            let Ok(Some(message)) = next_msg else {
                break;
            };
            match message {
                AgentRunMessage::Event(event) => {
                    if let merry_core::RuntimeEvent::UsageUpdated { usage, .. } = &event {
                        super::events::fold_incremental_child_usage(
                            sink.as_ref(),
                            &mut reported_usage,
                            usage.total,
                        )
                        .await;
                        if let Some(limit) = launch.task.max_total_tokens()
                            && usage
                                .total
                                .input_tokens
                                .saturating_add(usage.total.output_tokens)
                                > limit
                        {
                            token_budget_exceeded = true;
                            loop_token.cancel();
                        }
                    }
                    let delta_bytes = observed_event_output_bytes(&event);
                    if delta_bytes > 0 {
                        let mut guard = scheduler.state.lock().await;
                        if let Some(agent) = guard.agents.get_mut(&launch.agent_id) {
                            agent.raw_output_bytes =
                                agent.raw_output_bytes.saturating_add(delta_bytes);
                        }
                    }
                    let _ = launch.event_tx.send(event.clone());
                    if let Some(hub) = launch.activity_hub.as_deref()
                        && let Some(snapshot) =
                            activity_reducer.reduce(&event, crate::plan::unix_time_ms())
                    {
                        hub.publish(snapshot);
                    }
                    if token_budget_exceeded {
                        break;
                    }
                }
                AgentRunMessage::ToolInvocations { batch } => {
                    bridge_request = batch.calls().first().cloned();
                    break;
                }
            }
        }
        let (loop_result, loop_error) = if timed_out || token_budget_exceeded {
            drop(stream);
            (None, None)
        } else if let Some(call) = bridge_request.as_ref() {
            tracing::warn!(
                subagent_id = %launch.agent_id,
                task_id = %launch.task_id,
                tool_name = %call.name(),
                "child runtime has no bridge host for requested tool"
            );
            drop(stream);
            (None, None)
        } else {
            match stream.result().await {
                Ok(result) => (Some(result), None),
                Err(error) => (None, Some(error)),
            }
        };
        let child_projection = match loop_result.as_ref() {
            Some(result) => ChildLoopProjection::from_result(&launch.runtime, result).await,
            None => ChildLoopProjection::default(),
        };

        let mut state_guard = scheduler.state.lock().await;
        if state_guard
            .agents
            .get(&launch.agent_id)
            .is_some_and(|agent| agent.status == SubagentStatusLabel::Cancelled)
        {
            state_guard.handles.remove(&launch.agent_id);
            let to_start =
                reserve_queued_starts_locked(&mut state_guard, scheduler.effective_max_threads());
            drop(state_guard);
            super::events::finalize_agent_workspace_into_state(
                &scheduler.state,
                &launch.agent_id,
                true,
            )
            .await;
            scheduler.notify.notify_waiters();
            start_reserved_children_iteratively(scheduler, to_start).await;
            return;
        }
        let next_follow_up = state_guard
            .agents
            .get_mut(&launch.agent_id)
            .and_then(|agent| {
                if loop_token.is_cancelled() || agent.status == SubagentStatusLabel::Cancelled {
                    None
                } else {
                    let prompt = agent.pending_follow_ups.pop_front()?;
                    let turn_override = agent.resume_turn_override.take();
                    Some((prompt, turn_override))
                }
            });
        if let Some((next_prompt, turn_override)) = next_follow_up {
            if let Some(agent) = state_guard.agents.get_mut(&launch.agent_id) {
                if let Some(result) = loop_result.as_ref() {
                    apply_loop_result(agent, result, child_projection);
                }
                agent.status = SubagentStatusLabel::Running;
                agent.summary = "handling follow-up".to_owned();
                agent.diagnostics = None;
            }
            let handle_id = launch.agent_id.clone();
            let mut next_task = launch.task;
            if let Some(extra_turns) = turn_override {
                next_task = next_task.with_max_model_turns_unchecked(extra_turns);
            }
            let handle = spawn_child_loop(
                scheduler.clone(),
                ChildLoopLaunch {
                    task: next_task,
                    prompt: next_prompt,
                    ..launch
                },
            );
            state_guard.handles.insert(handle_id, handle);
            drop(state_guard);
            return;
        }
        // Finalize any isolated workspace BEFORE flipping `agent.status` to a
        // terminal label so `wait()` and `snapshot()` never observe completion
        // before worktree diff collection and fail-toward-keep cleanup settle.
        let workspace_lifecycle = state_guard
            .agents
            .get(&launch.agent_id)
            .and_then(|agent| agent.workspace_lifecycle.clone());
        drop(state_guard);
        let workspace_finalization =
            super::events::finalize_child_workspace(workspace_lifecycle, true).await;
        let mut state_guard = scheduler.state.lock().await;
        if let Some(agent) = state_guard.agents.get_mut(&launch.agent_id) {
            if let Some(started) = agent.started_at {
                agent.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            }
            if token_budget_exceeded {
                let limit = launch.task.max_total_tokens().unwrap_or(0);
                agent.usage = reported_usage.or(agent.usage);
                agent.status = SubagentStatusLabel::Blocked;
                agent.summary = format!(
                    "child exceeded max_total_tokens budget ({limit}); inspect partial progress before continuing"
                );
                agent.result = None;
                agent.diagnostics = Some(error_info(
                    "subagent_token_budget_exceeded",
                    format!("child exceeded configured max_total_tokens budget ({limit})"),
                ));
            } else if timed_out {
                let secs = launch.task.timeout_secs().unwrap_or(0);
                agent.status = SubagentStatusLabel::Blocked;
                agent.summary = format!(
                    "child timed out after {secs}s; resume with send_subagent_message if more time is needed"
                );
                agent.result = None;
                agent.diagnostics = Some(error_info(
                    "subagent_timeout",
                    format!(
                        "child exceeded wall-clock timeout of {secs}s. Call send_subagent_message(agent_id=\"{}\") to continue with its retained transcript.",
                        launch.agent_id.as_str()
                    ),
                ));
            } else if let Some(error) = loop_error {
                agent.status = SubagentStatusLabel::Failed;
                agent.summary = "child runtime stream failed".to_owned();
                agent.result = None;
                agent.diagnostics = Some(error_info("subagent_stream_error", error));
            } else {
                match loop_result {
                    Some(result) => {
                        apply_loop_result(agent, &result, child_projection);
                        if let (Some(limit), Some(usage)) =
                            (launch.task.max_total_tokens(), agent.usage)
                            && usage.input_tokens.saturating_add(usage.output_tokens) > limit
                            && agent.status != SubagentStatusLabel::Completed
                        {
                            agent.status = SubagentStatusLabel::Blocked;
                            agent.summary = format!(
                                "child exceeded max_total_tokens budget ({limit}); inspect partial progress before continuing"
                            );
                            agent.diagnostics = Some(error_info(
                                "subagent_token_budget_exceeded",
                                format!(
                                    "child exceeded configured max_total_tokens budget ({limit})"
                                ),
                            ));
                        }
                    }
                    None if loop_token.is_cancelled() => {
                        agent.status = SubagentStatusLabel::Cancelled;
                        agent.summary = "child cancelled before stream result".to_owned();
                        agent.result = None;
                        agent.diagnostics = Some(error_info(
                            "subagent_cancelled",
                            "child stream ended after cancellation without a result",
                        ));
                    }
                    None if bridge_request.is_some() => {
                        let call = bridge_request
                            .as_ref()
                            .expect("bridge request guard ensures a call is present");
                        agent.status = SubagentStatusLabel::Failed;
                        agent.summary =
                            format!("child bridge tool {} has no host", call.name().as_str());
                        agent.result = None;
                        agent.diagnostics = Some(error_info(
                            "subagent_bridge_unavailable",
                            format!(
                                "child bridge tool {} requested without a bridge host",
                                call.name().as_str()
                            ),
                        ));
                    }
                    None => {
                        agent.status = SubagentStatusLabel::Failed;
                        agent.summary = "child runtime stream ended without result".to_owned();
                        agent.result = None;
                        agent.diagnostics = Some(error_info(
                            "subagent_stream_result_missing",
                            "child runtime stream ended without a durable result",
                        ));
                    }
                }
            }
            super::events::apply_workspace_finalization(agent, workspace_finalization);
        }
        state_guard.handles.remove(&launch.agent_id);
        let (terminal_link, terminal_status, terminal_activity, terminal_notification, final_usage) =
            state_guard
                .agents
                .get(&launch.agent_id)
                .map(|agent| {
                    (
                        agent.plan_link.clone(),
                        plan_link_status_for_agent(agent),
                        terminal_activity_for_agent(agent),
                        agent.status.is_terminal().then(|| agent.status_view()),
                        agent.usage,
                    )
                })
                .unwrap_or((None, PlanLinkStatus::Failed, None, None, None));
        let to_start =
            reserve_queued_starts_locked(&mut state_guard, scheduler.effective_max_threads());
        drop(state_guard);
        update_plan_link_with_scheduler(&scheduler, terminal_link, terminal_status).await;
        if let Some((_task_id, phase, summary)) = terminal_activity {
            publish_terminal_activity(
                launch.activity_hub.as_deref(),
                &mut activity_reducer,
                phase,
                &summary,
            );
        }
        if let Some(status) = terminal_notification {
            super::events::teardown_child_runtime(
                sink.as_ref(),
                &launch.runtime,
                reported_usage,
                final_usage,
                launch.task.task().to_owned(),
                &status,
            )
            .await;
            scheduler.enqueue_completion_notification(status).await;
        }
        scheduler.notify.notify_waiters();
        start_reserved_children_iteratively(scheduler, to_start).await;
    })
}

pub(super) use super::manager_helpers::{
    apply_built_task_overrides, error_info, generation_config_for_child_task_with_parent,
    observed_event_output_bytes, plan_link_status_for_agent, terminal_activity_for_agent,
};
