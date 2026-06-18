use super::{
    completion::{CompletionMenu, CompletionSources},
    input::{InputHistory, TextInput},
    keymap::Keymap,
    theme::TuiTheme,
};
use merry_core::{InteractiveRunState, QueuedInputLane, QueuedInputView, SessionUsage};
use merry_runtime::SkillMetadata;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct QueuePreview {
    pub(crate) next: Vec<QueuedInputView>,
    pub(crate) suspended: Vec<QueuedInputView>,
    pub(crate) backlog: Vec<QueuedInputView>,
}

#[allow(dead_code)]
impl QueuePreview {
    pub(crate) fn empty() -> Self {
        Self {
            next: Vec::new(),
            suspended: Vec::new(),
            backlog: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct QueuePreviewItem {
    pub(crate) text: String,
}

#[allow(dead_code)]
impl QueuePreviewItem {
    pub(crate) fn display_text(&self, max_chars: usize) -> String {
        if max_chars <= 3 {
            return ".".repeat(max_chars);
        }
        if self.text.chars().count() <= max_chars {
            return self.text.clone();
        }
        let prefix = self.text.chars().take(max_chars - 3).collect::<String>();
        format!("{prefix}...")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct QueuePreviewState {
    pub(crate) next: Vec<QueuePreviewItem>,
    pub(crate) suspended: Vec<QueuePreviewItem>,
    pub(crate) backlog: Vec<QueuePreviewItem>,
}

impl QueuePreviewState {
    fn from_preview(preview: QueuePreview) -> Self {
        fn convert(items: Vec<QueuedInputView>) -> Vec<QueuePreviewItem> {
            items
                .into_iter()
                .map(|item| QueuePreviewItem { text: item.text })
                .collect()
        }

        Self {
            next: convert(preview.next),
            suspended: convert(preview.suspended),
            backlog: convert(preview.backlog),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.next.is_empty() && self.suspended.is_empty() && self.backlog.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum PatchLineKind {
    Context,
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct PatchLineView {
    pub(crate) kind: PatchLineKind,
    pub(crate) old_line: Option<usize>,
    pub(crate) new_line: Option<usize>,
    pub(crate) text: String,
}

#[allow(dead_code)]
impl PatchLineView {
    pub(crate) fn context(text: impl Into<String>, line: Option<usize>) -> Self {
        Self {
            kind: PatchLineKind::Context,
            old_line: line,
            new_line: line,
            text: text.into(),
        }
    }

    pub(crate) fn add(text: impl Into<String>, new_line: Option<usize>) -> Self {
        Self {
            kind: PatchLineKind::Add,
            old_line: None,
            new_line,
            text: text.into(),
        }
    }

    pub(crate) fn remove(text: impl Into<String>, old_line: Option<usize>) -> Self {
        Self {
            kind: PatchLineKind::Remove,
            old_line,
            new_line: None,
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct PatchChangeView {
    pub(crate) path: String,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    pub(crate) hunks: usize,
    pub(crate) bytes_before: Option<usize>,
    pub(crate) bytes_after: Option<usize>,
    pub(crate) lines: Vec<PatchLineView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TimelineItem {
    User { text: String, lane: QueuedInputLane },
    Assistant { text: String },
    Muted { title: String, detail: String },
    Expanded { title: String, body: String },
    Diagnostic { title: String, body: String },
    Patch { changes: Vec<PatchChangeView> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
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
    timeline_scroll_offset: usize,
    timeline_review_user_index: Option<usize>,
    pending_local_echoes: Vec<PendingLocalEcho>,
    run_state: InteractiveRunState,
    active_run_started_at: Option<Instant>,
    last_completed_run_elapsed: Option<Duration>,
    pending_empty_input_quit: bool,
    usage: Option<SessionUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingLocalEcho {
    text: String,
    lane: QueuedInputLane,
}

#[allow(dead_code)]
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
            timeline_scroll_offset: 0,
            timeline_review_user_index: None,
            pending_local_echoes: Vec::new(),
            run_state: InteractiveRunState::WaitingForInput,
            active_run_started_at: None,
            last_completed_run_elapsed: None,
            pending_empty_input_quit: false,
            usage: None,
        }
    }

    pub(crate) fn input_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    pub(crate) fn set_completion_skills(&mut self, skills: Vec<SkillMetadata>) {
        self.completion_sources = CompletionSources::new(self.workspace_root.clone(), skills);
        self.refresh_completion_menu();
    }

    pub(crate) fn input_text(&self) -> &str {
        self.input.text()
    }

    pub(crate) fn input_viewport(&self, max_width: usize) -> super::input::TextInputViewport {
        self.input.viewport(max_width)
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

    pub(crate) fn take_input_for_submit(&mut self) -> Option<String> {
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
        let value = self.input.take_trimmed()?;
        self.input_history.record(&value);
        Some(value)
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

    pub(crate) fn handle_input_key(&mut self, key: crossterm::event::KeyEvent) {
        self.pending_empty_input_quit = false;
        self.input.handle_key(key);
        self.refresh_completion_menu();
    }

    pub(crate) fn insert_input_str(&mut self, text: &str) {
        self.pending_empty_input_quit = false;
        self.input.insert_str(text);
        self.refresh_completion_menu();
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
        self.refresh_completion_menu();
        true
    }

    fn refresh_completion_menu(&mut self) {
        self.completion_menu = self.completion_sources.menu_for_input(
            self.input.text(),
            self.input.cursor_byte_index(),
            self.completion_menu.as_ref(),
        );
    }

    pub(crate) fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub(crate) fn theme(&self) -> &TuiTheme {
        &self.theme
    }

    pub(crate) fn timeline(&self) -> &[TimelineItem] {
        &self.timeline
    }

    pub(crate) fn push_timeline_item(&mut self, item: TimelineItem) {
        self.timeline.push(item);
        self.timeline_scroll_offset = 0;
        self.timeline_review_user_index = None;
    }

    pub(crate) fn append_assistant_delta(&mut self, index: Option<usize>, delta: &str) -> usize {
        let index = if let Some(index) = index
            && let Some(TimelineItem::Assistant { text }) = self.timeline.get_mut(index)
        {
            text.push_str(delta);
            index
        } else {
            self.timeline.push(TimelineItem::Assistant {
                text: delta.to_owned(),
            });
            self.timeline.len().saturating_sub(1)
        };
        self.timeline_scroll_offset = 0;
        self.timeline_review_user_index = None;
        index
    }

    pub(crate) fn push_user_timeline_item(&mut self, text: String, lane: QueuedInputLane) {
        self.push_timeline_item(TimelineItem::User { text, lane });
    }

    pub(crate) fn push_local_user_echo(&mut self, text: String, lane: QueuedInputLane) {
        self.pending_local_echoes.push(PendingLocalEcho {
            text: text.clone(),
            lane,
        });
        self.push_user_timeline_item(text, lane);
    }

    pub(crate) fn confirm_or_push_user_input(&mut self, text: String, lane: QueuedInputLane) {
        if let Some(index) = self
            .pending_local_echoes
            .iter()
            .position(|echo| echo.text == text && echo.lane == lane)
        {
            self.pending_local_echoes.remove(index);
            return;
        }

        self.push_user_timeline_item(text, lane);
    }

    pub(crate) fn replace_timeline_item(&mut self, index: usize, item: TimelineItem) {
        if let Some(slot) = self.timeline.get_mut(index) {
            *slot = item;
            self.timeline_scroll_offset = 0;
            self.timeline_review_user_index = None;
        }
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
        self.timeline_review_user_index = None;
        self.timeline_scroll_offset = 0;
    }

    pub(crate) fn scroll_timeline_up(&mut self) {
        self.scroll_timeline_up_by(1);
    }

    pub(crate) fn scroll_timeline_down(&mut self) {
        self.scroll_timeline_down_by(1);
    }

    pub(crate) fn scroll_timeline_up_by(&mut self, lines: usize) {
        self.pending_empty_input_quit = false;
        self.timeline_review_user_index = None;
        self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_add(lines);
    }

    pub(crate) fn scroll_timeline_down_by(&mut self, lines: usize) {
        self.pending_empty_input_quit = false;
        self.timeline_review_user_index = None;
        self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_sub(lines);
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

    pub(crate) fn set_run_state_at(&mut self, state: InteractiveRunState, now: Instant) {
        let was_active = is_active_run_state(self.run_state);
        let is_active = is_active_run_state(state);
        if is_active && !was_active {
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

    pub(crate) fn is_active_run(&self) -> bool {
        is_active_run_state(self.run_state)
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

    pub(crate) fn status_text(&self) -> String {
        let usage = self
            .usage
            .as_ref()
            .map(format_session_usage)
            .unwrap_or_else(|| "usage -".to_owned());
        let model = self.model_status_label();
        format!("{}  {}  {}", self.workspace_root.display(), model, usage)
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

    #[cfg(test)]
    pub(crate) fn set_active_run_started_at_for_test(&mut self, started_at: Instant) {
        self.active_run_started_at = Some(started_at);
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

fn format_session_usage(usage: &SessionUsage) -> String {
    format!(
        "last in {} out {} | total {} tok",
        format_token_count(usage.last.input_tokens()),
        format_token_count(usage.last.output_tokens()),
        format_token_count(usage.total.total_tokens())
    )
}

fn format_token_count(tokens: u64) -> String {
    if tokens < 1_000 {
        return tokens.to_string();
    }

    let whole = tokens / 1_000;
    let decimal = (tokens % 1_000) / 100;
    if decimal == 0 {
        format!("{whole}k")
    } else {
        format!("{whole}.{decimal}k")
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

fn merry_motion(elapsed: Duration) -> String {
    const TRACK_WIDTH: usize = 9;
    const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    const FRAME_MS: u128 = 70;
    const HOLD_FRAMES: usize = SPINNER_FRAMES.len();
    const SPIN_WIDTH: usize = 2;
    const MOVE_FRAMES: usize = TRACK_WIDTH - SPIN_WIDTH;
    const LOOP_FRAMES: usize = HOLD_FRAMES + MOVE_FRAMES + HOLD_FRAMES + MOVE_FRAMES;

    let frame = (elapsed.as_millis() / FRAME_MS) as usize % LOOP_FRAMES;
    if frame < HOLD_FRAMES {
        return format!(
            "[{}M{}]",
            SPINNER_FRAMES[frame],
            ".".repeat(TRACK_WIDTH - SPIN_WIDTH)
        );
    }

    let outbound_start = HOLD_FRAMES;
    let right_spin_start = outbound_start + MOVE_FRAMES;
    if frame < right_spin_start {
        let position = frame - outbound_start + SPIN_WIDTH;
        return moving_merry_marker(position, TRACK_WIDTH);
    }

    let inbound_start = right_spin_start + HOLD_FRAMES;
    if frame < inbound_start {
        let spin = SPINNER_FRAMES[frame - right_spin_start];
        return format!("[{}M{}]", ".".repeat(TRACK_WIDTH - SPIN_WIDTH), spin);
    }

    let position = TRACK_WIDTH - SPIN_WIDTH - (frame - inbound_start);
    moving_merry_marker(position, TRACK_WIDTH)
}

fn moving_merry_marker(position: usize, width: usize) -> String {
    let mut marker = vec!['.'; width];
    marker[position] = 'M';
    format!("[{}]", marker.into_iter().collect::<String>())
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
