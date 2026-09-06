use super::APPLY_PATCH_TOOL_NAME;
use super::activity::SubagentActivityReducer;
use super::manager::{ManagedSubagent, SubagentManagerState};
use super::scheduler::{
    ChildScheduler, ReservedChildStart, ReservedChildStartError, ReservedChildStartOutcome,
    reserve_queued_starts_locked, start_reserved_children_iteratively,
};
use super::spec::validate_scope_path;
use super::{
    ChildRuntimeInput, ChildWorkspaceScope, PlanLinkRuntime, PlanSubagentScope,
    SubagentActivityHub, SubagentManager, SubagentResultView, SubagentStatusLabel,
    SubagentTaskSpec,
};
use crate::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopResult, AgentLoopStatus, AgentRunMessage,
    ArtifactContent, Runtime, StepContext, StepInput, TaskAnchor,
};
use merry_core::{
    ErrorInfo, PlanLinkSnapshot, PlanLinkStatus, RuntimeJournalEvent, RuntimeJournalPayload,
    SubagentActivityPhase, SubagentId, SubagentTaskId, ToolCallResultStatus,
};
use merry_llm::GenerationConfig;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub(super) struct ChildLoopLaunch {
    pub(super) agent_id: SubagentId,
    pub(super) task_id: SubagentTaskId,
    pub(super) task: SubagentTaskSpec,
    pub(super) token: CancellationToken,
    pub(super) runtime: Runtime,
    pub(super) generation_config: GenerationConfig,
    pub(super) activity_hub: Option<Arc<SubagentActivityHub>>,
}

struct ChildErrorTransition {
    claimed: bool,
    to_start: Vec<ReservedChildStart>,
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
        let child_session_id = child_session_id();
        let generation_config = generation_config_for_child_task(&task);
        let plan_link_runtime = self.attached_plan_link_runtime();
        let activity_hub = self.attached_activity_hub();
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
        let runtime = match self.factory.build_child(ChildRuntimeInput {
            session_id: child_session_id,
            task_anchor,
            task: task.clone(),
            allowed_tools: task.allowed_tools().to_vec(),
            workspace_scope: ChildWorkspaceScope::from_task(&task),
            depth: self.depth.saturating_add(1),
            generation_config: generation_config.clone(),
            plan_subagent_control: None,
            plan_subagent_scope,
            plan_link: plan_link.clone(),
            plan_link_runtime,
            activity_hub: activity_hub.clone(),
        }) {
            Ok(runtime) => runtime,
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
            self.mark_cancelled_and_schedule(&agent_id).await;
            return;
        }

        spawn_child_loop(
            self.child_scheduler(),
            ChildLoopLaunch {
                agent_id,
                task_id,
                task,
                token,
                runtime,
                generation_config,
                activity_hub,
            },
        );
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
        if let Some(agent) = state.agents.get_mut(agent_id) {
            if agent.status.is_terminal() {
                return;
            }
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = summary.to_owned();
            agent.diagnostics = Some(diagnostics);
            link = agent.plan_link.clone();
            terminal_activity = terminal_activity_for_agent(agent);
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
    let child_session_id = child_session_id();
    let generation_config = generation_config_for_child_task(&start.task);
    let runtime = scheduler
        .factory
        .build_child(ChildRuntimeInput {
            session_id: child_session_id,
            task_anchor: start.task_anchor.clone(),
            task: start.task.clone(),
            allowed_tools: start.task.allowed_tools().to_vec(),
            workspace_scope: ChildWorkspaceScope::from_task(&start.task),
            depth: scheduler.depth.saturating_add(1),
            generation_config: generation_config.clone(),
            plan_subagent_control: None,
            plan_subagent_scope,
            plan_link: start.plan_link.clone(),
            plan_link_runtime,
            activity_hub: scheduler.attached_activity_hub(),
        })
        .map_err(ReservedChildStartError::Factory)?;
    if start.cancellation_token.is_cancelled() {
        return Ok(ReservedChildStartOutcome::Cancelled);
    }

    let activity_hub = scheduler.attached_activity_hub();
    spawn_child_loop(
        scheduler,
        ChildLoopLaunch {
            agent_id: start.agent_id.clone(),
            task_id: start.task_id.clone(),
            task: start.task.clone(),
            token: start.cancellation_token.clone(),
            runtime,
            generation_config,
            activity_hub,
        },
    );

    Ok(ReservedChildStartOutcome::Started)
}

fn spawn_child_loop(scheduler: ChildScheduler, launch: ChildLoopLaunch) {
    tokio::spawn(async move {
        let mut activity_reducer =
            SubagentActivityReducer::new(launch.agent_id.clone(), launch.task_id.clone());
        if let Some(hub) = launch.activity_hub.as_deref() {
            hub.publish(activity_reducer.starting(crate::plan::unix_time_ms()));
        }

        let input = match StepInput::user_text(launch.task.task()) {
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

        let mut bridge_request = None;
        while let Ok(Some(message)) = stream.next_message().await {
            match message {
                AgentRunMessage::Event(event) => {
                    if let Some(hub) = launch.activity_hub.as_deref()
                        && let Some(snapshot) =
                            activity_reducer.reduce(&event, crate::plan::unix_time_ms())
                    {
                        hub.publish(snapshot);
                    }
                }
                AgentRunMessage::ToolInvocations { batch } => {
                    bridge_request = batch.calls().first().cloned();
                    break;
                }
            }
        }
        let (loop_result, loop_error) = if let Some(call) = bridge_request.as_ref() {
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
            let to_start =
                reserve_queued_starts_locked(&mut state_guard, scheduler.effective_max_threads());
            drop(state_guard);
            scheduler.notify.notify_waiters();
            start_reserved_children_iteratively(scheduler, to_start).await;
            return;
        }
        if let Some(agent) = state_guard.agents.get_mut(&launch.agent_id) {
            if let Some(error) = loop_error {
                agent.status = SubagentStatusLabel::Failed;
                agent.summary = "child runtime stream failed".to_owned();
                agent.result = None;
                agent.diagnostics = Some(error_info("subagent_stream_error", error));
            } else {
                match loop_result {
                    Some(result) => apply_loop_result(agent, &result, child_projection),
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
        }
        let (terminal_link, terminal_status, terminal_activity, terminal_notification) =
            state_guard
                .agents
                .get(&launch.agent_id)
                .map(|agent| {
                    (
                        agent.plan_link.clone(),
                        plan_link_status_for_agent(agent),
                        terminal_activity_for_agent(agent),
                        agent.status.is_terminal().then(|| agent.status_view()),
                    )
                })
                .unwrap_or((None, PlanLinkStatus::Failed, None, None));
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
            scheduler.enqueue_completion_notification(status).await;
        }
        scheduler.notify.notify_waiters();
        start_reserved_children_iteratively(scheduler, to_start).await;
    });
}

pub(super) async fn finish_child_with_status(
    scheduler: ChildScheduler,
    launch: &ChildLoopLaunch,
    reducer: &mut SubagentActivityReducer,
    status: SubagentStatusLabel,
    summary: &str,
    diagnostics: ErrorInfo,
) {
    let transition = update_child_after_error(
        &scheduler.state,
        &launch.agent_id,
        status,
        summary,
        diagnostics,
        scheduler.effective_max_threads(),
    )
    .await;
    if transition.claimed {
        settle_child_terminal(
            scheduler,
            &launch.agent_id,
            reducer,
            launch.activity_hub.clone(),
            transition.to_start,
        )
        .await;
    } else {
        scheduler.notify.notify_waiters();
        start_reserved_children_iteratively(scheduler, transition.to_start).await;
    }
}

async fn settle_child_terminal(
    scheduler: ChildScheduler,
    agent_id: &SubagentId,
    reducer: &mut SubagentActivityReducer,
    activity_hub: Option<Arc<SubagentActivityHub>>,
    to_start: Vec<ReservedChildStart>,
) {
    let (link, link_status, terminal_activity, terminal_notification) = scheduler
        .state
        .lock()
        .await
        .agents
        .get(agent_id)
        .map(|agent| {
            (
                agent.plan_link.clone(),
                plan_link_status_for_agent(agent),
                terminal_activity_for_agent(agent),
                agent.status.is_terminal().then(|| agent.status_view()),
            )
        })
        .unwrap_or((None, PlanLinkStatus::Failed, None, None));
    update_plan_link_with_scheduler(&scheduler, link, link_status).await;
    if let Some((_task_id, phase, summary)) = terminal_activity {
        publish_terminal_activity(activity_hub.as_deref(), reducer, phase, &summary);
    }
    if let Some(status) = terminal_notification {
        scheduler.enqueue_completion_notification(status).await;
    }
    scheduler.notify.notify_waiters();
    start_reserved_children_iteratively(scheduler, to_start).await;
}

fn publish_terminal_activity(
    hub: Option<&SubagentActivityHub>,
    reducer: &mut SubagentActivityReducer,
    phase: SubagentActivityPhase,
    summary: &str,
) {
    if let Some(hub) = hub {
        hub.publish(reducer.terminal(phase, summary, crate::plan::unix_time_ms()));
    }
}

pub(super) fn publish_terminal_activity_snapshot(
    hub: Option<Arc<SubagentActivityHub>>,
    agent_id: SubagentId,
    task_id: SubagentTaskId,
    phase: SubagentActivityPhase,
    summary: &str,
) {
    let Some(hub) = hub else {
        return;
    };
    let mut reducer = SubagentActivityReducer::new(agent_id, task_id);
    publish_terminal_activity(Some(&hub), &mut reducer, phase, summary);
}

fn plan_link_status_for_agent(agent: &ManagedSubagent) -> PlanLinkStatus {
    match agent.status {
        SubagentStatusLabel::Completed => PlanLinkStatus::Completed,
        SubagentStatusLabel::Cancelled => PlanLinkStatus::Cancelled,
        SubagentStatusLabel::Blocked => PlanLinkStatus::Blocked,
        SubagentStatusLabel::Failed => PlanLinkStatus::Failed,
        SubagentStatusLabel::Queued | SubagentStatusLabel::Running => PlanLinkStatus::Failed,
    }
}

pub(super) fn terminal_activity_for_agent(
    agent: &ManagedSubagent,
) -> Option<(SubagentTaskId, SubagentActivityPhase, String)> {
    let phase = match agent.status {
        SubagentStatusLabel::Completed => SubagentActivityPhase::Completed,
        SubagentStatusLabel::Failed => SubagentActivityPhase::Failed,
        SubagentStatusLabel::Blocked => SubagentActivityPhase::Blocked,
        SubagentStatusLabel::Cancelled => SubagentActivityPhase::Cancelled,
        SubagentStatusLabel::Queued | SubagentStatusLabel::Running => return None,
    };
    Some((agent.task_id.clone(), phase, agent.summary.clone()))
}

pub(super) async fn update_plan_link_with_scheduler(
    scheduler: &ChildScheduler,
    link: Option<PlanLinkSnapshot>,
    status: PlanLinkStatus,
) {
    let Some(link) = link else {
        return;
    };
    let runtime = scheduler
        .plan_link_runtime
        .lock()
        .expect("subagent plan link runtime mutex is not poisoned")
        .clone();
    let Some(runtime) = runtime else {
        return;
    };
    if let Err(error) = runtime
        .update_subagent_link(link.binding_id.clone(), status, crate::plan::unix_time_ms())
        .await
    {
        tracing::warn!(
            binding_id = %link.binding_id,
            ?status,
            %error,
            "failed to persist subagent Plan link state"
        );
    }
}

pub(super) fn generation_config_for_child_task(task: &SubagentTaskSpec) -> GenerationConfig {
    GenerationConfig::default().with_reasoning_effort(task.reasoning_effort().cloned())
}
fn child_session_id() -> merry_core::SessionId {
    merry_core::SessionId::random()
}
#[derive(Debug, Default)]
pub(super) struct ChildLoopProjection {
    result: Option<SubagentResultView>,
    changed_paths: Vec<String>,
}

impl ChildLoopProjection {
    async fn from_result(runtime: &Runtime, result: &AgentLoopResult) -> Self {
        let explicit_result = match result.status() {
            AgentLoopStatus::Completed => result
                .final_output()
                .and_then(SubagentResultView::from_conclusion),
            AgentLoopStatus::Failed { .. }
            | AgentLoopStatus::Cancelled { .. }
            | AgentLoopStatus::Blocked { .. } => None,
        };

        Self {
            result: explicit_result,
            changed_paths: changed_paths_from_child_events(runtime, result.events()).await,
        }
    }
}

pub(super) fn apply_loop_result(
    agent: &mut ManagedSubagent,
    result: &AgentLoopResult,
    projection: ChildLoopProjection,
) {
    agent.result = projection.result;
    agent.changed_paths = projection.changed_paths;

    match result.status() {
        AgentLoopStatus::Completed => {
            agent.status = SubagentStatusLabel::Completed;
            agent.summary = "child completed".to_owned();
            agent.output_paths.clear();
        }
        AgentLoopStatus::Failed { diagnostic } => {
            agent.status = SubagentStatusLabel::Failed;
            agent.summary = format!("child failed: {}", diagnostic.message());
            agent.diagnostics = Some(diagnostic.clone());
        }
        AgentLoopStatus::Cancelled { diagnostic } => {
            agent.status = SubagentStatusLabel::Cancelled;
            agent.summary = format!("child cancelled: {}", diagnostic.message());
            agent.diagnostics = Some(diagnostic.clone());
        }
        AgentLoopStatus::Blocked { reason } => {
            agent.status = SubagentStatusLabel::Blocked;
            match reason {
                AgentLoopBlockedReason::MaxModelTurnsReached { max_model_turns } => {
                    agent.summary = format!(
                        "child blocked after {max_model_turns} model turns; spawn a replacement with a larger budget"
                    );
                    agent.diagnostics = Some(error_info(
                        "subagent_max_model_turns_reached",
                        format!(
                            "child used all {max_model_turns} model turns. This is recoverable: inspect the returned status and spawn a replacement with the same plan_client_key and a larger budget within the configured maximum; use a larger value for complex tasks and continue from the shared workspace."
                        ),
                    ));
                }
                _ => {
                    agent.summary = format!("child blocked: {reason:?}");
                    agent.diagnostics = Some(error_info("subagent_blocked", format!("{reason:?}")));
                }
            }
        }
    }
}

async fn changed_paths_from_child_events(
    runtime: &Runtime,
    events: &[RuntimeJournalEvent],
) -> Vec<String> {
    let mut pending_tool_names = BTreeMap::new();
    let mut paths = BTreeSet::new();

    for event in events {
        match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call } => {
                pending_tool_names.insert(call.id().clone(), call.name().clone());
            }
            RuntimeJournalPayload::ToolCallResolved { result }
                if result.status() == ToolCallResultStatus::Succeeded
                    && pending_tool_names
                        .get(result.call_id())
                        .is_some_and(|tool_name| tool_name.as_str() == APPLY_PATCH_TOOL_NAME) =>
            {
                let Ok(content) = runtime.read_artifact_content(result.artifact().id()).await
                else {
                    continue;
                };
                collect_apply_patch_changed_paths(&content, &mut paths);
            }
            _ => {}
        }
    }

    paths.into_iter().collect()
}

fn collect_apply_patch_changed_paths(content: &ArtifactContent, paths: &mut BTreeSet<String>) {
    let Some(text) = content.as_text() else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
        || value.get("tool").and_then(serde_json::Value::as_str) != Some(APPLY_PATCH_TOOL_NAME)
    {
        return;
    }

    let Some(changes) = value.get("changes").and_then(serde_json::Value::as_array) else {
        return;
    };
    for change in changes {
        let Some(path) = change.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if validate_scope_path(PathBuf::from(path)).is_ok() {
            paths.insert(path.to_owned());
        }
    }
}

async fn update_child_after_error(
    state: &Mutex<SubagentManagerState>,
    agent_id: &SubagentId,
    status: SubagentStatusLabel,
    summary: &str,
    diagnostics: ErrorInfo,
    max_threads: usize,
) -> ChildErrorTransition {
    let mut state = state.lock().await;
    let mut claimed = false;
    if let Some(agent) = state.agents.get_mut(agent_id)
        && !agent.status.is_terminal()
    {
        agent.status = status;
        agent.summary = summary.to_owned();
        agent.diagnostics = Some(diagnostics);
        claimed = true;
    }
    ChildErrorTransition {
        claimed,
        to_start: reserve_queued_starts_locked(&mut state, max_threads),
    }
}

pub(super) fn error_info(code: &'static str, message: impl ToString) -> ErrorInfo {
    ErrorInfo::new(code, &sanitize_diagnostic_message(message.to_string()))
        .expect("static diagnostic code and sanitized message are valid")
}

pub(crate) fn sanitize_diagnostic_message(message: String) -> String {
    let sanitized = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim();
    let source = if trimmed.is_empty() {
        "child runtime failed without diagnostic detail"
    } else {
        trimmed
    };

    source.chars().take(4096).collect()
}
