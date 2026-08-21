use super::{
    command::all_commands,
    input::{TextInput, TextInputViewport},
    provider_overlay::{
        ModelPickerOverlay, ProviderFormOverlay, ProviderManagerOverlay, ProviderOverlayAction,
    },
    reasoning_picker::{ReasoningPickerAction, ReasoningPickerOverlay},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{PlanAttemptOutcome, PlanNodeId, PlanPhase, PlanSnapshot};

pub(crate) use super::command::{CommandSpec, PaletteCommand};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlanPaletteContext {
    has_plan: bool,
    plan_open: bool,
    plan_focused: bool,
    phase: Option<PlanPhase>,
    plan_ready: bool,
    retry_selected: bool,
}

impl PlanPaletteContext {
    pub(crate) fn from_snapshot(
        snapshot: Option<&PlanSnapshot>,
        selected_node_id: Option<&PlanNodeId>,
        plan_open: bool,
        plan_focused: bool,
    ) -> Self {
        let retry_selected = snapshot.is_some_and(|snapshot| {
            let selected_is_blocked = selected_node_id.is_some_and(|selected| {
                snapshot.nodes.iter().any(|node| {
                    &node.id == selected && node.status == merry_core::PlanNodeStatus::Blocked
                })
            });
            let latest_outcome = selected_node_id.and_then(|selected| {
                snapshot
                    .attempts
                    .iter()
                    .rev()
                    .find(|attempt| &attempt.node_id == selected)
                    .and_then(|attempt| attempt.outcome)
            });
            selected_is_blocked && latest_outcome == Some(PlanAttemptOutcome::Interrupted)
        });
        Self {
            has_plan: snapshot.is_some(),
            plan_open,
            plan_focused,
            phase: snapshot.map(|snapshot| snapshot.phase),
            plan_ready: snapshot.is_some_and(|snapshot| snapshot.root_node_id.is_some()),
            retry_selected,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Overlay {
    CommandPalette(CommandPalette),
    Settings(SettingsOverlay),
    ProviderManager(ProviderManagerOverlay),
    ProviderForm(ProviderFormOverlay),
    ModelPicker(ModelPickerOverlay),
    ReasoningPicker(ReasoningPickerOverlay),
    PlanApproval(PlanApprovalOverlay),
    PermissionReview(PermissionReviewOverlay),
    Dialog(MessageDialogOverlay),
    Shortcuts(ShortcutsBack),
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
pub(crate) struct PlanApprovalOverlay {
    message: String,
    input: merry_runtime::PlanApprovalInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionReviewOverlay {
    approval_id: String,
    body: String,
}

impl PermissionReviewOverlay {
    pub(crate) fn new(approval_id: String, body: String) -> Self {
        Self { approval_id, body }
    }

    pub(crate) fn approval_id(&self) -> &str {
        &self.approval_id
    }

    pub(crate) fn body(&self) -> &str {
        &self.body
    }
}

impl PlanApprovalOverlay {
    pub(crate) fn new(message: String, input: merry_runtime::PlanApprovalInput) -> Self {
        Self { message, input }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn input(&self) -> &merry_runtime::PlanApprovalInput {
        &self.input
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShortcutsBack {
    CommandPalette,
    Settings(SettingsOverlay),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingItem {
    CodeTheme,
    DefaultProvider,
    DefaultModel,
    ReasoningEffort,
    ContextWindow,
    AutoCompaction,
    ContextStrategy,
    Subagents,
    MaxThreads,
    KeyboardShortcuts,
}

impl SettingItem {
    pub(crate) const ALL: [Self; 10] = [
        Self::CodeTheme,
        Self::DefaultProvider,
        Self::DefaultModel,
        Self::ReasoningEffort,
        Self::ContextWindow,
        Self::AutoCompaction,
        Self::ContextStrategy,
        Self::Subagents,
        Self::MaxThreads,
        Self::KeyboardShortcuts,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SettingsOverlay {
    selected: usize,
    model_editor: Option<TextInput>,
    reasoning_editor: Option<TextInput>,
    context_window_editor: Option<TextInput>,
    notice: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandPalette {
    query: TextInput,
    selected: usize,
    plan: PlanPaletteContext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverlayKeyResult {
    Consumed,
    Close,
    Back,
    Run(PaletteCommand),
    AdjustSetting(SettingItem, SettingDirection),
    ResetSetting(SettingItem),
    BeginModelEdit,
    CommitModel(String),
    BeginReasoningEdit,
    CommitReasoning(String),
    BeginContextWindowEdit,
    CommitContextWindow(String),
    OpenShortcuts,
    ConfirmPlanApproval,
    ApprovePermission(String),
    DenyPermission(String),
    Provider(ProviderOverlayAction),
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

    pub(crate) fn visible_commands(&self) -> Vec<&'static CommandSpec> {
        let query = self.query.text().trim().to_ascii_lowercase();
        let slash_query = query.strip_prefix('/').unwrap_or(&query);
        all_commands()
            .iter()
            .filter(|command| plan_command_is_available(command.command, self.plan))
            .filter(|command| {
                query.is_empty()
                    || fuzzy_matches(command.label, &query)
                    || fuzzy_matches(command.category, &query)
                    || command
                        .slash_name()
                        .is_some_and(|name| fuzzy_matches(name, slash_query))
            })
            .collect()
    }

    fn insert_paste(&mut self, text: &str) {
        self.query.insert_str(text);
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

impl SettingsOverlay {
    pub(crate) fn selected_item(&self) -> SettingItem {
        SettingItem::ALL[self.selected.min(SettingItem::ALL.len() - 1)]
    }

    pub(crate) fn model_editor(&self) -> Option<&TextInput> {
        self.model_editor.as_ref()
    }

    pub(crate) fn context_window_editor(&self) -> Option<&TextInput> {
        self.context_window_editor.as_ref()
    }

    pub(crate) fn reasoning_editor(&self) -> Option<&TextInput> {
        self.reasoning_editor.as_ref()
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn begin_model_edit(&mut self, value: String) {
        let mut input = TextInput::default();
        input.replace_text(value);
        self.model_editor = Some(input);
        self.notice = None;
    }

    pub(crate) fn begin_context_window_edit(&mut self, value: String) {
        let mut input = TextInput::default();
        input.replace_text(value);
        self.context_window_editor = Some(input);
        self.notice = None;
    }

    pub(crate) fn begin_reasoning_edit(&mut self, value: String) {
        let mut input = TextInput::default();
        input.replace_text(value);
        self.reasoning_editor = Some(input);
        self.notice = None;
    }

    pub(crate) fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    fn insert_paste(&mut self, text: &str) {
        if let Some(editor) = self.model_editor.as_mut() {
            editor.insert_str(text);
        } else if let Some(editor) = self.reasoning_editor.as_mut() {
            editor.insert_str(text);
        } else if let Some(editor) = self.context_window_editor.as_mut() {
            editor.insert_str(text);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> OverlayKeyResult {
        if let Some(editor) = self.model_editor.as_mut() {
            return match key.code {
                KeyCode::Esc => {
                    self.model_editor = None;
                    OverlayKeyResult::Consumed
                }
                KeyCode::Enter => {
                    let value = editor.text().to_owned();
                    self.model_editor = None;
                    OverlayKeyResult::CommitModel(value)
                }
                _ => {
                    editor.handle_key(key);
                    OverlayKeyResult::Consumed
                }
            };
        }

        if let Some(editor) = self.context_window_editor.as_mut() {
            return match key.code {
                KeyCode::Esc => {
                    self.context_window_editor = None;
                    OverlayKeyResult::Consumed
                }
                KeyCode::Enter => {
                    let value = editor.text().to_owned();
                    self.context_window_editor = None;
                    OverlayKeyResult::CommitContextWindow(value)
                }
                _ => {
                    editor.handle_key(key);
                    OverlayKeyResult::Consumed
                }
            };
        }

        if let Some(editor) = self.reasoning_editor.as_mut() {
            return match key.code {
                KeyCode::Esc => {
                    self.reasoning_editor = None;
                    OverlayKeyResult::Consumed
                }
                KeyCode::Enter => {
                    let value = editor.text().to_owned();
                    self.reasoning_editor = None;
                    OverlayKeyResult::CommitReasoning(value)
                }
                _ => {
                    editor.handle_key(key);
                    OverlayKeyResult::Consumed
                }
            };
        }

        match key.code {
            KeyCode::Esc => OverlayKeyResult::Back,
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(SettingItem::ALL.len() - 1);
                self.notice = None;
                OverlayKeyResult::Consumed
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.notice = None;
                OverlayKeyResult::Consumed
            }
            KeyCode::Left => {
                if self.selected_item() == SettingItem::DefaultProvider {
                    OverlayKeyResult::Provider(ProviderOverlayAction::OpenProviderManager)
                } else {
                    OverlayKeyResult::AdjustSetting(
                        self.selected_item(),
                        SettingDirection::Previous,
                    )
                }
            }
            KeyCode::Right => {
                if self.selected_item() == SettingItem::DefaultProvider {
                    OverlayKeyResult::Provider(ProviderOverlayAction::OpenProviderManager)
                } else {
                    OverlayKeyResult::AdjustSetting(self.selected_item(), SettingDirection::Next)
                }
            }
            KeyCode::Backspace | KeyCode::Delete => {
                OverlayKeyResult::ResetSetting(self.selected_item())
            }
            KeyCode::Enter => match self.selected_item() {
                SettingItem::DefaultModel => OverlayKeyResult::BeginModelEdit,
                SettingItem::ReasoningEffort => OverlayKeyResult::BeginReasoningEdit,
                SettingItem::ContextWindow => OverlayKeyResult::BeginContextWindowEdit,
                SettingItem::DefaultProvider => {
                    OverlayKeyResult::Provider(ProviderOverlayAction::OpenProviderManager)
                }
                SettingItem::KeyboardShortcuts => OverlayKeyResult::OpenShortcuts,
                item => OverlayKeyResult::AdjustSetting(item, SettingDirection::Next),
            },
            _ => OverlayKeyResult::Consumed,
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

    pub(crate) fn settings() -> Self {
        Self::Settings(SettingsOverlay::default())
    }

    pub(crate) fn plan_approval(message: String, input: merry_runtime::PlanApprovalInput) -> Self {
        Self::PlanApproval(PlanApprovalOverlay::new(message, input))
    }

    pub(crate) fn permission_review(approval_id: String, body: String) -> Self {
        Self::PermissionReview(PermissionReviewOverlay::new(approval_id, body))
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> OverlayKeyResult {
        if key.code == KeyCode::Char('p')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !matches!(self, Self::PermissionReview(_))
        {
            return OverlayKeyResult::Close;
        }

        match self {
            Self::CommandPalette(palette) => palette.handle_key(key),
            Self::Settings(settings) => settings.handle_key(key),
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
                    if picker.target()
                        == super::provider_overlay::ModelPickerTarget::ProviderForm =>
                {
                    OverlayKeyResult::Provider(ProviderOverlayAction::BackToProviderForm)
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
            Self::PlanApproval(_) => match key.code {
                KeyCode::Enter => OverlayKeyResult::ConfirmPlanApproval,
                KeyCode::Esc => OverlayKeyResult::Back,
                _ => OverlayKeyResult::Consumed,
            },
            Self::PermissionReview(review) => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    OverlayKeyResult::ApprovePermission(review.approval_id().to_owned())
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    OverlayKeyResult::DenyPermission(review.approval_id().to_owned())
                }
                _ => OverlayKeyResult::Consumed,
            },
            Self::Dialog(_) => match key.code {
                KeyCode::Esc | KeyCode::Enter => OverlayKeyResult::Back,
                _ => OverlayKeyResult::Consumed,
            },
            Self::Shortcuts(_) => match key.code {
                KeyCode::Esc => OverlayKeyResult::Back,
                _ => OverlayKeyResult::Consumed,
            },
        }
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        match self {
            Self::CommandPalette(palette) => palette.insert_paste(text),
            Self::Settings(settings) => settings.insert_paste(text),
            Self::ProviderForm(form) => form.insert_paste(text),
            Self::ProviderManager(_)
            | Self::ModelPicker(_)
            | Self::PlanApproval(_)
            | Self::PermissionReview(_)
            | Self::Dialog(_) => {}
            Self::ReasoningPicker(picker) => picker.insert_paste(text),
            Self::Shortcuts(_) => {}
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
        PaletteCommand::EnterPlanMode => {
            !context.has_plan
                || matches!(
                    context.phase,
                    Some(PlanPhase::Completed | PlanPhase::Blocked | PlanPhase::Cancelled)
                )
        }
        PaletteCommand::ApprovePlan => {
            context.plan_ready
                && matches!(
                    context.phase,
                    Some(PlanPhase::Planning | PlanPhase::AwaitingApproval)
                )
        }
        PaletteCommand::RevisePlan => matches!(
            context.phase,
            Some(PlanPhase::AwaitingApproval | PlanPhase::Executing)
        ),
        PaletteCommand::OpenPlan => context.has_plan && !context.plan_open,
        PaletteCommand::FocusPlan => context.has_plan && context.plan_open && !context.plan_focused,
        PaletteCommand::ClosePlan => context.has_plan && context.plan_open,
        // Plan no longer owns an automatic scheduler. Child lifecycle is
        // runtime-owned, so pause/resume are intentionally absent from the UI.
        PaletteCommand::RetryPlanNode => context.retry_selected,
        PaletteCommand::CancelPlan => matches!(
            context.phase,
            Some(PlanPhase::Planning | PlanPhase::AwaitingApproval | PlanPhase::Executing)
        ),
        _ => true,
    }
}
