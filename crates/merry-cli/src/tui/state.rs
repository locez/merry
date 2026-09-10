use crate::tui::{
    completion::{CompletionMenu, CompletionSources},
    input::{DraftImage, InputHistory, TextInput, TuiSubmission},
    keymap::Keymap,
    plan::PlanUiState,
    preferences::{TuiPreferences, TuiSettingsDefaults},
    status::{format_header_status_parts, format_session_usage_full},
    text_interaction::TextSelection,
    theme::TuiTheme,
};
use clipboard::ClipboardFeedback;
use merry_core::{InteractiveRunState, QueuedInputLane, SessionUsage};
use merry_runtime::SkillMetadata;
use overlays::OverlayState;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
pub(crate) use timeline::TimelineAnchor;
pub(crate) use views::{
    CommandFailure, CommandView, PatchChangeView, PatchLineKind, PatchLineView,
    ProcessOutputPreview, QueuePreview, QueuePreviewItem, QueuePreviewState, TimelineItem,
};

mod overlays;

mod views;

mod settings;

mod clipboard;
mod text_interaction;
mod timeline;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiState {
    workspace_root: PathBuf,
    model_label: String,
    reasoning_effort_label: Option<String>,
    keymap: Keymap,
    theme: TuiTheme,
    input: TextInput,
    completion_sources: CompletionSources,
    completion_menu: Option<CompletionMenu>,
    input_history: InputHistory,
    queue_preview: QueuePreviewState,
    timeline: Vec<TimelineItem>,
    show_successful_command_output: bool,
    command_details_generation: u64,
    timeline_scroll_offset: usize,
    timeline_review_user_index: Option<usize>,
    timeline_anchor: Option<TimelineAnchor>,
    timeline_has_updates: bool,
    text_selection: Option<TextSelection>,
    clipboard_feedback: Option<ClipboardFeedback>,
    pending_local_echoes: Vec<PendingLocalEcho>,
    pending_local_run_start: bool,
    stop_feedback: StopFeedbackState,
    run_state: InteractiveRunState,
    active_run_started_at: Option<Instant>,
    last_completed_run_elapsed: Option<Duration>,
    pending_empty_input_quit: bool,
    usage: Option<SessionUsage>,
    overlays: OverlayState,
    preferences: TuiPreferences,
    settings_defaults: TuiSettingsDefaults,
    plan: PlanUiState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingLocalEcho {
    text: String,
    lane: QueuedInputLane,
    timeline_index: usize,
}

/// A stop request moves from idle to pending, then completes at a cancellation boundary.
/// A new active run resets the state only when entered from an inactive boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StopFeedbackState {
    #[default]
    Idle,
    Pending(usize),
    Completed,
}

impl TuiState {
    pub(crate) fn new(
        workspace_root: PathBuf,
        model_label: String,
        keymap: Keymap,
        theme: TuiTheme,
    ) -> Self {
        Self {
            workspace_root: workspace_root.clone(),
            model_label,
            reasoning_effort_label: None,
            keymap,
            theme,
            input: TextInput::default(),
            completion_sources: CompletionSources::new(workspace_root.clone(), Vec::new()),
            completion_menu: None,
            input_history: InputHistory::default(),
            queue_preview: QueuePreviewState::from_preview(QueuePreview::empty()),
            timeline: Vec::new(),
            show_successful_command_output: false,
            command_details_generation: 0,
            timeline_scroll_offset: 0,
            timeline_review_user_index: None,
            timeline_anchor: None,
            timeline_has_updates: false,
            text_selection: None,
            clipboard_feedback: None,
            pending_local_echoes: Vec::new(),
            pending_local_run_start: false,
            stop_feedback: StopFeedbackState::Idle,
            run_state: InteractiveRunState::WaitingForInput,
            active_run_started_at: None,
            last_completed_run_elapsed: None,
            pending_empty_input_quit: false,
            usage: None,
            overlays: OverlayState::default(),
            preferences: TuiPreferences::default(),
            settings_defaults: TuiSettingsDefaults::default(),
            plan: PlanUiState::default(),
        }
    }

    /// Sets the display-only success-output policy without changing runtime artifacts.
    pub(crate) fn with_successful_command_output(mut self, show_output: bool) -> Self {
        self.show_successful_command_output = show_output;
        self
    }

    /// Whether completed successful commands show their bounded output preview.
    pub(crate) fn show_successful_command_output(&self) -> bool {
        self.show_successful_command_output
    }

    pub(crate) fn plan(&self) -> &PlanUiState {
        &self.plan
    }

    pub(crate) fn plan_mut(&mut self) -> &mut PlanUiState {
        &mut self.plan
    }

    #[cfg(test)]
    pub(crate) fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    pub(crate) fn set_completion_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.completion_sources = CompletionSources::new(self.workspace_root.clone(), skills);
        self.refresh_completion_menu();
    }

    #[cfg(test)]
    pub(crate) fn input_text(&self) -> &str {
        self.input.text()
    }

    pub(crate) fn plain_input_text(&self) -> Option<&str> {
        self.input.plain_text()
    }

    pub(crate) fn clear_input(&mut self) {
        self.input.clear();
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
    }

    pub(crate) fn input_viewport_rows(
        &self,
        max_width: usize,
        max_rows: usize,
    ) -> super::input::TextInputViewport {
        self.input.viewport_rows(max_width, max_rows)
    }

    pub(crate) fn input_visible_rows(&self, max_rows: usize) -> usize {
        self.input
            .text()
            .split('\n')
            .count()
            .max(1)
            .min(max_rows.max(1))
    }

    pub(crate) fn take_input_for_submit(&mut self) -> Option<TuiSubmission> {
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
        match self.input.take_submission() {
            Ok(submission) => submission,
            Err(error) => {
                self.push_timeline_item(TimelineItem::Diagnostic {
                    title: "user_input".to_owned(),
                    body: error.to_string(),
                });
                None
            }
        }
    }

    pub(crate) fn previous_input_history(&mut self) {
        self.pending_empty_input_quit = false;
        self.input_history.previous(&mut self.input);
        self.refresh_completion_menu();
    }

    pub(crate) fn next_input_history(&mut self) {
        self.pending_empty_input_quit = false;
        self.input_history.next(&mut self.input);
        self.refresh_completion_menu();
    }

    pub(crate) fn set_input_history(&mut self, entries: Vec<String>) {
        self.input_history.replace_entries(entries);
    }

    pub(crate) fn record_input_history(&mut self, text: &str) {
        self.input_history.record(text);
    }

    #[cfg(test)]
    pub(crate) fn input_history_entries(&self) -> &[String] {
        self.input_history.entries()
    }

    pub(crate) fn handle_input_key(&mut self, key: crossterm::event::KeyEvent) {
        self.pending_empty_input_quit = false;
        self.input.handle_key(key);
        self.refresh_completion_menu();
    }

    #[cfg(test)]
    pub(crate) fn insert_input_str(&mut self, text: &str) {
        self.pending_empty_input_quit = false;
        self.input.insert_str(text);
        self.refresh_completion_menu();
    }

    pub(crate) fn insert_input_paste(&mut self, text: &str) {
        self.pending_empty_input_quit = false;
        self.input.insert_paste(text);
        self.refresh_completion_menu();
    }

    pub(crate) fn insert_input_image(
        &mut self,
        image: DraftImage,
    ) -> Result<(), merry_runtime::RuntimeError> {
        self.pending_empty_input_quit = false;
        self.input.insert_image(image)?;
        self.refresh_completion_menu();
        Ok(())
    }

    pub(crate) fn insert_input_newline(&mut self) {
        self.pending_empty_input_quit = false;
        self.input.insert_newline();
        self.close_completion_menu();
    }

    pub(crate) fn completion_menu(&self) -> Option<&CompletionMenu> {
        self.completion_menu.as_ref()
    }

    pub(crate) fn close_completion_menu(&mut self) {
        self.completion_menu = None;
    }

    pub(crate) fn select_next_completion(&mut self) -> bool {
        let Some(menu) = self.completion_menu.as_mut() else {
            return false;
        };
        menu.select_next();
        true
    }

    pub(crate) fn select_previous_completion(&mut self) -> bool {
        let Some(menu) = self.completion_menu.as_mut() else {
            return false;
        };
        menu.select_previous();
        true
    }

    pub(crate) fn accept_completion(&mut self) -> bool {
        let Some(menu) = self.completion_menu.take() else {
            return false;
        };
        let Some(replacement) = menu.replacement_text() else {
            return false;
        };
        self.pending_empty_input_quit = false;
        self.input
            .replace_range(menu.replacement_range(), &replacement);
        if !menu.is_slash() {
            self.refresh_completion_menu();
        }
        true
    }

    fn refresh_completion_menu(&mut self) {
        let menu = self.completion_sources.menu_for_input(
            self.input.text(),
            self.input.cursor_byte_index(),
            self.completion_menu.as_ref(),
        );
        self.completion_menu =
            menu.filter(|menu| !menu.is_slash() || self.input.plain_text().is_some());
    }

    pub(crate) fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub(crate) fn theme(&self) -> &TuiTheme {
        &self.theme
    }

    pub(crate) fn replace_settings_defaults(&mut self, defaults: TuiSettingsDefaults) {
        self.settings_defaults = defaults;
    }

    pub(crate) fn current_provider_alias(&self) -> Option<&str> {
        self.preferences
            .provider
            .as_deref()
            .or(self.settings_defaults.provider.as_deref())
    }

    pub(crate) fn replace_preferences(&mut self, preferences: TuiPreferences) {
        self.preferences = preferences;
    }

    pub(crate) fn timeline(&self) -> &[TimelineItem] {
        &self.timeline
    }

    pub(crate) fn latest_user_input_title(&self) -> Option<String> {
        self.timeline.iter().rev().find_map(|item| {
            let TimelineItem::User { text, .. } = item else {
                return None;
            };
            let title = compact_title(text);
            (!title.is_empty()).then_some(title)
        })
    }

    pub(crate) fn push_timeline_item(&mut self, item: TimelineItem) {
        self.note_timeline_update();
        self.timeline.push(item);
    }

    pub(crate) fn append_assistant_delta(&mut self, index: Option<usize>, delta: &str) -> usize {
        self.note_timeline_update();
        if let Some(index) = index
            && let Some(TimelineItem::Assistant { text }) = self.timeline.get_mut(index)
        {
            text.push_str(delta);
            index
        } else {
            self.timeline.push(TimelineItem::Assistant {
                text: delta.to_owned(),
            });
            self.timeline.len().saturating_sub(1)
        }
    }

    pub(crate) fn push_user_timeline_item(&mut self, text: String, lane: QueuedInputLane) {
        self.push_timeline_item(TimelineItem::User { text, lane });
    }

    pub(crate) fn push_local_user_echo(&mut self, text: String, lane: QueuedInputLane) {
        let timeline_index = self.timeline.len();
        self.pending_local_echoes.push(PendingLocalEcho {
            text: text.clone(),
            lane,
            timeline_index,
        });
        self.push_user_timeline_item(text, lane);
    }

    pub(crate) fn confirm_or_push_user_input(&mut self, text: String, lane: QueuedInputLane) {
        let exact = self
            .pending_local_echoes
            .iter()
            .position(|echo| echo.text == text && echo.lane == lane);
        let moved_to_suspended = (lane == QueuedInputLane::Suspended).then(|| {
            self.pending_local_echoes
                .iter()
                .position(|echo| echo.text == text && echo.lane == QueuedInputLane::Next)
        });
        if let Some(index) = exact.or_else(|| moved_to_suspended.flatten()) {
            let echo = self.pending_local_echoes.remove(index);
            if echo.lane != lane
                && let Some(TimelineItem::User {
                    lane: timeline_lane,
                    ..
                }) = self.timeline.get_mut(echo.timeline_index)
            {
                *timeline_lane = lane;
            }
            return;
        }

        self.push_user_timeline_item(text, lane);
    }

    pub(crate) fn replace_timeline_item(&mut self, index: usize, item: TimelineItem) {
        self.note_timeline_update();
        if let Some(slot) = self.timeline.get_mut(index) {
            *slot = item;
        }
    }

    pub(crate) fn begin_stop_feedback(&mut self) {
        self.set_run_state(InteractiveRunState::Interrupting);
        if self.stop_feedback == StopFeedbackState::Idle {
            self.push_pending_stop_feedback("Stopping", "Interrupt requested for the active run.");
        }
    }

    pub(crate) fn repeat_stop_feedback(&mut self) {
        match self.stop_feedback {
            StopFeedbackState::Idle => self.push_pending_stop_feedback(
                "Stop already requested",
                "Waiting for the active run to reach a cancellation boundary.",
            ),
            StopFeedbackState::Pending(index) => self.replace_or_append_stop_feedback(
                index,
                "Stop already requested",
                "Waiting for the active run to reach a cancellation boundary.",
            ),
            StopFeedbackState::Completed => {}
        }
    }

    pub(crate) fn complete_stop_feedback(&mut self) -> bool {
        match self.stop_feedback {
            StopFeedbackState::Idle if !self.is_interrupting() => false,
            StopFeedbackState::Idle => {
                self.push_timeline_item(run_stopped_item());
                self.stop_feedback = StopFeedbackState::Completed;
                true
            }
            StopFeedbackState::Pending(index) => {
                if index.saturating_add(1) == self.timeline.len() {
                    self.replace_timeline_item(index, run_stopped_item());
                } else {
                    self.replace_timeline_item(index, stop_requested_item());
                    self.push_timeline_item(run_stopped_item());
                }
                self.stop_feedback = StopFeedbackState::Completed;
                true
            }
            StopFeedbackState::Completed => true,
        }
    }

    fn push_pending_stop_feedback(&mut self, title: &str, body: &str) {
        let index = self.timeline.len();
        self.push_timeline_item(TimelineItem::LocalCommand {
            title: title.to_owned(),
            body: body.to_owned(),
        });
        self.stop_feedback = StopFeedbackState::Pending(index);
    }

    fn replace_or_append_stop_feedback(&mut self, index: usize, title: &str, body: &str) {
        if index.saturating_add(1) == self.timeline.len() {
            self.replace_timeline_item(
                index,
                TimelineItem::LocalCommand {
                    title: title.to_owned(),
                    body: body.to_owned(),
                },
            );
            return;
        }

        self.replace_timeline_item(index, stop_requested_item());
        self.push_pending_stop_feedback(title, body);
    }

    pub(crate) fn timeline_scroll_offset(&self) -> usize {
        self.timeline_scroll_offset
    }

    pub(crate) fn timeline_review_user_index(&self) -> Option<usize> {
        self.timeline_review_user_index
    }

    pub(crate) fn is_timeline_reviewing(&self) -> bool {
        self.timeline_review_user_index.is_some()
    }

    pub(crate) fn exit_timeline_review(&mut self) {
        self.follow_latest();
    }

    pub(crate) fn follow_latest(&mut self) {
        self.timeline_scroll_offset = 0;
        self.timeline_review_user_index = None;
        self.timeline_anchor = None;
        self.timeline_has_updates = false;
        self.pending_empty_input_quit = false;
    }

    #[cfg(test)]
    pub(crate) fn scroll_timeline_up(&mut self) {
        self.scroll_timeline_up_by(1);
    }

    pub(crate) fn scroll_timeline_up_by(&mut self, lines: usize) {
        self.pending_empty_input_quit = false;
        self.timeline_review_user_index = None;
        self.timeline_anchor = None;
        self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_add(lines);
    }

    pub(crate) fn scroll_timeline_down_by(&mut self, lines: usize) {
        self.pending_empty_input_quit = false;
        self.timeline_review_user_index = None;
        self.timeline_anchor = None;
        self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_sub(lines);
        if self.timeline_scroll_offset == 0 {
            self.follow_latest();
        }
    }

    pub(crate) fn jump_to_previous_user_input(&mut self) {
        let before = self
            .timeline_review_user_index
            .unwrap_or(self.timeline.len());
        if let Some(index) = self.timeline[..before]
            .iter()
            .rposition(|item| matches!(item, TimelineItem::User { .. }))
        {
            self.pending_empty_input_quit = false;
            self.timeline_review_user_index = Some(index);
            self.timeline_anchor = None;
        }
    }

    pub(crate) fn queue_preview(&self) -> &QueuePreviewState {
        &self.queue_preview
    }

    pub(crate) fn has_queue_preview_items(&self) -> bool {
        !self.queue_preview.is_empty()
    }

    pub(crate) fn update_queue_preview(&mut self, preview: QueuePreview) {
        self.queue_preview = QueuePreviewState::from_preview(preview);
    }

    pub(crate) fn set_run_state(&mut self, state: InteractiveRunState) {
        self.set_run_state_at(state, Instant::now());
    }

    pub(crate) fn project_local_run_start(&mut self) {
        if self.run_state == InteractiveRunState::WaitingForInput {
            self.pending_local_run_start = true;
            self.set_run_state(InteractiveRunState::RunningModel);
        }
    }

    pub(crate) fn confirm_local_run_start(&mut self) {
        self.pending_local_run_start = false;
    }

    pub(crate) fn apply_runtime_run_state(&mut self, state: InteractiveRunState) {
        // The producer's initial or previous Waiting event can race a locally submitted input.
        if state == InteractiveRunState::WaitingForInput && self.pending_local_run_start {
            return;
        }
        if state == InteractiveRunState::WaitingForInput && self.is_interrupting() {
            self.complete_stop_feedback();
        }
        self.pending_local_run_start = false;
        self.set_run_state(state);
    }

    pub(crate) fn set_run_state_at(&mut self, state: InteractiveRunState, now: Instant) {
        let was_active = is_active_run_state(self.run_state);
        let is_active = is_active_run_state(state);
        if is_active && !was_active {
            self.stop_feedback = StopFeedbackState::Idle;
            self.active_run_started_at = Some(now);
        } else if !is_active {
            if was_active && let Some(started_at) = self.active_run_started_at.take() {
                self.last_completed_run_elapsed = Some(now.saturating_duration_since(started_at));
            } else {
                self.active_run_started_at = None;
            }
        }
        self.run_state = state;
    }

    pub(crate) fn set_usage(&mut self, usage: SessionUsage) {
        self.usage = Some(usage);
    }

    pub(crate) fn set_reasoning_effort_label(&mut self, label: Option<String>) {
        self.reasoning_effort_label = label;
    }

    pub(crate) fn set_model_label(&mut self, label: String) {
        self.model_label = label;
    }

    pub(crate) fn is_active_run(&self) -> bool {
        is_active_run_state(self.run_state)
    }

    pub(crate) fn can_interrupt_run(&self) -> bool {
        matches!(
            self.run_state,
            InteractiveRunState::RunningModel | InteractiveRunState::RunningTool
        )
    }

    pub(crate) fn is_interrupting(&self) -> bool {
        self.run_state == InteractiveRunState::Interrupting
    }

    pub(crate) fn cancel_input_or_mark_quit(&mut self) -> bool {
        if !self.input.text().is_empty() {
            self.input.replace_text(String::new());
            self.completion_menu = None;
            self.pending_empty_input_quit = true;
            return false;
        }

        if self.pending_empty_input_quit {
            self.pending_empty_input_quit = false;
            return true;
        }

        self.pending_empty_input_quit = true;
        false
    }

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
        let usage = format_session_usage_full(self.usage.as_ref());
        let model = self.model_status_label();
        [self.workspace_root.display().to_string(), model, usage]
    }

    pub(crate) fn header_status_parts(&self, width: u16) -> [String; 3] {
        let model = self.model_status_label();
        format_header_status_parts(&self.workspace_root, &model, self.usage.as_ref(), width)
    }

    pub(crate) fn interaction_status_text(&self) -> String {
        self.interaction_status_text_at(Instant::now())
    }

    pub(crate) fn interaction_status_text_at(&self, now: Instant) -> String {
        match self.run_state {
            InteractiveRunState::WaitingForInput => self.ready_status_text(),
            InteractiveRunState::RunningModel => self.active_status_text("Running model", now),
            InteractiveRunState::RunningTool => self.active_status_text("Running tool", now),
            InteractiveRunState::Interrupting => self.active_status_text("Interrupting", now),
            InteractiveRunState::Closed => "Closed".to_owned(),
        }
    }

    fn ready_status_text(&self) -> String {
        self.last_completed_run_elapsed
            .map(|elapsed| format!("Ready  last run {}", format_elapsed(elapsed)))
            .unwrap_or_else(|| "Ready".to_owned())
    }

    fn active_status_text(&self, label: &str, now: Instant) -> String {
        let elapsed = self
            .active_run_started_at
            .map(|started_at| now.saturating_duration_since(started_at))
            .unwrap_or_default();
        format!(
            "{} {} ({})",
            merry_motion(elapsed),
            label,
            format_elapsed(elapsed)
        )
    }

    fn model_status_label(&self) -> String {
        self.reasoning_effort_label
            .as_deref()
            .filter(|label| !label.is_empty())
            .map(|label| format!("{} {}", self.model_label, label))
            .unwrap_or_else(|| self.model_label.clone())
    }
}

fn is_active_run_state(state: InteractiveRunState) -> bool {
    matches!(
        state,
        InteractiveRunState::RunningModel
            | InteractiveRunState::RunningTool
            | InteractiveRunState::Interrupting
    )
}

fn stop_requested_item() -> TimelineItem {
    TimelineItem::LocalCommand {
        title: "Stop requested".to_owned(),
        body: "The interrupt request remains active.".to_owned(),
    }
}

fn run_stopped_item() -> TimelineItem {
    TimelineItem::LocalCommand {
        title: "Run stopped".to_owned(),
        body: "The active run reached a cancellation boundary.".to_owned(),
    }
}

fn compact_title(text: &str) -> String {
    const MAX_CHARS: usize = 60;
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.chars().count() <= MAX_CHARS {
        return title;
    }
    title
        .chars()
        .take(MAX_CHARS.saturating_sub(1))
        .collect::<String>()
        + "..."
}

fn merry_motion(elapsed: Duration) -> &'static str {
    const FRAMES: [&str; 4] = ["[M··]", "[·M·]", "[··M]", "[·M·]"];
    const FRAME_MS: u128 = 100;
    let frame = (elapsed.as_millis() / FRAME_MS) as usize % FRAMES.len();
    FRAMES[frame]
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
