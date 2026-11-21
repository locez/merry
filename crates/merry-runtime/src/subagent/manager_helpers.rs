//! Pure helper functions and status projections for [`super::manager::SubagentManager`].

use super::{
    manager::{ManagedSubagent, SubagentManagerState},
    protocol::{SubagentStatusLabel, SubagentStatusView, WaitMode},
    spec::{SubagentError, SubagentTaskSpec},
};
use merry_core::SubagentId;
use std::path::{Path, PathBuf};

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
