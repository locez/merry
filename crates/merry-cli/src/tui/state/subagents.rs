//! Subagent projection updates stored on `TuiState`.

use crate::tui::{
    state::{TimelineItem, TuiState},
    subagent_view::{SubagentEntry, SubagentPhase},
};

/// Typed payload for inserting or updating a projected subagent card.
pub(crate) struct SubagentUpdate {
    pub(crate) agent_id: String,
    pub(crate) task_id: String,
    pub(crate) display_name: Option<String>,
    pub(crate) phase: SubagentPhase,
    pub(crate) summary: String,
    pub(crate) changed_paths: Option<Vec<String>>,
    pub(crate) output_paths: Option<Vec<String>>,
    pub(crate) diagnostic: Option<String>,
}

impl TuiState {
    pub(crate) fn subagents(&self) -> &[SubagentEntry] {
        &self.subagents
    }

    pub(crate) fn subagent_by_id(&self, agent_id: &str) -> Option<&SubagentEntry> {
        self.subagents
            .iter()
            .find(|entry| entry.agent_id == agent_id)
    }

    pub(crate) fn has_active_subagents(&self) -> bool {
        self.subagents.iter().any(|entry| entry.phase.is_active())
    }

    /// Inserts or updates a projected subagent card and bumps its timeline
    /// version so the cached layout refreshes cleanly without rebuilding history.
    pub(crate) fn upsert_subagent(&mut self, update: SubagentUpdate) {
        if let Some(pos) = self
            .subagents
            .iter()
            .position(|entry| entry.agent_id == update.agent_id)
        {
            let timeline_index = {
                let entry = &mut self.subagents[pos];
                entry.task_id = update.task_id;
                entry.set_display_name(update.display_name);
                entry.phase = update.phase;
                if !update.summary.trim().is_empty() {
                    entry.summary = update.summary.clone();
                    entry.record_log(update.phase, update.summary);
                }
                if let Some(paths) = update.changed_paths {
                    entry.changed_paths = paths;
                }
                if let Some(paths) = update.output_paths {
                    entry.output_paths = paths;
                }
                if let Some(diag) = update.diagnostic {
                    entry.diagnostic = Some(diag);
                }
                entry.timeline_index
            };
            if let Some(index) = timeline_index {
                self.bump_timeline_version(index);
            }
            return;
        }

        let timeline_index = self.timeline.len();
        self.push_timeline_item(TimelineItem::Subagent {
            agent_id: update.agent_id.clone(),
            task_id: update.task_id.clone(),
        });
        let mut entry = SubagentEntry::new(
            update.agent_id,
            update.task_id,
            update.phase,
            update.summary,
        );
        entry.set_display_name(update.display_name);
        entry.timeline_index = Some(timeline_index);
        if let Some(paths) = update.changed_paths {
            entry.changed_paths = paths;
        }
        if let Some(paths) = update.output_paths {
            entry.output_paths = paths;
        }
        if let Some(diag) = update.diagnostic {
            entry.diagnostic = Some(diag);
        }
        self.subagents.push(entry);
    }

    pub(crate) fn update_subagent_activity(
        &mut self,
        snapshots: &[merry_core::SubagentActivitySnapshot],
    ) {
        let mut bumped = Vec::new();
        for snapshot in snapshots {
            let agent_id = snapshot.subagent_id.as_str().to_owned();
            let task_id = snapshot.task_id.as_str().to_owned();
            if let Some(entry) = self
                .subagents
                .iter_mut()
                .find(|entry| entry.agent_id == agent_id)
            {
                entry.task_id = task_id;
                entry.apply_activity(snapshot);
                if let Some(index) = entry.timeline_index {
                    bumped.push(index);
                }
            } else {
                let timeline_index = self.timeline.len();
                self.push_timeline_item(TimelineItem::Subagent {
                    agent_id: agent_id.clone(),
                    task_id: task_id.clone(),
                });
                let mut entry = SubagentEntry::new(
                    agent_id,
                    task_id,
                    snapshot.phase.into(),
                    snapshot.summary.clone(),
                );
                entry.timeline_index = Some(timeline_index);
                self.subagents.push(entry);
            }
        }
        for index in bumped {
            self.bump_timeline_version(index);
        }
    }

    pub(crate) fn apply_subagent_status_views(
        &mut self,
        views: &[merry_runtime::SubagentStatusView],
    ) {
        let mut bumped = Vec::new();
        for view in views {
            let agent_id = view.agent_id.as_str().to_owned();
            let task_id = view.task_id.as_str().to_owned();
            if let Some(entry) = self
                .subagents
                .iter_mut()
                .find(|entry| entry.agent_id == agent_id)
            {
                entry.task_id = task_id;
                entry.apply_status_view(view);
                if let Some(index) = entry.timeline_index {
                    bumped.push(index);
                }
            } else {
                let timeline_index = self.timeline.len();
                self.push_timeline_item(TimelineItem::Subagent {
                    agent_id: agent_id.clone(),
                    task_id: task_id.clone(),
                });
                let mut entry = SubagentEntry::new(
                    agent_id,
                    task_id,
                    view.status.clone().into(),
                    view.summary.clone(),
                );
                entry.apply_status_view(view);
                entry.timeline_index = Some(timeline_index);
                self.subagents.push(entry);
            }
        }
        for index in bumped {
            self.bump_timeline_version(index);
        }
    }
}
