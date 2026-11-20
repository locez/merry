//! Status text, active phase labels, and elapsed-time formatting for [`TuiState`].

use super::TuiState;
use crate::tui::{state::TimelineItem, status::format_session_usage_full};
use merry_core::InteractiveRunState;
use std::time::{Duration, Instant};

impl TuiState {
    #[cfg(test)]
    pub(crate) fn status_text(&self) -> String {
        self.status_parts().join("  ")
    }

    pub(crate) fn command_status_body(&self) -> String {
        let [workspace, model, usage] = self.status_parts();
        let plan = self
            .plan
            .snapshot()
            .map(|snapshot| match snapshot.phase {
                merry_core::PlanPhase::Planning => "planning",
                merry_core::PlanPhase::AwaitingApproval => "awaiting approval",
                merry_core::PlanPhase::Executing => "executing",
                merry_core::PlanPhase::Completed => "completed",
                merry_core::PlanPhase::Blocked => "blocked",
                merry_core::PlanPhase::Cancelled => "cancelled",
            })
            .unwrap_or("none");
        let run = match self.run_state {
            InteractiveRunState::WaitingForInput => "ready",
            InteractiveRunState::RunningModel => "running model",
            InteractiveRunState::RunningTool => "running tool",
            InteractiveRunState::Interrupting => "interrupting",
            InteractiveRunState::Closed => "closed",
        };
        format!("Run: {run}\nModel: {model}\nUsage: {usage}\nPlan: {plan}\nWorkspace: {workspace}")
    }

    pub(crate) fn status_parts(&self) -> [String; 3] {
        let rate = self.output_rate.label();
        let usage = format_session_usage_full(self.usage.as_ref(), &rate);
        let model = self.model_status_label();
        [self.workspace_root.display().to_string(), model, usage]
    }

    pub(crate) fn interaction_status_text(&self) -> String {
        self.interaction_status_text_at(Instant::now())
    }

    pub(crate) fn interaction_status_text_at(&self, now: Instant) -> String {
        if let Some(status) = self.history_search_status() {
            return status;
        }
        match self.run_state {
            InteractiveRunState::WaitingForInput => self
                .comment_status_text()
                .unwrap_or_else(|| self.ready_status_text()),
            InteractiveRunState::RunningModel
            | InteractiveRunState::RunningTool
            | InteractiveRunState::Interrupting => {
                self.active_status_text(self.active_phase_label(), now)
            }
            InteractiveRunState::Closed => "Closed".to_owned(),
        }
    }

    /// Context-aware human label for the current active phase (replaces robotic
    /// "Running model" with natural stage cues).
    pub(crate) fn active_phase_label(&self) -> &'static str {
        match self.run_state {
            InteractiveRunState::RunningModel => match self.timeline.last() {
                Some(TimelineItem::Thinking { elapsed: None, .. }) => "Thinking…",
                Some(TimelineItem::Assistant { .. }) => "Responding…",
                _ => "Working…",
            },
            InteractiveRunState::RunningTool => "Working…",
            InteractiveRunState::Interrupting => "Stopping…",
            InteractiveRunState::WaitingForInput => "Ready",
            InteractiveRunState::Closed => "Closed",
        }
    }

    pub(crate) fn active_run_elapsed_label(&self, now: Instant) -> Option<String> {
        if !self.is_active_run() {
            return None;
        }
        let elapsed = self
            .active_run_started_at
            .map(|started_at| now.saturating_duration_since(started_at))
            .unwrap_or_default();
        Some(format_elapsed(elapsed))
    }

    fn ready_status_text(&self) -> String {
        self.last_completed_run_elapsed
            .map(|elapsed| format!("Ready · last {}", format_elapsed(elapsed)))
            .unwrap_or_else(|| "Ready".to_owned())
    }

    fn active_status_text(&self, label: &str, now: Instant) -> String {
        let elapsed = self
            .active_run_started_at
            .map(|started_at| now.saturating_duration_since(started_at))
            .unwrap_or_default();
        format!("{label} · {}", format_elapsed(elapsed))
    }

    pub(super) fn model_status_label(&self) -> String {
        self.reasoning_effort_label
            .as_deref()
            .filter(|label| !label.is_empty())
            .map(|label| format!("{} {}", self.model_label, label))
            .unwrap_or_else(|| self.model_label.clone())
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    let total_seconds = elapsed.as_secs();
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    if total_minutes == 0 {
        return format!("{seconds}s");
    }

    let minutes = total_minutes % 60;
    let hours = total_minutes / 60;
    if hours == 0 {
        format!("{total_minutes}m {seconds:02}s")
    } else {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    }
}
