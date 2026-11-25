use super::{
    command::all_commands,
    input::{TextInput, TextInputViewport},
    provider_overlay::{
        ModelPickerOverlay, ProviderFormOverlay, ProviderManagerOverlay, ProviderOverlayAction,
    },
    reasoning_picker::{ReasoningPickerAction, ReasoningPickerOverlay},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(crate) use super::command::{CommandSpec, PaletteCommand};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlanPaletteContext {
    has_plans: bool,
}

impl PlanPaletteContext {
    pub(crate) fn from_plan_state(plan: &super::plan_view::PlanUiState) -> Self {
        Self {
            has_plans: plan.has_plans(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Overlay {
    CommandPalette(CommandPalette),
    Settings(SettingsOverlay),
    Skills(super::skills_overlay::SkillsOverlay),
    ProviderManager(ProviderManagerOverlay),
    ProviderForm(ProviderFormOverlay),
    ModelPicker(ModelPickerOverlay),
    ReasoningPicker(ReasoningPickerOverlay),
    PlanView(super::plan_view::PlanViewOverlay),
    PermissionReview(PermissionReviewOverlay),
    Dialog(MessageDialogOverlay),
    CommandDetails(super::command_details::CommandDetails),
    SubagentInspect(super::subagent_view::SubagentTakeoverOverlay),
    BackgroundTaskInspect(super::task_dock::BackgroundTaskOverlay),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageDialogKind {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageDialogOverlay {
    title: String,
    message: String,
    kind: MessageDialogKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionReviewOverlay {
    view: crate::permission_prompt::PermissionReviewView,
    scroll: u16,
}

impl PermissionReviewOverlay {
    #[cfg(test)]
    pub(crate) fn new(approval_id: String, body: String) -> Self {
        Self::from_view(
            crate::permission_prompt::PermissionReviewView::from_plain_text(approval_id, body),
        )
    }

    pub(crate) fn from_view(view: crate::permission_prompt::PermissionReviewView) -> Self {
        Self { view, scroll: 0 }
    }

    pub(crate) fn approval_id(&self) -> &str {
        &self.view.approval_id
    }

    pub(crate) fn view(&self) -> &crate::permission_prompt::PermissionReviewView {
        &self.view
    }

    pub(crate) const fn scroll(&self) -> u16 {
        self.scroll
    }

    pub(crate) fn scroll_up(&mut self, lines: u16) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub(crate) fn scroll_down(&mut self, lines: u16) {
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub(crate) fn set_queue_total(&mut self, total: usize) {
        self.view.queue_total = total.max(1);
    }
}

impl MessageDialogOverlay {
    pub(crate) fn new(kind: MessageDialogKind, title: &str, message: String) -> Self {
        Self {
            title: title.to_owned(),
            message,
            kind,
        }
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn kind(&self) -> MessageDialogKind {
        self.kind
    }
}

pub(crate) use super::settings_overlay::{SettingDirection, SettingItem, SettingsOverlay};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandPalette {
    query: TextInput,
    selected: usize,
    plan: PlanPaletteContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverlayKeyResult {
    Consumed,
    PreviousCommand,
    NextCommand,
    CopyText(String),
    Close,
    Back,
    Run(PaletteCommand),
    AdjustSetting(SettingItem, SettingDirection),
    ResetSetting(SettingItem),
    BeginSettingEdit(SettingItem),
    CommitSettingEdit {
        item: SettingItem,
        value: String,
    },
    PickSettingModel(SettingItem),
    PickSettingReasoning(SettingItem),
    OpenSettingChoices(SettingItem),
    SelectSettingChoice {
        item: SettingItem,
        index: usize,
    },
    CaptureShortcut {
        action: super::keymap::KeyAction,
        key: KeyEvent,
    },
    ApprovePermission(String),
    DenyPermission(String),
    InsertSkill(String),
    Provider(ProviderOverlayAction),
    SubagentPrevious,
    SubagentNext,
    SubagentRefresh,
    SubagentStop(String),
    SubagentSendMessage {
        agent_id: String,
        message: String,
    },
    RefreshBackgroundTask(String),
    StopBackgroundTask(String),
}

impl CommandPalette {
    pub(crate) fn query(&self) -> &str {
        self.query.text()
    }

    pub(crate) fn query_viewport(&self, width: usize) -> TextInputViewport {
        self.query.viewport(width)
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn select(&mut self, index: usize) {
        let count = self.visible_commands().len();
        if count > 0 {
            self.selected = index.min(count - 1);
        }
    }

    pub(crate) fn visible_commands(&self) -> Vec<&'static CommandSpec> {
        let query = self.query.text().trim().to_ascii_lowercase();
        let slash_query = query.strip_prefix('/').unwrap_or(&query);
        all_commands()
            .iter()
            .filter(|command| command.show_in_palette())
            .filter(|command| plan_command_is_available(command.command, self.plan))
            .filter(|command| {
                query.is_empty()
                    || fuzzy_matches(command.label, &query)
                    || fuzzy_matches(command.category, &query)
                    || command
                        .all_slash_names()
                        .any(|name| fuzzy_matches(name, slash_query))
            })
            .collect()
    }

    fn insert_paste(&mut self, text: &str) {
        let single_line = crate::text::single_line_paste_collapsed(text);
        self.query.insert_str(&single_line);
        self.selected = 0;
    }

    fn handle_key(&mut self, key: KeyEvent) -> OverlayKeyResult {
        match key.code {
            KeyCode::Esc => OverlayKeyResult::Close,
            KeyCode::Enter => self
                .visible_commands()
                .get(self.selected)
                .map_or(OverlayKeyResult::Consumed, |command| {
                    OverlayKeyResult::Run(command.command)
                }),
            KeyCode::Down => {
                let count = self.visible_commands().len();
                if count > 0 {
                    self.selected = (self.selected + 1).min(count - 1);
                }
                OverlayKeyResult::Consumed
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                OverlayKeyResult::Consumed
            }
            _ => {
                self.query.handle_key(key);
                let count = self.visible_commands().len();
                self.selected = self.selected.min(count.saturating_sub(1));
                OverlayKeyResult::Consumed
            }
        }
    }
}

impl Overlay {
    pub(crate) fn command_palette_for_plan(plan: PlanPaletteContext) -> Self {
        Self::CommandPalette(CommandPalette {
            plan,
            ..CommandPalette::default()
        })
    }

    pub(crate) fn settings_for_viewport(compact: bool) -> Self {
        Self::Settings(SettingsOverlay::for_viewport(compact))
    }

    #[cfg(test)]
    pub(crate) fn permission_review(approval_id: String, body: String) -> Self {
        Self::PermissionReview(PermissionReviewOverlay::new(approval_id, body))
    }

    pub(crate) fn permission_review_view(
        view: crate::permission_prompt::PermissionReviewView,
    ) -> Self {
        Self::PermissionReview(PermissionReviewOverlay::from_view(view))
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> OverlayKeyResult {
        if key.code == KeyCode::Char('x')
            && key.modifiers.contains(KeyModifiers::ALT)
            && !matches!(self, Self::PermissionReview(_))
            && !matches!(self, Self::Settings(settings) if settings.is_capturing_shortcut())
        {
            return OverlayKeyResult::Close;
        }

        match self {
            Self::CommandPalette(palette) => palette.handle_key(key),
            Self::CommandDetails(details) => details.handle_key(key),
            Self::Settings(settings) => settings.handle_key(key),
            Self::Skills(picker) => match picker.handle_key(key) {
                super::skills_overlay::SkillsOverlayAction::Consumed => OverlayKeyResult::Consumed,
                super::skills_overlay::SkillsOverlayAction::Close => OverlayKeyResult::Close,
                super::skills_overlay::SkillsOverlayAction::Select(name) => {
                    OverlayKeyResult::InsertSkill(name)
                }
            },
            Self::ProviderManager(manager) => match manager.handle_key(key) {
                ProviderOverlayAction::Back => OverlayKeyResult::Back,
                action => OverlayKeyResult::Provider(action),
            },
            Self::ProviderForm(form) => match form.handle_key(key) {
                ProviderOverlayAction::Back => {
                    OverlayKeyResult::Provider(ProviderOverlayAction::OpenProviderManager)
                }
                action => OverlayKeyResult::Provider(action),
            },
            Self::ModelPicker(picker) => match picker.handle_key(key) {
                ProviderOverlayAction::Back
                    if matches!(
                        picker.target(),
                        super::provider_overlay::ModelPickerTarget::ProviderForm
                    ) =>
                {
                    OverlayKeyResult::Provider(ProviderOverlayAction::BackToProviderForm)
                }
                ProviderOverlayAction::Back
                    if matches!(
                        picker.target(),
                        super::provider_overlay::ModelPickerTarget::QuickSwitch
                    ) =>
                {
                    OverlayKeyResult::Close
                }
                ProviderOverlayAction::Back
                    if matches!(
                        picker.target(),
                        super::provider_overlay::ModelPickerTarget::SettingsActiveModel
                            | super::provider_overlay::ModelPickerTarget::RoleBinding(_)
                    ) =>
                {
                    OverlayKeyResult::Back
                }
                ProviderOverlayAction::Back => {
                    OverlayKeyResult::Provider(ProviderOverlayAction::OpenProviderManager)
                }
                action => OverlayKeyResult::Provider(action),
            },
            Self::ReasoningPicker(picker) => match picker.handle_key(key) {
                ReasoningPickerAction::Back => OverlayKeyResult::Back,
                ReasoningPickerAction::Consumed => OverlayKeyResult::Consumed,
                ReasoningPickerAction::SelectReasoning {
                    alias,
                    model,
                    reasoning_effort,
                    target,
                } => OverlayKeyResult::Provider(ProviderOverlayAction::SelectReasoning {
                    alias,
                    model,
                    reasoning_effort,
                    target,
                }),
            },
            Self::PlanView(_) => OverlayKeyResult::Consumed,
            Self::PermissionReview(review) => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    OverlayKeyResult::ApprovePermission(review.approval_id().to_owned())
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    OverlayKeyResult::DenyPermission(review.approval_id().to_owned())
                }
                KeyCode::Up => {
                    review.scroll_up(1);
                    OverlayKeyResult::Consumed
                }
                KeyCode::Down => {
                    review.scroll_down(1);
                    OverlayKeyResult::Consumed
                }
                KeyCode::PageUp => {
                    review.scroll_up(5);
                    OverlayKeyResult::Consumed
                }
                KeyCode::PageDown => {
                    review.scroll_down(5);
                    OverlayKeyResult::Consumed
                }
                _ => OverlayKeyResult::Consumed,
            },
            Self::Dialog(_) => match key.code {
                KeyCode::Esc | KeyCode::Enter => OverlayKeyResult::Back,
                _ => OverlayKeyResult::Consumed,
            },
            Self::SubagentInspect(takeover) => match takeover.handle_key(key) {
                super::subagent_view::SubagentTakeoverAction::Consumed => {
                    OverlayKeyResult::Consumed
                }
                super::subagent_view::SubagentTakeoverAction::Back => OverlayKeyResult::Close,
                super::subagent_view::SubagentTakeoverAction::Previous => {
                    OverlayKeyResult::SubagentPrevious
                }
                super::subagent_view::SubagentTakeoverAction::Next => {
                    OverlayKeyResult::SubagentNext
                }
                super::subagent_view::SubagentTakeoverAction::Refresh => {
                    OverlayKeyResult::SubagentRefresh
                }
                super::subagent_view::SubagentTakeoverAction::CopyText(text) => {
                    OverlayKeyResult::CopyText(text)
                }
                super::subagent_view::SubagentTakeoverAction::Stop(agent_id) => {
                    OverlayKeyResult::SubagentStop(agent_id)
                }
                super::subagent_view::SubagentTakeoverAction::SendMessage { agent_id, message } => {
                    OverlayKeyResult::SubagentSendMessage { agent_id, message }
                }
            },
            Self::BackgroundTaskInspect(inspector) => match inspector.handle_key(key) {
                super::task_dock::BackgroundTaskOverlayAction::Consumed => {
                    OverlayKeyResult::Consumed
                }
                super::task_dock::BackgroundTaskOverlayAction::Close => OverlayKeyResult::Close,
                super::task_dock::BackgroundTaskOverlayAction::Refresh(id) => {
                    OverlayKeyResult::RefreshBackgroundTask(id)
                }
                super::task_dock::BackgroundTaskOverlayAction::Stop(id) => {
                    OverlayKeyResult::StopBackgroundTask(id)
                }
            },
        }
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        match self {
            Self::CommandPalette(palette) => palette.insert_paste(text),
            Self::Settings(settings) => settings.insert_paste(text),
            Self::Skills(picker) => picker.insert_paste(text),
            Self::ProviderForm(form) => form.insert_paste(text),
            Self::ProviderManager(manager) => manager.insert_paste(text),
            Self::SubagentInspect(takeover) => takeover.insert_paste(text),
            Self::PlanView(plan_view) => plan_view.insert_paste(text),
            Self::CommandDetails(_)
            | Self::BackgroundTaskInspect(_)
            | Self::ModelPicker(_)
            | Self::PermissionReview(_)
            | Self::Dialog(_) => {}
            Self::ReasoningPicker(picker) => picker.insert_paste(text),
        }
    }
}

fn fuzzy_matches(candidate: &str, query: &str) -> bool {
    let candidate = candidate.to_ascii_lowercase();
    if candidate.contains(query) {
        return true;
    }

    let mut query_chars = query.chars();
    let mut next = query_chars.next();
    for character in candidate.chars() {
        if Some(character) == next {
            next = query_chars.next();
            if next.is_none() {
                return true;
            }
        }
    }
    next.is_none()
}

fn plan_command_is_available(command: PaletteCommand, context: PlanPaletteContext) -> bool {
    match command {
        PaletteCommand::ViewPlans => context.has_plans,
        _ => true,
    }
}
