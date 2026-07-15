use super::{
    completion::{CompletionMenu, CompletionSources},
    input::{DraftImage, InputHistory, TextInput, TuiSubmission},
    keymap::Keymap,
    overlay::{
        MessageDialogKind, MessageDialogOverlay, Overlay, PlanPaletteContext, SettingsOverlay,
        ShortcutsBack,
    },
    plan::PlanUiState,
    preferences::{TuiPreferences, TuiSettingsDefaults},
    provider_overlay::{
        ModelListItem, ModelPickerOverlay, ProviderFormOverlay, ProviderFormSeed,
        ProviderFormValues, ProviderListItem, ProviderManagerOverlay,
    },
    status::{format_header_status_parts, format_session_usage_full},
    theme::TuiTheme,
};
use merry_core::{InteractiveRunState, QueuedInputLane, QueuedInputView, SessionUsage};
use merry_runtime::SkillMetadata;
use std::time::{Duration, Instant};
use std::{collections::BTreeSet, path::PathBuf};

mod settings;

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
    User {
        text: String,
        lane: QueuedInputLane,
    },
    Assistant {
        text: String,
    },
    Muted {
        title: String,
        detail: String,
    },
    Expanded {
        title: String,
        body: String,
    },
    ExpandedDetail {
        title: String,
        body: String,
        focus_body: String,
    },
    Diagnostic {
        title: String,
        body: String,
    },
    Patch {
        changes: Vec<PatchChangeView>,
    },
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
    focus_scroll_offset: usize,
    timeline_review_user_index: Option<usize>,
    artifact_review_timeline_index: Option<usize>,
    pending_local_echoes: Vec<PendingLocalEcho>,
    run_state: InteractiveRunState,
    active_run_started_at: Option<Instant>,
    last_completed_run_elapsed: Option<Duration>,
    pending_empty_input_quit: bool,
    usage: Option<SessionUsage>,
    overlay: Option<Overlay>,
    dialog_back: Option<Box<Overlay>>,
    provider_overlay_back: Option<ProviderOverlayBack>,
    provider_form_back: Option<ProviderFormOverlay>,
    preferences: TuiPreferences,
    settings_defaults: TuiSettingsDefaults,
    plan: PlanUiState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderOverlayBack {
    CommandPalette,
    Settings(SettingsOverlay),
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
            focus_scroll_offset: 0,
            timeline_review_user_index: None,
            artifact_review_timeline_index: None,
            pending_local_echoes: Vec::new(),
            run_state: InteractiveRunState::WaitingForInput,
            active_run_started_at: None,
            last_completed_run_elapsed: None,
            pending_empty_input_quit: false,
            usage: None,
            overlay: None,
            dialog_back: None,
            provider_overlay_back: None,
            provider_form_back: None,
            preferences: TuiPreferences::default(),
            settings_defaults: TuiSettingsDefaults::default(),
            plan: PlanUiState::default(),
        }
    }

    pub(crate) fn plan(&self) -> &PlanUiState {
        &self.plan
    }

    pub(crate) fn plan_mut(&mut self) -> &mut PlanUiState {
        &mut self.plan
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

    pub(crate) fn take_input_for_submit(&mut self) -> Option<TuiSubmission> {
        self.completion_menu = None;
        self.pending_empty_input_quit = false;
        let submission = match self.input.take_submission() {
            Ok(submission) => submission?,
            Err(error) => {
                self.push_timeline_item(TimelineItem::Diagnostic {
                    title: "user_input".to_owned(),
                    body: error.to_string(),
                });
                return None;
            }
        };
        self.input_history.record(&submission.history_text);
        Some(submission)
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

    pub(crate) fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref()
    }

    pub(crate) fn overlay_mut(&mut self) -> Option<&mut Overlay> {
        self.overlay.as_mut()
    }

    pub(crate) fn insert_overlay_paste(&mut self, text: &str) -> bool {
        let Some(overlay) = self.overlay.as_mut() else {
            return false;
        };
        overlay.insert_paste(text);
        true
    }

    pub(crate) fn open_command_palette(&mut self) {
        self.completion_menu = None;
        self.dialog_back = None;
        self.provider_overlay_back = None;
        self.overlay = Some(self.command_palette_overlay());
    }

    pub(crate) fn open_settings(&mut self) {
        self.dialog_back = None;
        self.provider_overlay_back = None;
        self.overlay = Some(Overlay::settings());
    }

    pub(crate) fn open_provider_manager(&mut self, items: Vec<ProviderListItem>) {
        self.provider_form_back = None;
        match self.overlay.take() {
            Some(Overlay::CommandPalette(_)) => {
                self.provider_overlay_back = Some(ProviderOverlayBack::CommandPalette);
            }
            Some(Overlay::Settings(settings)) => {
                self.provider_overlay_back = Some(ProviderOverlayBack::Settings(settings));
            }
            Some(
                Overlay::ProviderManager(_) | Overlay::ProviderForm(_) | Overlay::ModelPicker(_),
            ) => {}
            Some(
                Overlay::PlanApproval(_)
                | Overlay::PermissionReview(_)
                | Overlay::Dialog(_)
                | Overlay::Shortcuts(_),
            )
            | None => {
                self.provider_overlay_back
                    .get_or_insert(ProviderOverlayBack::CommandPalette);
            }
        }
        let current = self.current_provider_alias().map(str::to_owned);
        self.overlay = Some(Overlay::ProviderManager(ProviderManagerOverlay::new(
            items,
            current.as_deref(),
        )));
    }

    pub(crate) fn open_provider_form(&mut self, alias: String, used_aliases: BTreeSet<String>) {
        self.provider_form_back = None;
        self.overlay = Some(Overlay::ProviderForm(ProviderFormOverlay::new(
            alias,
            used_aliases,
        )));
    }

    pub(crate) fn open_provider_editor(
        &mut self,
        seed: ProviderFormSeed,
        used_aliases: BTreeSet<String>,
    ) {
        self.provider_form_back = None;
        self.overlay = Some(Overlay::ProviderForm(ProviderFormOverlay::edit(
            seed,
            used_aliases,
        )));
    }

    pub(crate) fn open_model_picker(
        &mut self,
        alias: String,
        display_name: String,
        models: Vec<ModelListItem>,
    ) {
        self.provider_form_back = None;
        self.overlay = Some(Overlay::ModelPicker(ModelPickerOverlay::new(
            alias,
            display_name,
            models,
            true,
        )));
    }

    pub(crate) fn open_provider_form_model_picker(
        &mut self,
        alias: String,
        display_name: String,
    ) -> bool {
        let form = match self.overlay.take() {
            Some(Overlay::ProviderForm(form)) => form,
            overlay => {
                self.overlay = overlay;
                return false;
            }
        };
        self.provider_form_back = Some(form);
        self.overlay = Some(Overlay::ModelPicker(ModelPickerOverlay::for_provider_form(
            alias,
            display_name,
            Vec::new(),
        )));
        true
    }

    pub(crate) fn provider_form_discovery_request(
        &self,
    ) -> Option<(Option<String>, ProviderFormValues)> {
        self.provider_form_back
            .as_ref()
            .map(ProviderFormOverlay::discovery_request)
    }

    pub(crate) fn select_provider_form_model(&mut self, model: &str) -> bool {
        let Some(mut form) = self.provider_form_back.take() else {
            return false;
        };
        form.set_model(model);
        self.overlay = Some(Overlay::ProviderForm(form));
        true
    }

    pub(crate) fn update_model_picker(
        &mut self,
        alias: &str,
        result: Result<Vec<ModelListItem>, String>,
    ) {
        match result {
            Ok(models) => {
                if let Some(Overlay::ModelPicker(picker)) = self.overlay.as_mut()
                    && picker.alias() == alias
                {
                    picker.set_models(models);
                }
            }
            Err(error) => {
                if self.overlay.as_ref().is_some_and(
                    |overlay| matches!(overlay, Overlay::ModelPicker(picker) if picker.alias() == alias),
                ) {
                    self.show_error_dialog("Model discovery failed", error);
                }
            }
        }
    }

    pub(crate) fn mark_model_picker_loading(&mut self, alias: &str) {
        if let Some(Overlay::ModelPicker(picker)) = self.overlay.as_mut()
            && picker.alias() == alias
        {
            picker.set_loading();
        }
    }

    pub(crate) fn set_provider_overlay_error(&mut self, error: String) {
        self.show_error_dialog("Provider error", error);
    }

    pub(crate) fn show_info_dialog(&mut self, title: &str, message: String) {
        self.show_dialog(MessageDialogKind::Info, title, message);
    }

    pub(crate) fn open_plan_approval(&mut self) {
        match (self.plan.approval_summary(), self.plan.approval_input()) {
            (Ok(message), Ok(input)) => {
                if !matches!(self.overlay, Some(Overlay::PlanApproval(_))) {
                    self.dialog_back = self.overlay.take().map(Box::new);
                }
                self.overlay = Some(Overlay::plan_approval(message, input));
            }
            (Err(error), _) | (_, Err(error)) => {
                self.show_error_dialog("Plan approval unavailable", error)
            }
        }
    }

    pub(crate) fn open_permission_review(&mut self, approval_id: String, body: String) {
        self.completion_menu = None;
        self.dialog_back = None;
        self.provider_overlay_back = None;
        self.overlay = Some(Overlay::permission_review(approval_id, body));
    }

    pub(crate) fn plan_approval_input(&self) -> Option<merry_runtime::PlanApprovalInput> {
        match self.overlay.as_ref() {
            Some(Overlay::PlanApproval(approval)) => Some(approval.input().clone()),
            _ => None,
        }
    }

    pub(crate) fn show_error_dialog(&mut self, title: &str, message: String) {
        self.show_dialog(MessageDialogKind::Error, title, message);
    }

    fn show_dialog(&mut self, kind: MessageDialogKind, title: &str, message: String) {
        if !matches!(self.overlay, Some(Overlay::Dialog(_))) {
            self.dialog_back = self.overlay.take().map(Box::new);
        }
        self.overlay = Some(Overlay::Dialog(MessageDialogOverlay::new(
            kind, title, message,
        )));
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

    pub(crate) fn open_shortcuts(&mut self) {
        let back = match self.overlay.take() {
            Some(Overlay::Settings(settings)) => ShortcutsBack::Settings(settings),
            _ => ShortcutsBack::CommandPalette,
        };
        self.overlay = Some(Overlay::Shortcuts(back));
    }

    pub(crate) fn close_overlay(&mut self) {
        self.overlay = None;
        self.dialog_back = None;
        self.provider_overlay_back = None;
        self.provider_form_back = None;
    }

    pub(crate) fn back_overlay(&mut self) {
        let command_palette = self.command_palette_overlay();
        self.overlay = match self.overlay.take() {
            Some(Overlay::Shortcuts(ShortcutsBack::Settings(settings))) => {
                Some(Overlay::Settings(settings))
            }
            Some(Overlay::Shortcuts(ShortcutsBack::CommandPalette))
            | Some(Overlay::Settings(_)) => Some(command_palette.clone()),
            Some(Overlay::ProviderManager(_)) => match self.provider_overlay_back.take() {
                Some(ProviderOverlayBack::Settings(settings)) => Some(Overlay::Settings(settings)),
                Some(ProviderOverlayBack::CommandPalette) | None => Some(command_palette),
            },
            Some(Overlay::PlanApproval(_) | Overlay::PermissionReview(_) | Overlay::Dialog(_)) => {
                self.dialog_back.take().map(|overlay| *overlay)
            }
            Some(Overlay::ModelPicker(_)) => {
                self.provider_form_back.take().map(Overlay::ProviderForm)
            }
            _ => None,
        };
    }

    fn command_palette_overlay(&self) -> Overlay {
        Overlay::command_palette_for_plan(PlanPaletteContext::from_snapshot(
            self.plan.snapshot(),
            self.plan.selected_node_id(),
            self.plan.is_open(),
            self.plan.is_focused(),
        ))
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

    pub(crate) fn artifact_timeline_indexes(&self) -> Vec<usize> {
        self.timeline
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.is_artifact_candidate().then_some(index))
            .collect()
    }

    pub(crate) fn selected_artifact_timeline_index(&self) -> Option<usize> {
        if let Some(index) = self.artifact_review_timeline_index
            && self
                .timeline
                .get(index)
                .is_some_and(TimelineItem::is_artifact_candidate)
        {
            return Some(index);
        }

        self.artifact_timeline_indexes().last().copied()
    }

    pub(crate) fn push_timeline_item(&mut self, item: TimelineItem) {
        self.timeline.push(item);
        self.timeline_scroll_offset = 0;
        self.focus_scroll_offset = 0;
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
        self.focus_scroll_offset = 0;
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
            self.focus_scroll_offset = 0;
            self.timeline_review_user_index = None;
        }
    }

    pub(crate) fn timeline_scroll_offset(&self) -> usize {
        self.timeline_scroll_offset
    }

    pub(crate) fn focus_scroll_offset(&self) -> usize {
        self.focus_scroll_offset
    }

    pub(crate) fn timeline_review_user_index(&self) -> Option<usize> {
        self.timeline_review_user_index
    }

    pub(crate) fn is_timeline_reviewing(&self) -> bool {
        self.timeline_review_user_index.is_some()
    }

    pub(crate) fn is_artifact_reviewing(&self) -> bool {
        self.artifact_review_timeline_index.is_some()
    }

    pub(crate) fn artifact_review_timeline_index(&self) -> Option<usize> {
        self.artifact_review_timeline_index
    }

    pub(crate) fn exit_timeline_review(&mut self) {
        self.timeline_review_user_index = None;
        self.timeline_scroll_offset = 0;
    }

    pub(crate) fn exit_artifact_review(&mut self) {
        self.artifact_review_timeline_index = None;
        self.focus_scroll_offset = 0;
    }

    pub(crate) fn follow_latest(&mut self) {
        self.timeline_scroll_offset = 0;
        self.focus_scroll_offset = 0;
        self.timeline_review_user_index = None;
        self.artifact_review_timeline_index = None;
        self.pending_empty_input_quit = false;
    }

    pub(crate) fn select_previous_artifact(&mut self) {
        let indexes = self.artifact_timeline_indexes();
        if self.artifact_review_timeline_index.is_none() {
            self.artifact_review_timeline_index = indexes.last().copied();
            self.focus_scroll_offset = 0;
            self.pending_empty_input_quit = false;
            return;
        }
        let Some(selected) = self.selected_artifact_timeline_index() else {
            return;
        };
        let Some(position) = indexes.iter().position(|index| *index == selected) else {
            return;
        };
        if position == 0 {
            return;
        }
        self.pending_empty_input_quit = false;
        self.artifact_review_timeline_index = Some(indexes[position - 1]);
        self.focus_scroll_offset = 0;
    }

    pub(crate) fn select_next_artifact(&mut self) {
        let Some(review_index) = self.artifact_review_timeline_index else {
            return;
        };
        let indexes = self.artifact_timeline_indexes();
        let Some(position) = indexes.iter().position(|index| *index == review_index) else {
            self.artifact_review_timeline_index = None;
            return;
        };
        self.pending_empty_input_quit = false;
        if position + 1 >= indexes.len() {
            self.artifact_review_timeline_index = None;
        } else {
            self.artifact_review_timeline_index = Some(indexes[position + 1]);
        }
        self.focus_scroll_offset = 0;
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

    pub(crate) fn scroll_focus_up_by(&mut self, lines: usize) {
        self.pending_empty_input_quit = false;
        self.focus_scroll_offset = self.focus_scroll_offset.saturating_sub(lines);
    }

    pub(crate) fn scroll_focus_down_by(&mut self, lines: usize) {
        self.pending_empty_input_quit = false;
        self.focus_scroll_offset = self.focus_scroll_offset.saturating_add(lines);
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

    pub(crate) fn set_model_label(&mut self, label: String) {
        self.model_label = label;
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
        self.status_parts().join("  ")
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

fn is_active_run_state(state: InteractiveRunState) -> bool {
    matches!(
        state,
        InteractiveRunState::RunningModel
            | InteractiveRunState::RunningTool
            | InteractiveRunState::Interrupting
    )
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

impl TimelineItem {
    pub(crate) fn is_artifact_candidate(&self) -> bool {
        matches!(
            self,
            TimelineItem::Muted { .. }
                | TimelineItem::Expanded { .. }
                | TimelineItem::ExpandedDetail { .. }
                | TimelineItem::Diagnostic { .. }
                | TimelineItem::Patch { .. }
        )
    }
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
