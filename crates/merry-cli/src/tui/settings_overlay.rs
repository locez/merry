//! Grouped Settings Workbench state, focus/drill-down navigation, and key handling.

mod items;

pub(crate) use items::{SettingItem, SettingsCategory};

use super::{
    input::TextInput, keymap::KeyAction, overlay::OverlayKeyResult,
    provider_overlay::ProviderOverlayAction,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Which pane currently owns keyboard input in the Settings Workbench.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsFocus {
    #[default]
    Categories,
    Items,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingDirection {
    Previous,
    Next,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingsInlineEditor {
    pub(crate) item: SettingItem,
    pub(crate) input: TextInput,
}

/// One selectable option in the narrow-screen Level-3 choice submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingChoice {
    pub(crate) label: String,
    pub(crate) current: bool,
}

/// Level-3 option picker opened from a multi-choice setting in compact drill-down mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingsChoiceMenu {
    item: SettingItem,
    choices: Vec<SettingChoice>,
    selected: usize,
}

impl SettingsChoiceMenu {
    pub(crate) fn new(item: SettingItem, choices: Vec<SettingChoice>) -> Self {
        let selected = choices
            .iter()
            .position(|choice| choice.current)
            .unwrap_or(0);
        Self {
            item,
            choices,
            selected,
        }
    }

    pub(crate) fn item(&self) -> SettingItem {
        self.item
    }

    pub(crate) fn choices(&self) -> &[SettingChoice] {
        &self.choices
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn select(&mut self, index: usize) {
        if !self.choices.is_empty() {
            self.selected = index.min(self.choices.len() - 1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingsOverlay {
    category: SettingsCategory,
    focus: SettingsFocus,
    compact: bool,
    selected: usize,
    selected_role: String,
    inline_editor: Option<SettingsInlineEditor>,
    choice_menu: Option<SettingsChoiceMenu>,
    capturing_shortcut: Option<KeyAction>,
    notice: Option<String>,
}

impl Default for SettingsOverlay {
    fn default() -> Self {
        Self::for_viewport(false)
    }
}

impl SettingsOverlay {
    pub(crate) fn for_viewport(compact: bool) -> Self {
        Self {
            category: SettingsCategory::default(),
            focus: SettingsFocus::Categories,
            compact,
            selected: 0,
            selected_role: "explore".to_owned(),
            inline_editor: None,
            choice_menu: None,
            capturing_shortcut: None,
            notice: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_compact(&self) -> bool {
        self.compact
    }

    pub(crate) fn set_compact(&mut self, compact: bool) {
        if self.compact == compact {
            return;
        }
        self.compact = compact;
        if !compact {
            self.choice_menu = None;
        }
    }

    pub(crate) fn category(&self) -> SettingsCategory {
        self.category
    }

    pub(crate) fn focus(&self) -> SettingsFocus {
        self.focus
    }

    pub(crate) fn selected_index(&self) -> usize {
        self.selected
    }

    pub(crate) fn selected_role(&self) -> &str {
        &self.selected_role
    }

    pub(crate) fn set_selected_role(&mut self, role: String) {
        self.selected_role = role;
    }

    pub(crate) fn choice_menu(&self) -> Option<&SettingsChoiceMenu> {
        self.choice_menu.as_ref()
    }

    pub(crate) fn choice_menu_mut(&mut self) -> Option<&mut SettingsChoiceMenu> {
        self.choice_menu.as_mut()
    }

    pub(crate) fn open_choice_menu(&mut self, item: SettingItem, choices: Vec<SettingChoice>) {
        if choices.is_empty() {
            return;
        }
        self.choice_menu = Some(SettingsChoiceMenu::new(item, choices));
        self.notice = None;
    }

    pub(crate) fn capturing_shortcut(&self) -> Option<KeyAction> {
        self.capturing_shortcut
    }

    pub(crate) fn is_capturing_shortcut(&self) -> bool {
        self.capturing_shortcut.is_some()
    }

    pub(crate) fn selected_item(&self) -> Option<SettingItem> {
        let items = self.category.items();
        if items.is_empty() {
            None
        } else {
            Some(items[self.selected.min(items.len() - 1)])
        }
    }

    pub(crate) fn select_category(&mut self, category: SettingsCategory) {
        if self.category != category {
            self.category = category;
            self.selected = 0;
            self.notice = None;
        }
        self.choice_menu = None;
        self.capturing_shortcut = None;
        self.focus = SettingsFocus::Items;
    }

    pub(crate) fn select_item(&mut self, item: SettingItem) {
        let category = item.category();
        self.category = category;
        self.focus = SettingsFocus::Items;
        self.choice_menu = None;
        self.capturing_shortcut = None;
        if let Some(index) = category
            .items()
            .iter()
            .position(|candidate| *candidate == item)
        {
            self.selected = index;
            self.notice = None;
        }
    }

    pub(crate) fn editor_for(&self, item: SettingItem) -> Option<&TextInput> {
        self.inline_editor
            .as_ref()
            .filter(|editor| editor.item == item)
            .map(|editor| &editor.input)
    }

    pub(crate) fn is_editing(&self) -> bool {
        self.inline_editor.is_some()
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn begin_inline_edit(&mut self, item: SettingItem, value: String) {
        let mut input = TextInput::default();
        input.replace_text(value);
        self.inline_editor = Some(SettingsInlineEditor { item, input });
        self.choice_menu = None;
        self.notice = None;
    }

    pub(crate) fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        if let Some(editor) = self.inline_editor.as_mut() {
            let single_line = crate::text::single_line_paste_stripped(text);
            editor.input.insert_str(&single_line);
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> OverlayKeyResult {
        if let Some(editor) = self.inline_editor.as_mut() {
            return match key.code {
                KeyCode::Esc => {
                    self.inline_editor = None;
                    OverlayKeyResult::Consumed
                }
                KeyCode::Enter => {
                    let item = editor.item;
                    let value = editor.input.text().to_owned();
                    self.inline_editor = None;
                    OverlayKeyResult::CommitSettingEdit { item, value }
                }
                _ => {
                    editor.input.handle_key(key);
                    OverlayKeyResult::Consumed
                }
            };
        }

        if let Some(action) = self.capturing_shortcut.take() {
            if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE {
                return OverlayKeyResult::Consumed;
            }
            return OverlayKeyResult::CaptureShortcut { action, key };
        }

        if let Some(menu) = self.choice_menu.as_mut() {
            return match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Backspace => {
                    self.choice_menu = None;
                    OverlayKeyResult::Consumed
                }
                KeyCode::Down => {
                    let count = menu.choices.len();
                    if count > 0 {
                        menu.selected = (menu.selected + 1).min(count - 1);
                    }
                    OverlayKeyResult::Consumed
                }
                KeyCode::Up => {
                    menu.selected = menu.selected.saturating_sub(1);
                    OverlayKeyResult::Consumed
                }
                KeyCode::Enter => {
                    let item = menu.item;
                    let index = menu.selected;
                    self.choice_menu = None;
                    OverlayKeyResult::SelectSettingChoice { item, index }
                }
                _ => OverlayKeyResult::Consumed,
            };
        }

        match key.code {
            KeyCode::Esc => {
                if self.focus == SettingsFocus::Items {
                    self.focus = SettingsFocus::Categories;
                    self.notice = None;
                    return OverlayKeyResult::Consumed;
                }
                return OverlayKeyResult::Close;
            }
            KeyCode::Char('[') | KeyCode::PageUp => {
                self.category = self.category.step(-1);
                self.selected = 0;
                self.notice = None;
                if self.category.items().is_empty() && !self.compact {
                    self.focus = SettingsFocus::Categories;
                }
                return OverlayKeyResult::Consumed;
            }
            KeyCode::Char(']') | KeyCode::PageDown => {
                self.category = self.category.step(1);
                self.selected = 0;
                self.notice = None;
                return OverlayKeyResult::Consumed;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    SettingsFocus::Categories => SettingsFocus::Items,
                    SettingsFocus::Items => SettingsFocus::Categories,
                };
                self.notice = None;
                return OverlayKeyResult::Consumed;
            }
            _ => {}
        }

        if self.focus == SettingsFocus::Categories {
            return match key.code {
                KeyCode::Down => {
                    self.category = self.category.step(1);
                    self.selected = 0;
                    self.notice = None;
                    OverlayKeyResult::Consumed
                }
                KeyCode::Up => {
                    self.category = self.category.step(-1);
                    self.selected = 0;
                    self.notice = None;
                    OverlayKeyResult::Consumed
                }
                KeyCode::Right | KeyCode::Enter => {
                    self.focus = SettingsFocus::Items;
                    OverlayKeyResult::Consumed
                }
                _ => OverlayKeyResult::Consumed,
            };
        }

        let items = self.category.items();
        if items.is_empty() {
            return OverlayKeyResult::Consumed;
        }
        let selected_item = items[self.selected.min(items.len() - 1)];

        match key.code {
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(items.len() - 1);
                self.notice = None;
                OverlayKeyResult::Consumed
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.notice = None;
                OverlayKeyResult::Consumed
            }
            KeyCode::Left => {
                if selected_item == SettingItem::ManageProviders {
                    OverlayKeyResult::Provider(ProviderOverlayAction::OpenProviderManager)
                } else if selected_item.is_action_only()
                    || matches!(selected_item, SettingItem::Shortcut(_))
                {
                    OverlayKeyResult::Consumed
                } else {
                    OverlayKeyResult::AdjustSetting(selected_item, SettingDirection::Previous)
                }
            }
            KeyCode::Right => {
                if selected_item == SettingItem::ManageProviders {
                    OverlayKeyResult::Provider(ProviderOverlayAction::OpenProviderManager)
                } else if selected_item.is_action_only()
                    || matches!(selected_item, SettingItem::Shortcut(_))
                {
                    OverlayKeyResult::Consumed
                } else {
                    OverlayKeyResult::AdjustSetting(selected_item, SettingDirection::Next)
                }
            }
            KeyCode::Backspace | KeyCode::Delete | KeyCode::Char('r') => {
                if selected_item.is_action_only()
                    || selected_item == SettingItem::SubagentRoleSelector
                {
                    OverlayKeyResult::Consumed
                } else {
                    OverlayKeyResult::ResetSetting(selected_item)
                }
            }
            KeyCode::Enter => match selected_item {
                SettingItem::Shortcut(action) => {
                    self.capturing_shortcut = Some(action);
                    self.notice = None;
                    OverlayKeyResult::Consumed
                }
                item if item.opens_inline_editor() => OverlayKeyResult::BeginSettingEdit(item),
                item if item.opens_model_picker() => OverlayKeyResult::PickSettingModel(item),
                item if item.opens_reasoning_picker() => {
                    OverlayKeyResult::PickSettingReasoning(item)
                }
                SettingItem::ActiveProvider | SettingItem::ManageProviders => {
                    OverlayKeyResult::Provider(ProviderOverlayAction::OpenProviderManager)
                }
                item if self.compact && item.opens_choice_menu() => {
                    OverlayKeyResult::OpenSettingChoices(item)
                }
                item => OverlayKeyResult::AdjustSetting(item, SettingDirection::Next),
            },
            _ => OverlayKeyResult::Consumed,
        }
    }
}
