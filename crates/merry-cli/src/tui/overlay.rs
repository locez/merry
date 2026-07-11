use super::{
    input::{TextInput, TextInputViewport},
    keymap::KeyAction,
    provider_overlay::{
        ModelPickerOverlay, ProviderFormOverlay, ProviderManagerOverlay, ProviderOverlayAction,
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteCommand {
    OpenSettings,
    OpenProviders,
    ShowShortcuts,
    FollowLatest,
    ReviewPreviousArtifact,
    ReviewNextArtifact,
    ReviewPreviousUserInput,
    Interrupt,
    ResumeSuspended,
    DiscardSuspended,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandSpec {
    pub(crate) command: PaletteCommand,
    pub(crate) category: &'static str,
    pub(crate) label: &'static str,
    pub(crate) key_action: Option<KeyAction>,
}

const COMMANDS: [CommandSpec; 11] = [
    CommandSpec {
        command: PaletteCommand::OpenSettings,
        category: "Merry",
        label: "Settings",
        key_action: None,
    },
    CommandSpec {
        command: PaletteCommand::OpenProviders,
        category: "Merry",
        label: "Providers & models",
        key_action: None,
    },
    CommandSpec {
        command: PaletteCommand::ShowShortcuts,
        category: "Merry",
        label: "Keyboard shortcuts",
        key_action: None,
    },
    CommandSpec {
        command: PaletteCommand::FollowLatest,
        category: "Navigation",
        label: "Follow latest",
        key_action: Some(KeyAction::FollowLatestArtifact),
    },
    CommandSpec {
        command: PaletteCommand::ReviewPreviousArtifact,
        category: "Navigation",
        label: "Previous artifact",
        key_action: Some(KeyAction::ReviewPreviousArtifact),
    },
    CommandSpec {
        command: PaletteCommand::ReviewNextArtifact,
        category: "Navigation",
        label: "Next artifact",
        key_action: Some(KeyAction::ReviewNextArtifact),
    },
    CommandSpec {
        command: PaletteCommand::ReviewPreviousUserInput,
        category: "Navigation",
        label: "Previous user input",
        key_action: Some(KeyAction::ReviewPreviousUserInput),
    },
    CommandSpec {
        command: PaletteCommand::Interrupt,
        category: "Runtime",
        label: "Interrupt current run",
        key_action: Some(KeyAction::Interrupt),
    },
    CommandSpec {
        command: PaletteCommand::ResumeSuspended,
        category: "Runtime",
        label: "Resume suspended input",
        key_action: Some(KeyAction::ResumeSuspended),
    },
    CommandSpec {
        command: PaletteCommand::DiscardSuspended,
        category: "Runtime",
        label: "Discard suspended input",
        key_action: Some(KeyAction::DiscardSuspended),
    },
    CommandSpec {
        command: PaletteCommand::Quit,
        category: "Session",
        label: "Quit Merry",
        key_action: Some(KeyAction::Quit),
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Overlay {
    CommandPalette(CommandPalette),
    Settings(SettingsOverlay),
    ProviderManager(ProviderManagerOverlay),
    ProviderForm(ProviderFormOverlay),
    ModelPicker(ModelPickerOverlay),
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
    context_window_editor: Option<TextInput>,
    notice: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandPalette {
    query: TextInput,
    selected: usize,
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
    BeginContextWindowEdit,
    CommitContextWindow(String),
    OpenShortcuts,
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
        COMMANDS
            .iter()
            .filter(|command| {
                query.is_empty()
                    || fuzzy_matches(command.label, &query)
                    || fuzzy_matches(command.category, &query)
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

    pub(crate) fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    fn insert_paste(&mut self, text: &str) {
        if let Some(editor) = self.model_editor.as_mut() {
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
    pub(crate) fn command_palette() -> Self {
        Self::CommandPalette(CommandPalette::default())
    }

    pub(crate) fn settings() -> Self {
        Self::Settings(SettingsOverlay::default())
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> OverlayKeyResult {
        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
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
            Self::ProviderManager(_) | Self::ModelPicker(_) | Self::Dialog(_) => {}
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
