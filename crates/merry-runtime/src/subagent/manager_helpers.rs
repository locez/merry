//! Pure helper functions and status projections for [`super::manager::SubagentManager`].

use super::{
    manager::{ManagedSubagent, SubagentManagerState},
    protocol::{SubagentStatusLabel, SubagentStatusView, WaitMode},
    spec::{SubagentError, SubagentTaskSpec},
};
use merry_core::{PlanLinkStatus, SubagentActivityPhase, SubagentId, SubagentTaskId};
use std::path::{Path, PathBuf};

pub(super) fn plan_link_status_for_agent(agent: &ManagedSubagent) -> PlanLinkStatus {
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

pub(super) fn wait_mode_satisfied(mode: WaitMode, agents: &[SubagentStatusView]) -> bool {
    match mode {
        WaitMode::Any => agents.iter().any(SubagentStatusView::is_terminal),
        WaitMode::All => agents.iter().all(SubagentStatusView::is_terminal),
    }
}

impl ManagedSubagent {
    pub(super) fn status_view(&self) -> SubagentStatusView {
        let summary = if self.summary.is_empty() {
            fallback_summary(self.status.clone(), &self.task)
        } else {
            self.summary.clone()
        };
        let elapsed_ms = if self.status.is_terminal() {
            self.elapsed_ms
        } else {
            self.started_at
                .map(|started| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(self.elapsed_ms)
        };

        SubagentStatusView {
            agent_id: self.agent_id.clone(),
            task_id: self.task_id.clone(),
            display_name: self.task.display_name().map(str::to_owned),
            role: self.task.role().map(str::to_owned),
            status: self.status.clone(),
            summary,
            result: self.result.clone(),
            output_paths: self.output_paths.clone(),
            changed_paths: self.changed_paths.clone(),
            diagnostics: self.diagnostics.clone(),
            turns_used: self.turns_used,
            tool_calls: self.tool_calls,
            elapsed_ms,
            usage: self.usage,
            raw_output_bytes: self.raw_output_bytes,
            worktree: self.worktree.clone(),
        }
    }
}

pub(super) fn selected_statuses(
    state: &SubagentManagerState,
    agent_ids: &[SubagentId],
) -> Vec<SubagentStatusView> {
    state
        .agents
        .values()
        .filter(|agent| agent_ids.contains(&agent.agent_id))
        .map(ManagedSubagent::status_view)
        .collect()
}

pub(super) fn paths_to_strings(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

pub(super) fn ensure_scope_within_parent(
    field: &'static str,
    requested: &[PathBuf],
    parent: &[PathBuf],
) -> Result<(), SubagentError> {
    for path in requested {
        if !parent
            .iter()
            .any(|root| root == Path::new(".") || path == root || path.starts_with(root))
        {
            return Err(SubagentError::CapabilityExpansion {
                field,
                value: path.display().to_string(),
            });
        }
    }
    Ok(())
}

pub(super) fn initial_summary(status: SubagentStatusLabel) -> String {
    match status {
        SubagentStatusLabel::Queued => "child queued".to_owned(),
        SubagentStatusLabel::Running => "child running".to_owned(),
        SubagentStatusLabel::Completed => "child completed".to_owned(),
        SubagentStatusLabel::Failed => "child failed".to_owned(),
        SubagentStatusLabel::Blocked => "child blocked".to_owned(),
        SubagentStatusLabel::Cancelled => "child cancelled".to_owned(),
    }
}

pub(super) fn fallback_summary(status: SubagentStatusLabel, task: &SubagentTaskSpec) -> String {
    match task.display_name() {
        Some(display_name) => format!("{}: {display_name}", initial_summary(status)),
        None => initial_summary(status),
    }
}

pub(super) fn apply_built_task_overrides(
    mut task: SubagentTaskSpec,
    built: &super::BuiltChildRuntime,
    fallback_timeout_secs: Option<u64>,
    fallback_max_total_tokens: Option<u64>,
) -> SubagentTaskSpec {
    if let Some(turns) = built.max_model_turns
        && turns > 0
    {
        task = task.with_max_model_turns_unchecked(turns);
    }
    let effective_timeout = built
        .timeout_secs
        .or_else(|| task.timeout_secs())
        .or(fallback_timeout_secs);
    if let Some(timeout) = effective_timeout
        && let Ok(updated) = task.clone().with_timeout_secs(Some(timeout))
    {
        task = updated;
    }
    let effective_max_tokens = built
        .max_total_tokens
        .or_else(|| task.max_total_tokens())
        .or(fallback_max_total_tokens);
    if let Some(max_tokens) = effective_max_tokens {
        task = task.with_max_total_tokens(Some(max_tokens));
    }
    if let Some(role) = built.resolved_role.as_deref()
        && let Ok(updated) = task.clone().with_role(Some(role.to_owned()))
    {
        task = updated;
    }
    task
}

pub(super) fn generation_config_for_child_task_with_parent(
    task: &SubagentTaskSpec,
    parent_generation: &merry_llm::GenerationConfig,
) -> merry_llm::GenerationConfig {
    match task.reasoning_effort() {
        Some(effort) => parent_generation
            .clone()
            .with_reasoning_effort(Some(effort.clone())),
        None => parent_generation.clone(),
    }
}

pub(super) fn error_info(code: &'static str, message: impl ToString) -> merry_core::ErrorInfo {
    merry_core::ErrorInfo::new(code, &sanitize_diagnostic_message(message.to_string()))
        .expect("static diagnostic code and sanitized message are valid")
}

pub(super) fn observed_event_output_bytes(event: &merry_core::RuntimeEvent) -> u64 {
    let bytes = match event {
        merry_core::RuntimeEvent::AssistantMessageDelta { delta, .. }
        | merry_core::RuntimeEvent::ReasoningDelta { delta, .. }
        | merry_core::RuntimeEvent::ToolCallOutputDelta { delta, .. } => delta.len(),
        merry_core::RuntimeEvent::ToolCallFinished { output, .. } => {
            output.as_ref().map_or(0, |value| value.as_str().len())
        }
        _ => 0,
    };
    u64::try_from(bytes).unwrap_or(u64::MAX)
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
