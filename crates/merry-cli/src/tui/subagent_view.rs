//! Interactive Subagent Drill-In projection state and overlay types.
//!
//! Runtime owns child-session lifecycle and execution; this module strictly
//! projects runtime-emitted `RuntimeEvent::Subagent*`, live
//! `SubagentActivitySnapshot`, and `SubagentStatusView` snapshots into a
//! navigable, clickable inspection surface inspired by `.merry/grok-build`.

mod render;
#[cfg(test)]
mod tests;

pub(crate) use render::{render_subagent_takeover, subagent_takeover_pointer};

use super::{
    motion::{self, MotionClock},
    projector::TuiProjector,
    state::{TimelineItem, TuiState},
    theme::SemanticColor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{RuntimeEvent, SubagentActivityPhase, SubagentActivitySnapshot, SubagentStatus};
use merry_runtime::{SessionTranscriptItem, SubagentStatusLabel, SubagentStatusView};
use std::time::Duration;

const MAX_ACTIVITY_LOG_PER_SUBAGENT: usize = 64;

/// UI-facing lifecycle phase for a projected subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubagentPhase {
    Starting,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Blocked,
}

impl SubagentPhase {
    pub(crate) fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Waiting)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Starting => "STARTING",
            Self::Running => "RUNNING",
            Self::Waiting => "WAITING",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::Blocked => "BLOCKED",
        }
    }

    pub(crate) fn color(self) -> SemanticColor {
        match self {
            Self::Starting | Self::Running => SemanticColor::ToolKeyword,
            Self::Waiting => SemanticColor::Focus,
            Self::Completed => SemanticColor::Success,
            Self::Failed => SemanticColor::Error,
            Self::Cancelled | Self::Blocked => SemanticColor::Warning,
        }
    }

    pub(crate) fn glyph(self, clock: MotionClock) -> &'static str {
        match self {
            Self::Starting | Self::Running | Self::Waiting => {
                motion::spinner_glyph(Duration::from_millis(clock.millis()))
            }
            Self::Completed => "✓",
            Self::Failed => "✗",
            Self::Cancelled => "⊘",
            Self::Blocked => "!",
        }
    }
}

impl From<SubagentActivityPhase> for SubagentPhase {
    fn from(value: SubagentActivityPhase) -> Self {
        match value {
            SubagentActivityPhase::Starting => Self::Starting,
            SubagentActivityPhase::Running => Self::Running,
            SubagentActivityPhase::Waiting => Self::Waiting,
            SubagentActivityPhase::Completed => Self::Completed,
            SubagentActivityPhase::Failed => Self::Failed,
            SubagentActivityPhase::Cancelled => Self::Cancelled,
            SubagentActivityPhase::Blocked => Self::Blocked,
        }
    }
}

impl From<SubagentStatus> for SubagentPhase {
    fn from(value: SubagentStatus) -> Self {
        match value {
            SubagentStatus::Queued => Self::Starting,
            SubagentStatus::Running => Self::Running,
            SubagentStatus::Completed => Self::Completed,
            SubagentStatus::Failed => Self::Failed,
            SubagentStatus::Cancelled => Self::Cancelled,
            SubagentStatus::Blocked => Self::Blocked,
        }
    }
}

impl From<SubagentStatusLabel> for SubagentPhase {
    fn from(value: SubagentStatusLabel) -> Self {
        match value {
            SubagentStatusLabel::Queued => Self::Starting,
            SubagentStatusLabel::Running => Self::Running,
            SubagentStatusLabel::Completed => Self::Completed,
            SubagentStatusLabel::Failed => Self::Failed,
            SubagentStatusLabel::Cancelled => Self::Cancelled,
            SubagentStatusLabel::Blocked => Self::Blocked,
        }
    }
}

/// Bounded activity entry projected from runtime updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubagentLogEntry {
    pub(crate) phase: SubagentPhase,
    pub(crate) summary: String,
}

/// Projected UI state for one runtime-owned subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubagentEntry {
    pub(crate) agent_id: String,
    pub(crate) task_id: String,
    pub(crate) phase: SubagentPhase,
    pub(crate) summary: String,
    pub(crate) activity_log: Vec<SubagentLogEntry>,
    pub(crate) conclusion: Option<String>,
    pub(crate) changed_paths: Vec<String>,
    pub(crate) output_paths: Vec<String>,
    pub(crate) diagnostic: Option<String>,
    pub(crate) timeline_index: Option<usize>,
}

impl SubagentEntry {
    pub(crate) fn new(
        agent_id: String,
        task_id: String,
        phase: SubagentPhase,
        summary: String,
    ) -> Self {
        let mut entry = Self {
            agent_id,
            task_id,
            phase,
            summary: summary.clone(),
            activity_log: Vec::new(),
            conclusion: None,
            changed_paths: Vec::new(),
            output_paths: Vec::new(),
            diagnostic: None,
            timeline_index: None,
        };
        entry.record_log(phase, summary);
        entry
    }

    pub(crate) fn record_log(&mut self, phase: SubagentPhase, summary: String) {
        let trimmed = summary.trim();
        if trimmed.is_empty() {
            return;
        }
        if self
            .activity_log
            .last()
            .is_some_and(|last| last.phase == phase && last.summary == trimmed)
        {
            return;
        }
        if self.activity_log.len() >= MAX_ACTIVITY_LOG_PER_SUBAGENT {
            self.activity_log.remove(0);
        }
        self.activity_log.push(SubagentLogEntry {
            phase,
            summary: trimmed.to_owned(),
        });
    }

    pub(crate) fn apply_activity(&mut self, snapshot: &SubagentActivitySnapshot) {
        self.phase = snapshot.phase.into();
        if !snapshot.summary.trim().is_empty() {
            self.summary = snapshot.summary.clone();
            self.record_log(self.phase, snapshot.summary.clone());
        }
    }

    pub(crate) fn apply_status_view(&mut self, view: &SubagentStatusView) {
        self.phase = view.status.clone().into();
        if !view.summary.trim().is_empty() {
            self.summary = view.summary.clone();
            self.record_log(self.phase, view.summary.clone());
        }
        if let Some(result) = &view.result {
            self.conclusion = Some(result.conclusion.clone());
        }
        if !view.changed_paths.is_empty() {
            self.changed_paths = view.changed_paths.clone();
        }
        if !view.output_paths.is_empty() {
            self.output_paths = view.output_paths.clone();
        }
        if let Some(diag) = &view.diagnostics {
            self.diagnostic = Some(format!("{}: {}", diag.code(), diag.message()));
        }
    }
}

/// Overlay state for the fullscreen interactive Subagent Takeover View.
///
/// Holds a lazily-populated child [`TuiState`] + [`TuiProjector`] for the
/// currently focused subagent so the user sees its full timeline (Thinking,
/// collapsible tools, Markdown replies) and can send follow-up instructions
/// directly via the embedded composer. Closing the overlay drops the child
/// state immediately to keep parent memory bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubagentTakeoverOverlay {
    selected_agent_id: String,
    child_state: Box<TuiState>,
    child_projector: TuiProjector,
    transcript_loaded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubagentTakeoverAction {
    Consumed,
    Back,
    Previous,
    Next,
    Refresh,
    CopyText(String),
    Stop(String),
    SendMessage { agent_id: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubagentTakeoverPointer {
    Back,
    Previous,
    Next,
    Refresh,
    Stop,
    SelectAgent(String),
    Composer,
    TimelineToggleCommand(usize),
    TimelineToggleThinking(usize),
    TimelineCopy(String),
    TimelineCopyTooLarge,
}

impl SubagentTakeoverOverlay {
    pub(crate) fn new(agent_id: String, parent: &TuiState) -> Self {
        let mut child_state = fresh_child_state(parent);
        if let Some(entry) = parent.subagent_by_id(&agent_id) {
            seed_fallback_timeline(&mut child_state, entry);
        }
        Self {
            selected_agent_id: agent_id,
            child_state: Box::new(child_state),
            child_projector: TuiProjector::default(),
            transcript_loaded: false,
        }
    }

    pub(crate) fn selected_agent_id(&self) -> &str {
        &self.selected_agent_id
    }

    pub(crate) fn child_state(&self) -> &TuiState {
        &self.child_state
    }

    pub(crate) fn child_state_mut(&mut self) -> &mut TuiState {
        &mut self.child_state
    }

    pub(crate) fn select_agent(&mut self, agent_id: String, parent: &TuiState) {
        if self.selected_agent_id != agent_id {
            self.selected_agent_id = agent_id;
            let mut fresh = fresh_child_state(parent);
            if let Some(entry) = parent.subagent_by_id(&self.selected_agent_id) {
                seed_fallback_timeline(&mut fresh, entry);
            }
            *self.child_state = fresh;
            self.child_projector = TuiProjector::default();
            self.transcript_loaded = false;
        }
    }

    /// Replays durable transcript items loaded lazily from `merry-runtime`
    /// into this takeover view's child timeline.
    pub(crate) fn load_transcript(
        &mut self,
        agent_id: &str,
        task: &str,
        transcript: &[SessionTranscriptItem],
        fallback_entry: Option<&SubagentEntry>,
    ) {
        if self.selected_agent_id != agent_id {
            return;
        }
        let preserved_input = self
            .child_state
            .plain_input_text()
            .unwrap_or_default()
            .to_owned();
        let mut fresh = fresh_child_state(&self.child_state);
        if !preserved_input.is_empty() {
            fresh.insert_input_paste(&preserved_input);
        }
        let mut projector = TuiProjector::default();
        if transcript.is_empty() {
            if !task.trim().is_empty() {
                fresh.push_timeline_item(TimelineItem::User {
                    text: task.trim().to_owned(),
                });
            }
            if let Some(entry) = fallback_entry {
                append_fallback_summary(&mut fresh, entry);
            }
        } else {
            for item in transcript {
                projector.apply_transcript_item(item.clone(), &mut fresh);
            }
        }
        *self.child_state = fresh;
        self.child_projector = projector;
        self.transcript_loaded = true;
    }

    /// Applies a live [`RuntimeEvent`] emitted by the inspected subagent.
    pub(crate) fn apply_live_event(&mut self, event: RuntimeEvent) {
        self.child_projector.apply(event, &mut self.child_state);
    }

    pub(crate) fn scroll_up(&mut self, lines: usize) {
        self.child_state.scroll_timeline_up_by(lines.max(1));
    }

    pub(crate) fn scroll_down(&mut self, lines: usize) {
        self.child_state.scroll_timeline_down_by(lines.max(1));
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        self.child_state.insert_input_paste(text);
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> SubagentTakeoverAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let super_key = key.modifiers.contains(KeyModifiers::SUPER);
        let input_empty = self
            .child_state
            .plain_input_text()
            .is_none_or(str::is_empty);

        match key.code {
            KeyCode::Esc => {
                if self.child_state.clear_input_selection() {
                    SubagentTakeoverAction::Consumed
                } else {
                    SubagentTakeoverAction::Back
                }
            }
            KeyCode::Char('c' | 'C') if (ctrl || super_key) && !alt => {
                if let Some(selected) = self.child_state.selected_input_text() {
                    SubagentTakeoverAction::CopyText(selected)
                } else if super_key {
                    SubagentTakeoverAction::Consumed
                } else if !input_empty {
                    self.child_state.clear_input();
                    SubagentTakeoverAction::Consumed
                } else {
                    SubagentTakeoverAction::Stop(self.selected_agent_id.clone())
                }
            }
            KeyCode::Char('x' | 'X') if (ctrl || super_key) && !alt => {
                if let Some(cut) = self.child_state.cut_input_selection() {
                    SubagentTakeoverAction::CopyText(cut)
                } else {
                    SubagentTakeoverAction::Consumed
                }
            }
            KeyCode::F(5) => SubagentTakeoverAction::Refresh,
            KeyCode::Enter if shift || alt => {
                self.child_state.insert_input_newline();
                SubagentTakeoverAction::Consumed
            }
            KeyCode::Enter => {
                let message = self
                    .child_state
                    .plain_input_text()
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if message.is_empty() {
                    return SubagentTakeoverAction::Consumed;
                }
                self.child_state.clear_input();
                self.child_state.push_local_user_echo(message.clone());
                self.child_state.follow_latest();
                SubagentTakeoverAction::SendMessage {
                    agent_id: self.selected_agent_id.clone(),
                    message,
                }
            }
            KeyCode::Left if input_empty && !shift && !super_key => {
                SubagentTakeoverAction::Previous
            }
            KeyCode::Right if input_empty && !shift && !super_key => SubagentTakeoverAction::Next,
            KeyCode::Up if input_empty && !shift && !super_key => {
                self.scroll_up(2);
                SubagentTakeoverAction::Consumed
            }
            KeyCode::Down if input_empty && !shift && !super_key => {
                self.scroll_down(2);
                SubagentTakeoverAction::Consumed
            }
            KeyCode::Up if shift && !ctrl && !alt && !super_key => {
                self.child_state.select_input_line(false);
                SubagentTakeoverAction::Consumed
            }
            KeyCode::Down if shift && !ctrl && !alt && !super_key => {
                self.child_state.select_input_line(true);
                SubagentTakeoverAction::Consumed
            }
            KeyCode::Up if !ctrl && !alt && !super_key => {
                let _ = self.child_state.move_input_line(false);
                SubagentTakeoverAction::Consumed
            }
            KeyCode::Down if !ctrl && !alt && !super_key => {
                let _ = self.child_state.move_input_line(true);
                SubagentTakeoverAction::Consumed
            }
            KeyCode::PageUp => {
                self.scroll_up(10);
                SubagentTakeoverAction::Consumed
            }
            KeyCode::PageDown => {
                self.scroll_down(10);
                SubagentTakeoverAction::Consumed
            }
            _ => {
                self.child_state.handle_input_key(key);
                SubagentTakeoverAction::Consumed
            }
        }
    }
}

fn fresh_child_state(template: &TuiState) -> TuiState {
    TuiState::new(
        template.workspace_root().to_path_buf(),
        template.model_display_label(),
        template.keymap().clone(),
        template.theme().clone(),
    )
    .with_successful_command_output(template.show_successful_command_output())
    .with_animations(template.animations_enabled())
    .with_max_reading_width(template.max_reading_width())
}

fn seed_fallback_timeline(child: &mut TuiState, entry: &SubagentEntry) {
    if !entry.task_id.is_empty() {
        child.push_timeline_item(TimelineItem::Muted {
            title: "Subagent".to_owned(),
            detail: format!("{} ({})", entry.agent_id, entry.task_id),
        });
    }
    append_fallback_summary(child, entry);
}

fn append_fallback_summary(child: &mut TuiState, entry: &SubagentEntry) {
    if !entry.summary.trim().is_empty() {
        child.push_timeline_item(TimelineItem::Muted {
            title: entry.phase.label().to_owned(),
            detail: entry.summary.clone(),
        });
    }
    if let Some(conclusion) = &entry.conclusion {
        child.push_timeline_item(TimelineItem::Assistant {
            text: conclusion.clone(),
        });
    }
    if let Some(diag) = &entry.diagnostic {
        child.push_timeline_item(TimelineItem::Diagnostic {
            title: "Subagent diagnostic".to_owned(),
            body: diag.clone(),
        });
    }
    if !entry.changed_paths.is_empty() {
        child.push_timeline_item(TimelineItem::Muted {
            title: "Changed files".to_owned(),
            detail: entry.changed_paths.join(", "),
        });
    }
    if !entry.output_paths.is_empty() {
        child.push_timeline_item(TimelineItem::Muted {
            title: "Output artifacts".to_owned(),
            detail: entry.output_paths.join(", "),
        });
    }
}
