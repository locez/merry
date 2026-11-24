use crate::tui::{
    completion::{CompletionMenu, CompletionSources},
    input::{InputHistory, TextInput},
    keymap::Keymap,
    links::HoveredLink,
    output_rate::OutputRate,
    plan_view::PlanUiState,
    preferences::{TuiPreferences, TuiSettingsDefaults},
    text_interaction::TextSelection,
    theme::TuiTheme,
    todo_dock::TodoDockState,
};
use clipboard::ClipboardFeedback;
use merry_core::{InteractiveRunState, RuntimeEvent, SessionUsage};
use overlays::OverlayState;
pub(crate) use pending_queue::PendingEditCommit;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
#[cfg(test)]
pub(crate) use timeline::TIMELINE_REVIEW_IDLE_TIMEOUT;
pub(crate) use timeline::TimelineAnchor;
pub(crate) use views::{
    CommandFailure, CommandView, LiveCommandOutput, MAX_EXPANDED_PATCH_LINES, PatchChangeView,
    PatchLineKind, PatchLineView, PatchOperationView, PendingQueueEditor, QueuePreviewState,
    TimelineItem, ToolOutputPreview,
};

mod archive_paging;
mod comments;
pub(crate) use comments::CommentHighlight;
mod history_search;
mod input;
mod overlays;
mod pending_queue;
mod status;
mod subagents;
pub(crate) use subagents::SubagentUpdate;

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
    configured_keymap: Keymap,
    keymap: Keymap,
    configured_theme: TuiTheme,
    theme: TuiTheme,
    input: TextInput,
    viewport_size: ratatui::layout::Size,
    /// Inner composer width from the last layout. Zero means keys use logical lines.
    composer_columns: usize,
    composer_mouse_selecting: bool,
    completion_sources: CompletionSources,
    completion_menu: Option<CompletionMenu>,
    input_history: InputHistory,
    history_search: Option<crate::tui::input::HistorySearch>,
    queue_preview: QueuePreviewState,
    pending_editor: Option<PendingQueueEditor>,
    timeline: Vec<TimelineItem>,
    timeline_versions: Vec<u64>,
    next_timeline_version: u64,
    timeline_cache: crate::tui::render::timeline_cache::TimelineRenderCache,
    show_successful_command_output: bool,
    mouse_scroll_lines: usize,
    animations: bool,
    max_reading_width: u16,
    text_sizing_active: bool,
    brand_mark: crate::config::BrandMarkMode,
    header_items: Vec<crate::config::HeaderItemKind>,
    subagents: Vec<crate::tui::subagent_view::SubagentEntry>,
    task_dock: crate::tui::task_dock::TaskDockState,
    todo_dock: TodoDockState,
    command_details_generation: u64,
    timeline_scroll_offset: usize,
    timeline_review_user_index: Option<usize>,
    timeline_anchor: Option<TimelineAnchor>,
    timeline_has_updates: bool,
    timeline_review_last_activity: Option<tokio::time::Instant>,
    timeline_last_scroll: Option<usize>,
    timeline_scroll_anim: Option<timeline::TimelineScrollAnimation>,
    pending_turn_scroll_animation: bool,
    unloaded_archive_segments: Vec<u32>,
    loading_archive_segment: Option<u32>,
    text_selection: Option<TextSelection>,
    hovered_link: Option<HoveredLink>,
    clipboard_feedback: Option<ClipboardFeedback>,
    pending_local_echoes: Vec<String>,
    pending_local_run_start: bool,
    stop_feedback: StopFeedbackState,
    run_state: InteractiveRunState,
    active_run_started_at: Option<Instant>,
    last_completed_run_elapsed: Option<Duration>,
    pending_empty_input_quit: bool,
    usage: Option<SessionUsage>,
    output_rate: OutputRate,
    overlays: OverlayState,
    preferences: TuiPreferences,
    settings_defaults: TuiSettingsDefaults,
    plan: PlanUiState,
    comments: comments::CommentSession,
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
            configured_keymap: keymap.clone(),
            keymap,
            configured_theme: theme.clone(),
            theme,
            input: TextInput::default(),
            viewport_size: ratatui::layout::Size::new(100, 30),
            composer_columns: 0,
            composer_mouse_selecting: false,
            completion_sources: CompletionSources::new(workspace_root.clone(), Vec::new()),
            completion_menu: None,
            input_history: InputHistory::default(),
            history_search: None,
            queue_preview: QueuePreviewState::default(),
            pending_editor: None,
            timeline: Vec::new(),
            timeline_versions: Vec::new(),
            next_timeline_version: 1,
            timeline_cache: crate::tui::render::timeline_cache::TimelineRenderCache::default(),
            show_successful_command_output: true,
            mouse_scroll_lines: crate::config::DEFAULT_MOUSE_SCROLL_LINES,
            animations: true,
            max_reading_width: crate::config::DEFAULT_MAX_READING_WIDTH,
            text_sizing_active: false,
            brand_mark: crate::config::BrandMarkMode::Emoji,
            header_items: crate::config::HeaderItemKind::DEFAULT_ITEMS.to_vec(),
            subagents: Vec::new(),
            task_dock: crate::tui::task_dock::TaskDockState::default(),
            todo_dock: TodoDockState::default(),
            command_details_generation: 0,
            timeline_scroll_offset: 0,
            timeline_review_user_index: None,
            timeline_anchor: None,
            timeline_has_updates: false,
            timeline_review_last_activity: None,
            timeline_last_scroll: None,
            timeline_scroll_anim: None,
            pending_turn_scroll_animation: false,
            unloaded_archive_segments: Vec::new(),
            loading_archive_segment: None,
            text_selection: None,
            hovered_link: None,
            clipboard_feedback: None,
            pending_local_echoes: Vec::new(),
            pending_local_run_start: false,
            stop_feedback: StopFeedbackState::Idle,
            run_state: InteractiveRunState::WaitingForInput,
            active_run_started_at: None,
            last_completed_run_elapsed: None,
            pending_empty_input_quit: false,
            usage: None,
            output_rate: OutputRate::default(),
            overlays: OverlayState::default(),
            preferences: TuiPreferences::default(),
            settings_defaults: TuiSettingsDefaults::default(),
            plan: PlanUiState::default(),
            comments: comments::CommentSession::default(),
        }
    }

    /// Sets the display-only success-output policy without changing runtime artifacts.
    pub(crate) fn with_successful_command_output(mut self, show_output: bool) -> Self {
        self.show_successful_command_output = show_output;
        self
    }

    pub(crate) fn task_dock(&self) -> &crate::tui::task_dock::TaskDockState {
        &self.task_dock
    }

    pub(crate) fn todo_dock(&self) -> &TodoDockState {
        &self.todo_dock
    }

    pub(crate) fn todo_dock_mut(&mut self) -> &mut TodoDockState {
        &mut self.todo_dock
    }

    pub(crate) fn task_dock_mut(&mut self) -> &mut crate::tui::task_dock::TaskDockState {
        &mut self.task_dock
    }

    pub(crate) fn sync_background_processes(
        &mut self,
        processes: Vec<merry_runtime::BackgroundProcessSummary>,
    ) -> bool {
        self.task_dock.set_background_processes(processes)
    }

    pub(crate) fn has_running_background_processes(&self) -> bool {
        self.task_dock.has_running_background_processes()
    }

    /// Sets how many lines each coalesced mouse-wheel notch moves a scrollable pane.
    pub(crate) fn with_mouse_scroll_lines(mut self, lines: usize) -> Self {
        self.mouse_scroll_lines = lines.max(1);
        self
    }

    pub(crate) fn mouse_scroll_lines(&self) -> usize {
        self.mouse_scroll_lines
    }

    pub(crate) fn with_animations(mut self, animations: bool) -> Self {
        self.animations = animations;
        self
    }

    pub(crate) fn animations_enabled(&self) -> bool {
        self.animations
    }

    pub(crate) fn with_max_reading_width(mut self, width: u16) -> Self {
        self.max_reading_width = width;
        self
    }

    pub(crate) fn max_reading_width(&self) -> u16 {
        self.max_reading_width
    }

    pub(crate) fn with_text_sizing(mut self, mode: crate::config::TextSizingMode) -> Self {
        self.text_sizing_active = crate::tui::text_sizing::is_text_sizing_active(mode);
        self
    }

    pub(crate) fn with_brand_mark(mut self, mode: crate::config::BrandMarkMode) -> Self {
        self.brand_mark = mode;
        self
    }

    pub(crate) fn primary_brand_badge(&self) -> &'static str {
        crate::tui::glyphs::primary_badge(self.brand_mark)
    }

    #[allow(dead_code)]
    pub(crate) fn text_sizing_active(&self) -> bool {
        self.text_sizing_active
    }

    pub(crate) fn with_header_items(mut self, items: Vec<crate::config::HeaderItemKind>) -> Self {
        self.header_items = items;
        self
    }

    pub(crate) fn header_items(&self) -> &[crate::config::HeaderItemKind] {
        &self.header_items
    }

    pub(crate) fn usage(&self) -> Option<&SessionUsage> {
        self.usage.as_ref()
    }

    pub(crate) fn output_rate_label(&self) -> String {
        self.output_rate.label()
    }

    pub(crate) fn workspace_root(&self) -> &std::path::Path {
        &self.workspace_root
    }

    pub(crate) fn model_display_label(&self) -> String {
        self.model_status_label()
    }

    pub(crate) fn timeline_item_version(&self, index: usize) -> u64 {
        self.timeline_versions.get(index).copied().unwrap_or(0)
    }

    pub(crate) fn timeline_cache(
        &self,
    ) -> &crate::tui::render::timeline_cache::TimelineRenderCache {
        &self.timeline_cache
    }

    #[cfg(test)]
    pub(crate) fn layout_cache_misses(&self) -> u64 {
        self.timeline_cache.misses()
    }

    #[cfg(test)]
    pub(crate) fn materialized_layout_count(&self) -> usize {
        self.timeline_cache.materialized_count()
    }

    #[cfg(test)]
    pub(crate) fn cached_width_profile_count(&self) -> usize {
        self.timeline_cache.width_profile_count()
    }

    fn alloc_timeline_version(&mut self) -> u64 {
        let version = self.next_timeline_version;
        self.next_timeline_version = self.next_timeline_version.saturating_add(1);
        version
    }

    fn bump_timeline_version(&mut self, index: usize) {
        let version = self.alloc_timeline_version();
        if let Some(slot) = self.timeline_versions.get_mut(index) {
            *slot = version;
        }
    }

    /// Whether successful process commands and file reads show their bounded output preview.
    pub(crate) fn show_successful_command_output(&self) -> bool {
        self.show_successful_command_output
    }

    pub(crate) fn plan(&self) -> &PlanUiState {
        &self.plan
    }

    pub(crate) fn plan_mut(&mut self) -> &mut PlanUiState {
        &mut self.plan
    }

    pub(crate) fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    pub(crate) fn theme(&self) -> &TuiTheme {
        &self.theme
    }

    pub(super) fn sync_active_ui_preferences(&mut self) {
        let base = match self.preferences.theme_preset {
            Some(preset) => TuiTheme::from_preset(preset),
            None => self.configured_theme.clone(),
        };
        let transparent = self
            .preferences
            .transparent_background
            .unwrap_or(self.settings_defaults.transparent_background);
        self.theme = base.with_transparent_background(transparent);
        self.keymap = self
            .configured_keymap
            .with_overrides(&self.preferences.keymap);
        self.animations = self
            .preferences
            .animations
            .unwrap_or(self.settings_defaults.animations);
        self.brand_mark = self
            .preferences
            .brand_mark
            .unwrap_or(self.settings_defaults.brand_mark);
        self.max_reading_width = self
            .preferences
            .max_reading_width
            .unwrap_or(self.settings_defaults.max_reading_width);
        let text_sizing = self
            .preferences
            .text_sizing
            .unwrap_or(self.settings_defaults.text_sizing);
        self.text_sizing_active = crate::tui::text_sizing::is_text_sizing_active(text_sizing);
        self.header_items = self
            .preferences
            .header_items
            .clone()
            .unwrap_or_else(|| self.settings_defaults.header_items.clone());
        self.mouse_scroll_lines = self
            .preferences
            .mouse_scroll_lines
            .unwrap_or(self.settings_defaults.mouse_scroll_lines)
            .max(1);
        self.show_successful_command_output = self
            .preferences
            .show_successful_command_output
            .unwrap_or(self.settings_defaults.show_successful_command_output);
    }

    pub(crate) fn replace_settings_defaults(&mut self, defaults: TuiSettingsDefaults) {
        self.settings_defaults = defaults;
        self.sync_active_ui_preferences();
    }

    pub(crate) fn current_provider_alias(&self) -> Option<&str> {
        self.preferences
            .provider
            .as_deref()
            .or(self.settings_defaults.provider.as_deref())
    }

    pub(crate) fn replace_preferences(&mut self, preferences: TuiPreferences) {
        self.preferences = preferences;
        self.sync_active_ui_preferences();
    }

    pub(crate) fn timeline(&self) -> &[TimelineItem] {
        &self.timeline
    }

    pub(crate) fn into_timeline(self) -> Vec<TimelineItem> {
        self.timeline
    }

    pub(crate) fn first_user_input_topic(&self) -> Option<String> {
        self.timeline.iter().find_map(|item| {
            let TimelineItem::User { text, .. } = item else {
                return None;
            };
            let topic = super::session_list::compact_session_text(text);
            (!topic.is_empty()).then_some(topic)
        })
    }

    pub(crate) fn push_timeline_item(&mut self, item: TimelineItem) {
        self.note_timeline_update();
        let version = self.alloc_timeline_version();
        self.timeline.push(item);
        self.timeline_versions.push(version);
    }

    pub(crate) fn append_assistant_delta(&mut self, index: Option<usize>, delta: &str) -> usize {
        self.note_timeline_update();
        if let Some(index) = index
            && let Some(TimelineItem::Assistant { text }) = self.timeline.get_mut(index)
        {
            text.push_str(delta);
            self.bump_timeline_version(index);
            index
        } else {
            let version = self.alloc_timeline_version();
            self.timeline.push(TimelineItem::Assistant {
                text: delta.to_owned(),
            });
            self.timeline_versions.push(version);
            self.timeline.len().saturating_sub(1)
        }
    }

    pub(crate) fn push_user_timeline_item(&mut self, text: String) {
        if self.animations && !self.is_timeline_detached() && self.timeline_last_scroll.is_some() {
            self.pending_turn_scroll_animation = true;
        }
        self.push_timeline_item(TimelineItem::User { text });
    }

    pub(crate) fn push_local_user_echo(&mut self, text: String) {
        self.pending_local_echoes.push(text.clone());
        self.push_user_timeline_item(text);
    }

    pub(crate) fn confirm_or_push_user_input(&mut self, text: String) {
        if let Some(index) = self
            .pending_local_echoes
            .iter()
            .position(|echo| echo == &text)
        {
            let _ = self.pending_local_echoes.remove(index);
            return;
        }

        self.push_user_timeline_item(text);
    }

    pub(crate) fn replace_timeline_item(&mut self, index: usize, item: TimelineItem) {
        self.note_timeline_update();
        if let Some(slot) = self.timeline.get_mut(index) {
            *slot = item;
            self.bump_timeline_version(index);
        }
    }

    /// Removes an uncommitted streaming assistant item when its attempt is
    /// discarded for a model retry, so the retried attempt streams cleanly
    /// without duplicating partial commentary from the failed attempt.
    pub(crate) fn discard_streaming_assistant(&mut self, index: usize) {
        let Some(TimelineItem::Assistant { .. }) = self.timeline.get(index) else {
            return;
        };
        self.note_timeline_update();
        self.timeline.remove(index);
        self.timeline_versions.remove(index);
        if let Some(anchor) = &mut self.timeline_anchor {
            if anchor.item_index == index {
                self.timeline_anchor = None;
            } else if anchor.item_index > index {
                anchor.item_index -= 1;
            }
        }
        if let Some(review_index) = &mut self.timeline_review_user_index {
            if *review_index == index {
                self.timeline_review_user_index = None;
            } else if *review_index > index {
                *review_index -= 1;
            }
        }
        if !self.is_timeline_detached() {
            self.timeline_review_last_activity = None;
        }
    }

    /// Appends reasoning text to the open thinking block, or starts one.
    pub(crate) fn append_reasoning_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.note_timeline_update();
        if let Some(TimelineItem::Thinking {
            text,
            elapsed: None,
            ..
        }) = self.timeline.last_mut()
        {
            text.push_str(delta);
            let last_index = self.timeline.len().saturating_sub(1);
            self.bump_timeline_version(last_index);
            return;
        }
        let version = self.alloc_timeline_version();
        self.timeline.push(TimelineItem::Thinking {
            text: delta.to_owned(),
            started_at: tokio::time::Instant::now(),
            elapsed: None,
            expanded: false,
        });
        self.timeline_versions.push(version);
    }

    /// Closes every open thinking block so timers and animations stop cleanly
    /// even when another item (such as stop feedback) was appended after it.
    pub(crate) fn finish_reasoning(&mut self) {
        let mut closed_any = false;
        for index in 0..self.timeline.len() {
            if let Some(TimelineItem::Thinking {
                started_at,
                elapsed,
                ..
            }) = self.timeline.get_mut(index)
                && elapsed.is_none()
            {
                *elapsed = Some(started_at.elapsed());
                self.bump_timeline_version(index);
                closed_any = true;
            }
        }
        if closed_any {
            self.note_timeline_update();
        }
    }

    /// Opens or closes a finished thinking block.
    pub(crate) fn toggle_thinking_expanded(&mut self, index: usize) {
        if let Some(TimelineItem::Thinking {
            expanded, elapsed, ..
        }) = self.timeline.get_mut(index)
            && elapsed.is_some()
        {
            *expanded = !*expanded;
            self.bump_timeline_version(index);
        }
    }

    /// Opens or closes a command's output fold (both running live output and finished previews).
    pub(crate) fn toggle_command_expanded(&mut self, index: usize) {
        match self.timeline.get_mut(index) {
            Some(TimelineItem::Command {
                view: CommandView::Finished { expanded, .. },
            }) => {
                *expanded = !*expanded;
                self.bump_timeline_version(index);
            }
            Some(TimelineItem::Command {
                view: CommandView::Running { expanded, .. },
            })
            | Some(TimelineItem::ToolRunning { expanded, .. })
            | Some(TimelineItem::Read { expanded, .. })
            | Some(TimelineItem::Expanded { expanded, .. })
            | Some(TimelineItem::Patch { expanded, .. }) => {
                *expanded = !*expanded;
                self.bump_timeline_version(index);
            }
            _ => {}
        }
    }

    /// Appends a live stdout/stderr chunk to an active running command's rolling preview.
    pub(crate) fn append_running_command_output(&mut self, index: usize, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(TimelineItem::Command {
            view: CommandView::Running { live_preview, .. },
        }) = self.timeline.get_mut(index)
        {
            live_preview.append(delta);
            self.bump_timeline_version(index);
        }
    }

    pub(crate) fn begin_stop_feedback(&mut self) {
        self.set_run_state(InteractiveRunState::Interrupting);
        if self.stop_feedback == StopFeedbackState::Idle {
            self.push_pending_stop_feedback("⊘ Stopping…", "");
        }
    }

    pub(crate) fn repeat_stop_feedback(&mut self) {
        match self.stop_feedback {
            StopFeedbackState::Idle => {
                self.push_pending_stop_feedback("⊘ Stopping…", "waiting for current step")
            }
            StopFeedbackState::Pending(index) => self.replace_or_append_stop_feedback(
                index,
                "⊘ Stopping…",
                "waiting for current step",
            ),
            StopFeedbackState::Completed => {}
        }
    }

    pub(crate) fn complete_stop_feedback(&mut self) -> bool {
        self.finish_reasoning();
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

    fn push_pending_stop_feedback(&mut self, title: &str, detail: &str) {
        let index = self.timeline.len();
        self.push_timeline_item(TimelineItem::Muted {
            title: title.to_owned(),
            detail: detail.to_owned(),
        });
        self.stop_feedback = StopFeedbackState::Pending(index);
    }

    fn replace_or_append_stop_feedback(&mut self, index: usize, title: &str, detail: &str) {
        if index.saturating_add(1) == self.timeline.len() {
            self.replace_timeline_item(
                index,
                TimelineItem::Muted {
                    title: title.to_owned(),
                    detail: detail.to_owned(),
                },
            );
            return;
        }

        self.replace_timeline_item(index, stop_requested_item());
        self.push_pending_stop_feedback(title, detail);
    }

    pub(crate) fn timeline_scroll_offset(&self) -> usize {
        self.timeline_scroll_offset
    }

    pub(crate) fn timeline_review_user_index(&self) -> Option<usize> {
        self.timeline_review_user_index
    }

    pub(crate) fn follow_latest(&mut self) {
        self.timeline_scroll_offset = 0;
        self.timeline_review_user_index = None;
        self.timeline_anchor = None;
        self.timeline_has_updates = false;
        self.timeline_review_last_activity = None;
        self.timeline_last_scroll = None;
        self.clear_timeline_scroll_animation();
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
        self.clear_timeline_scroll_animation();
        self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_add(lines);
        self.note_timeline_review_activity();
    }

    pub(crate) fn scroll_timeline_down_by(&mut self, lines: usize) {
        self.pending_empty_input_quit = false;
        self.timeline_review_user_index = None;
        self.timeline_anchor = None;
        self.clear_timeline_scroll_animation();
        self.timeline_scroll_offset = self.timeline_scroll_offset.saturating_sub(lines);
        if self.timeline_scroll_offset == 0 {
            self.follow_latest();
        } else {
            self.note_timeline_review_activity();
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
            self.clear_timeline_scroll_animation();
            self.note_timeline_review_activity();
        }
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
            if self.is_timeline_detached() {
                self.timeline_review_last_activity = Some(tokio::time::Instant::now());
            }
        } else if !is_active {
            self.finish_reasoning();
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

    pub(crate) fn observe_output_rate(&mut self, event: &RuntimeEvent) {
        self.output_rate.observe(event);
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
    TimelineItem::Muted {
        title: "⊘ Stop requested".to_owned(),
        detail: String::new(),
    }
}

fn run_stopped_item() -> TimelineItem {
    TimelineItem::Muted {
        title: "⊘ Stopped".to_owned(),
        detail: String::new(),
    }
}
