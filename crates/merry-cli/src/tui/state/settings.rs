use super::TuiState;
use crate::tui::{
    input::TextInput,
    overlay::{Overlay, SettingDirection, SettingItem},
    preferences::{CodeTheme, CompactionStrategy, TuiPreferences, TuiSettingsDefaults},
};

impl TuiState {
    pub(crate) fn configure_preferences(
        &mut self,
        preferences: TuiPreferences,
        defaults: TuiSettingsDefaults,
    ) {
        self.preferences = preferences;
        self.settings_defaults = defaults;
    }

    pub(crate) fn preferences(&self) -> &TuiPreferences {
        &self.preferences
    }

    pub(crate) fn code_theme(&self) -> CodeTheme {
        self.preferences.code_theme
    }

    pub(crate) fn selected_setting(&self) -> Option<SettingItem> {
        match self.overlay.as_ref() {
            Some(Overlay::Settings(settings)) => Some(settings.selected_item()),
            _ => None,
        }
    }

    pub(crate) fn settings_model_editor(&self) -> Option<&TextInput> {
        match self.overlay.as_ref() {
            Some(Overlay::Settings(settings)) => settings.model_editor(),
            _ => None,
        }
    }

    pub(crate) fn settings_notice(&self) -> Option<&str> {
        match self.overlay.as_ref() {
            Some(Overlay::Settings(settings)) => settings.notice(),
            _ => None,
        }
    }

    pub(crate) fn begin_settings_model_edit(&mut self) {
        let provider = self.effective_provider_alias().map(str::to_owned);
        let value = provider
            .as_deref()
            .and_then(|provider| self.preferences.model_for_provider(provider))
            .map(str::to_owned)
            .or_else(|| {
                self.inherited_model_for_provider(provider.as_deref())
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        if let Some(Overlay::Settings(settings)) = self.overlay.as_mut() {
            settings.begin_model_edit(value);
        }
    }

    pub(crate) fn commit_settings_model(&mut self, value: String) -> bool {
        let Some(provider) = self.effective_provider_alias().map(str::to_owned) else {
            self.set_settings_notice(Some("Select a provider first".to_owned()));
            return false;
        };
        let value = value.trim();
        let result = if value.is_empty() {
            self.preferences.set_model_for_provider(&provider, None)
        } else {
            self.preferences
                .set_model_for_provider(&provider, Some(value))
        };
        match result {
            Ok(()) => {
                self.set_settings_notice(Some("Applied".to_owned()));
                true
            }
            Err(error) => {
                self.set_settings_notice(Some(error.to_string()));
                false
            }
        }
    }

    pub(crate) fn adjust_setting(
        &mut self,
        item: SettingItem,
        direction: SettingDirection,
    ) -> bool {
        match item {
            SettingItem::CodeTheme => {
                self.preferences.code_theme = match direction {
                    SettingDirection::Previous => self.preferences.code_theme.previous(),
                    SettingDirection::Next => self.preferences.code_theme.next(),
                };
            }
            SettingItem::DefaultProvider => self.adjust_provider(direction),
            SettingItem::DefaultModel | SettingItem::KeyboardShortcuts => return false,
            SettingItem::ReasoningEffort => self.adjust_reasoning(direction),
            SettingItem::AutoCompaction => self.adjust_auto_compaction(direction),
            SettingItem::ContextStrategy => self.adjust_compaction_strategy(direction),
            SettingItem::Subagents => self.adjust_subagents(direction),
            SettingItem::MaxThreads => self.adjust_max_threads(direction),
        }
        self.set_settings_notice(Some("Applied".to_owned()));
        true
    }

    pub(crate) fn reset_setting(&mut self, item: SettingItem) -> bool {
        match item {
            SettingItem::CodeTheme => self.preferences.code_theme = CodeTheme::default(),
            SettingItem::DefaultProvider => self.preferences.provider = None,
            SettingItem::DefaultModel => {
                if let Some(provider) = self.effective_provider_alias().map(str::to_owned) {
                    let _ = self.preferences.set_model_for_provider(&provider, None);
                }
            }
            SettingItem::ReasoningEffort => self.preferences.reasoning_effort = None,
            SettingItem::AutoCompaction => self.preferences.auto_compaction_enabled = None,
            SettingItem::ContextStrategy => self.preferences.compaction_strategy = None,
            SettingItem::Subagents => self.preferences.subagents_enabled = None,
            SettingItem::MaxThreads => self.preferences.subagent_max_threads = None,
            SettingItem::KeyboardShortcuts => return false,
        }
        self.set_settings_notice(Some("Reset to inherited value".to_owned()));
        true
    }

    pub(crate) fn setting_value(&self, item: SettingItem) -> String {
        match item {
            SettingItem::CodeTheme => self.preferences.code_theme.label().to_owned(),
            SettingItem::DefaultProvider => inherited_value(
                self.preferences.provider.as_deref(),
                self.settings_defaults.provider.as_deref(),
            ),
            SettingItem::DefaultModel => {
                let provider = self.effective_provider_alias();
                inherited_value(
                    provider.and_then(|provider| self.preferences.model_for_provider(provider)),
                    self.inherited_model_for_provider(provider),
                )
            }
            SettingItem::ReasoningEffort => inherited_value(
                self.preferences.reasoning_label(),
                self.settings_defaults
                    .reasoning_effort
                    .as_ref()
                    .map(merry_llm::ReasoningEffort::as_str),
            ),
            SettingItem::AutoCompaction => self
                .preferences
                .auto_compaction_enabled
                .map(on_off)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({})",
                        on_off(self.settings_defaults.auto_compaction_enabled)
                    )
                }),
            SettingItem::ContextStrategy => self
                .preferences
                .compaction_strategy
                .map(CompactionStrategy::label)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!("Inherit ({})", self.settings_defaults.compaction_strategy)
                }),
            SettingItem::Subagents => self
                .preferences
                .subagents_enabled
                .map(on_off)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({})",
                        on_off(self.settings_defaults.subagents_enabled)
                    )
                }),
            SettingItem::MaxThreads => self
                .preferences
                .subagent_max_threads
                .map(|value| value.to_string())
                .unwrap_or_else(|| {
                    format!("Inherit ({})", self.settings_defaults.subagent_max_threads)
                }),
            SettingItem::KeyboardShortcuts => "Open".to_owned(),
        }
    }

    fn adjust_provider(&mut self, direction: SettingDirection) {
        let choices = &self.settings_defaults.provider_aliases;
        if choices.is_empty() {
            return;
        }
        let current = self
            .preferences
            .provider
            .as_ref()
            .and_then(|provider| choices.iter().position(|choice| choice == provider))
            .map(|index| index + 1)
            .unwrap_or(0);
        let count = choices.len() + 1;
        let next = match direction {
            SettingDirection::Previous => (current + count - 1) % count,
            SettingDirection::Next => (current + 1) % count,
        };
        self.preferences.provider = (next > 0).then(|| choices[next - 1].clone());
    }

    fn effective_provider_alias(&self) -> Option<&str> {
        self.preferences
            .provider
            .as_deref()
            .or(self.settings_defaults.provider.as_deref())
    }

    fn inherited_model_for_provider(&self, provider: Option<&str>) -> Option<&str> {
        (provider == self.settings_defaults.provider.as_deref())
            .then_some(self.settings_defaults.model.as_deref())
            .flatten()
    }

    fn adjust_reasoning(&mut self, direction: SettingDirection) {
        const VALUES: [&str; 5] = ["minimal", "low", "medium", "high", "xhigh"];
        let current = self
            .preferences
            .reasoning_label()
            .and_then(|effort| VALUES.iter().position(|value| *value == effort))
            .map(|index| index + 1)
            .unwrap_or(0);
        let count = VALUES.len() + 1;
        let next = match direction {
            SettingDirection::Previous => (current + count - 1) % count,
            SettingDirection::Next => (current + 1) % count,
        };
        self.preferences.reasoning_effort = (next > 0).then(|| {
            merry_llm::ReasoningEffort::new(VALUES[next - 1])
                .expect("static reasoning effort should validate")
        });
    }

    fn adjust_auto_compaction(&mut self, direction: SettingDirection) {
        const VALUES: [Option<bool>; 3] = [None, Some(true), Some(false)];
        let current = VALUES
            .iter()
            .position(|value| *value == self.preferences.auto_compaction_enabled)
            .unwrap_or(0);
        let next = match direction {
            SettingDirection::Previous => (current + VALUES.len() - 1) % VALUES.len(),
            SettingDirection::Next => (current + 1) % VALUES.len(),
        };
        self.preferences.auto_compaction_enabled = VALUES[next];
    }

    fn adjust_compaction_strategy(&mut self, direction: SettingDirection) {
        const VALUES: [Option<CompactionStrategy>; 4] = [
            None,
            Some(CompactionStrategy::Compact),
            Some(CompactionStrategy::Balanced),
            Some(CompactionStrategy::PreserveDetail),
        ];
        let current = VALUES
            .iter()
            .position(|value| *value == self.preferences.compaction_strategy)
            .unwrap_or(0);
        let next = match direction {
            SettingDirection::Previous => (current + VALUES.len() - 1) % VALUES.len(),
            SettingDirection::Next => (current + 1) % VALUES.len(),
        };
        self.preferences.compaction_strategy = VALUES[next];
    }

    fn adjust_subagents(&mut self, direction: SettingDirection) {
        const VALUES: [Option<bool>; 3] = [None, Some(true), Some(false)];
        let current = VALUES
            .iter()
            .position(|value| *value == self.preferences.subagents_enabled)
            .unwrap_or(0);
        let next = match direction {
            SettingDirection::Previous => (current + VALUES.len() - 1) % VALUES.len(),
            SettingDirection::Next => (current + 1) % VALUES.len(),
        };
        self.preferences.subagents_enabled = VALUES[next];
    }

    fn adjust_max_threads(&mut self, direction: SettingDirection) {
        let effective = self
            .preferences
            .subagent_max_threads
            .unwrap_or(self.settings_defaults.subagent_max_threads);
        self.preferences.subagent_max_threads = Some(match direction {
            SettingDirection::Previous => effective.saturating_sub(1).max(1),
            SettingDirection::Next => effective.saturating_add(1),
        });
    }

    fn set_settings_notice(&mut self, notice: Option<String>) {
        if let Some(Overlay::Settings(settings)) = self.overlay.as_mut() {
            settings.set_notice(notice);
        }
    }
}

fn inherited_value(value: Option<&str>, inherited: Option<&str>) -> String {
    value.map(str::to_owned).unwrap_or_else(|| {
        inherited
            .map(|value| format!("Inherit ({value})"))
            .unwrap_or_else(|| "Inherit".to_owned())
    })
}

fn on_off(value: bool) -> &'static str {
    if value { "On" } else { "Off" }
}
