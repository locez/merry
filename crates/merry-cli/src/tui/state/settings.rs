mod choices;
mod permissions;
mod roles;

use super::TuiState;
use crate::{
    config::{
        BrandMarkMode, ConfiguredProviderKind, CopyButtonPosition, HeaderItemKind,
        MAX_MAX_READING_WIDTH, MAX_MOUSE_SCROLL_LINES, MIN_MAX_READING_WIDTH, TextSizingMode,
    },
    tui::{
        input::TextInput,
        keymap::{
            KeyAction, KeyBinding, alt_modifier_name, normalize_command_key, parse_binding_str,
        },
        overlay::{Overlay, SettingDirection, SettingItem},
        preferences::{
            CodeTheme, CompactionStrategy, SubagentLimitBehaviorPref, TuiPreferences,
            TuiSettingsDefaults,
        },
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_llm::ServiceTier;

pub(super) const BRAND_MARKS: [Option<BrandMarkMode>; 3] = [
    None,
    Some(BrandMarkMode::Emoji),
    Some(BrandMarkMode::Snowflake),
];
pub(super) const COPY_BUTTON_POSITIONS: [Option<CopyButtonPosition>; 5] = [
    None,
    Some(CopyButtonPosition::TopLeft),
    Some(CopyButtonPosition::TopRight),
    Some(CopyButtonPosition::BottomLeft),
    Some(CopyButtonPosition::BottomRight),
];
pub(super) const TEXT_SIZINGS: [Option<TextSizingMode>; 4] = [
    None,
    Some(TextSizingMode::Off),
    Some(TextSizingMode::Auto),
    Some(TextSizingMode::On),
];
pub(super) const COMPACTION_STRATEGIES: [Option<CompactionStrategy>; 4] = [
    None,
    Some(CompactionStrategy::Compact),
    Some(CompactionStrategy::Balanced),
    Some(CompactionStrategy::PreserveDetail),
];
pub(super) const SUBAGENT_LIMIT_BEHAVIORS: [Option<SubagentLimitBehaviorPref>; 3] = [
    None,
    Some(SubagentLimitBehaviorPref::Queue),
    Some(SubagentLimitBehaviorPref::Fail),
];

impl TuiState {
    pub(crate) fn configure_preferences(
        &mut self,
        preferences: TuiPreferences,
        defaults: TuiSettingsDefaults,
    ) {
        self.preferences = preferences;
        self.settings_defaults = defaults;
        self.sync_active_ui_preferences();
    }

    pub(crate) fn preferences(&self) -> &TuiPreferences {
        &self.preferences
    }

    pub(crate) fn code_theme(&self) -> CodeTheme {
        self.preferences.code_theme
    }

    #[cfg(test)]
    pub(crate) fn selected_setting(&self) -> Option<SettingItem> {
        match self.overlay() {
            Some(Overlay::Settings(settings)) => settings.selected_item(),
            _ => None,
        }
    }

    pub(crate) fn settings_editor_open(&self) -> bool {
        match self.overlay() {
            Some(Overlay::Settings(settings)) => {
                settings.is_editing() || settings.is_capturing_shortcut()
            }
            _ => false,
        }
    }

    pub(crate) fn settings_editor_for(&self, item: SettingItem) -> Option<&TextInput> {
        match self.overlay() {
            Some(Overlay::Settings(settings)) => settings.editor_for(item),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn settings_notice(&self) -> Option<&str> {
        match self.overlay() {
            Some(Overlay::Settings(settings)) => settings.notice(),
            _ => None,
        }
    }

    pub(crate) fn begin_settings_inline_edit(&mut self, item: SettingItem) {
        let value = match item {
            SettingItem::ContextWindow => self
                .preferences
                .context_window_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_default(),
            SettingItem::MaxReadingWidth => self
                .preferences
                .max_reading_width
                .unwrap_or(self.settings_defaults.max_reading_width)
                .to_string(),
            _ => return,
        };
        if let Some(Overlay::Settings(settings)) = self.overlay_mut() {
            settings.begin_inline_edit(item, value);
        }
    }

    pub(crate) fn commit_settings_inline_edit(&mut self, item: SettingItem, value: String) -> bool {
        match item {
            SettingItem::ContextWindow => self.commit_settings_context_window(value),
            SettingItem::MaxReadingWidth => self.commit_settings_reading_width(value),
            _ => false,
        }
    }

    fn commit_settings_context_window(&mut self, value: String) -> bool {
        match parse_context_window_tokens(&value) {
            Ok(tokens) => {
                self.preferences.context_window_tokens = Some(tokens);
                self.set_settings_notice(Some("Applied".to_owned()));
                true
            }
            Err(error) => {
                self.set_settings_notice(Some(error));
                false
            }
        }
    }

    fn commit_settings_reading_width(&mut self, value: String) -> bool {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            self.preferences.max_reading_width = None;
            self.sync_active_ui_preferences();
            self.set_settings_notice(Some("Reset to inherited value".to_owned()));
            return true;
        }
        let Ok(width) = trimmed.parse::<u16>() else {
            self.set_settings_notice(Some(format!(
                "Reading width must be 0 (unlimited) or {MIN_MAX_READING_WIDTH}..={MAX_MAX_READING_WIDTH}"
            )));
            return false;
        };
        if width != 0 && !(MIN_MAX_READING_WIDTH..=MAX_MAX_READING_WIDTH).contains(&width) {
            self.set_settings_notice(Some(format!(
                "Reading width must be 0 (unlimited) or {MIN_MAX_READING_WIDTH}..={MAX_MAX_READING_WIDTH}"
            )));
            return false;
        }
        self.preferences.max_reading_width = Some(width);
        self.sync_active_ui_preferences();
        self.set_settings_notice(Some("Applied".to_owned()));
        true
    }

    pub(crate) fn adjust_setting(
        &mut self,
        item: SettingItem,
        direction: SettingDirection,
    ) -> bool {
        if item.is_action_only() {
            return false;
        }
        let changed = match item {
            SettingItem::CodeTheme => {
                self.preferences.code_theme = match direction {
                    SettingDirection::Previous => self.preferences.code_theme.previous(),
                    SettingDirection::Next => self.preferences.code_theme.next(),
                };
                true
            }
            SettingItem::UiTheme => {
                let current = self
                    .preferences
                    .theme_preset
                    .unwrap_or(self.settings_defaults.theme_preset);
                let next = match direction {
                    SettingDirection::Previous => current.previous(),
                    SettingDirection::Next => current.next(),
                };
                self.preferences.theme_preset = Some(next);
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::TransparentBackground => {
                cycle_opt_bool(&mut self.preferences.transparent_background, direction);
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::Animations => {
                cycle_opt_bool(&mut self.preferences.animations, direction);
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::BrandMark => {
                cycle_slice(&mut self.preferences.brand_mark, &BRAND_MARKS, direction);
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::MaxReadingWidth => {
                const VALUES: [Option<u16>; 8] = [
                    None,
                    Some(0),
                    Some(80),
                    Some(96),
                    Some(104),
                    Some(120),
                    Some(140),
                    Some(160),
                ];
                cycle_slice(&mut self.preferences.max_reading_width, &VALUES, direction);
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::TextSizing => {
                cycle_slice(&mut self.preferences.text_sizing, &TEXT_SIZINGS, direction);
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::HeaderItem(kind) => {
                self.toggle_header_item(kind);
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::MouseScrollLines => {
                let effective = self
                    .preferences
                    .mouse_scroll_lines
                    .unwrap_or(self.settings_defaults.mouse_scroll_lines);
                self.preferences.mouse_scroll_lines = Some(match direction {
                    SettingDirection::Previous => effective.saturating_sub(1).max(1),
                    SettingDirection::Next => {
                        effective.saturating_add(1).min(MAX_MOUSE_SCROLL_LINES)
                    }
                });
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::ShowSuccessfulCommandOutput => {
                cycle_opt_bool(
                    &mut self.preferences.show_successful_command_output,
                    direction,
                );
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::CopyButtonPosition => {
                cycle_slice(
                    &mut self.preferences.copy_button_position,
                    &COPY_BUTTON_POSITIONS,
                    direction,
                );
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::ActiveProvider => self.adjust_provider(direction),
            SettingItem::ActiveModel => false,
            SettingItem::ReasoningEffort => self.adjust_reasoning(direction),
            SettingItem::ServiceTier => self.adjust_service_tier(direction),
            SettingItem::ContextWindow => {
                const VALUES: [Option<u64>; 7] = [
                    None,
                    Some(64_000),
                    Some(128_000),
                    Some(256_000),
                    Some(272_000),
                    Some(372_000),
                    Some(1_000_000),
                ];
                cycle_slice(
                    &mut self.preferences.context_window_tokens,
                    &VALUES,
                    direction,
                );
                true
            }
            SettingItem::AutoCompaction => {
                cycle_opt_bool(&mut self.preferences.auto_compaction_enabled, direction);
                true
            }
            SettingItem::ContextStrategy => {
                cycle_slice(
                    &mut self.preferences.compaction_strategy,
                    &COMPACTION_STRATEGIES,
                    direction,
                );
                true
            }
            SettingItem::RetainedModelTurns => {
                const VALUES: [Option<usize>; 11] = [
                    None,
                    Some(1),
                    Some(2),
                    Some(3),
                    Some(4),
                    Some(5),
                    Some(6),
                    Some(7),
                    Some(8),
                    Some(9),
                    Some(10),
                ];
                cycle_slice(
                    &mut self.preferences.retained_model_turns,
                    &VALUES,
                    direction,
                );
                true
            }
            SettingItem::Subagents => {
                cycle_opt_bool(&mut self.preferences.subagents_enabled, direction);
                true
            }
            SettingItem::MaxThreads => {
                let effective = self
                    .preferences
                    .subagent_max_threads
                    .unwrap_or(self.settings_defaults.subagent_max_threads);
                self.preferences.subagent_max_threads = Some(match direction {
                    SettingDirection::Previous => effective.saturating_sub(1).max(1),
                    SettingDirection::Next => effective.saturating_add(1),
                });
                true
            }
            SettingItem::SubagentLimitBehavior => {
                cycle_slice(
                    &mut self.preferences.subagent_limit_behavior,
                    &SUBAGENT_LIMIT_BEHAVIORS,
                    direction,
                );
                true
            }
            SettingItem::MaxParallelToolCalls => {
                let effective = self
                    .preferences
                    .max_parallel_tool_calls
                    .unwrap_or(self.settings_defaults.max_parallel_tool_calls);
                self.preferences.max_parallel_tool_calls = Some(match direction {
                    SettingDirection::Previous => effective.saturating_sub(1).max(1),
                    SettingDirection::Next => effective.saturating_add(1).min(32),
                });
                true
            }
            item if self.adjust_role_setting(item, direction) => true,
            item if self.adjust_permission_setting(item, direction) => true,
            _ => false,
        };
        if changed {
            let notice = if item.is_next_session() {
                "Saved (applies on next session)"
            } else {
                "Applied"
            };
            self.set_settings_notice(Some(notice.to_owned()));
        }
        changed
    }

    pub(crate) fn reset_setting(&mut self, item: SettingItem) -> bool {
        if item.is_action_only() {
            return false;
        }
        let reset = match item {
            SettingItem::CodeTheme => {
                self.preferences.code_theme = CodeTheme::default();
                true
            }
            SettingItem::UiTheme => {
                self.preferences.theme_preset = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::TransparentBackground => {
                self.preferences.transparent_background = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::Animations => {
                self.preferences.animations = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::BrandMark => {
                self.preferences.brand_mark = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::MaxReadingWidth => {
                self.preferences.max_reading_width = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::TextSizing => {
                self.preferences.text_sizing = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::HeaderItem(_) => {
                self.preferences.header_items = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::MouseScrollLines => {
                self.preferences.mouse_scroll_lines = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::ShowSuccessfulCommandOutput => {
                self.preferences.show_successful_command_output = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::CopyButtonPosition => {
                self.preferences.copy_button_position = None;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::ActiveProvider => {
                self.preferences.provider = None;
                true
            }
            SettingItem::ActiveModel => {
                if let Some(provider) = self.effective_provider_alias().map(str::to_owned) {
                    let _ = self.preferences.set_model_for_provider(&provider, None);
                }
                true
            }
            SettingItem::ReasoningEffort => {
                if let Some(provider) = self.effective_provider_alias().map(str::to_owned) {
                    let _ = self
                        .preferences
                        .set_reasoning_effort_for_provider(&provider, None);
                }
                true
            }
            SettingItem::ServiceTier => {
                if let Some(provider) = self.effective_provider_alias().map(str::to_owned) {
                    let _ = self
                        .preferences
                        .set_service_tier_for_provider(&provider, None);
                }
                true
            }
            SettingItem::ContextWindow => {
                self.preferences.context_window_tokens = None;
                true
            }
            SettingItem::AutoCompaction => {
                self.preferences.auto_compaction_enabled = None;
                true
            }
            SettingItem::ContextStrategy => {
                self.preferences.compaction_strategy = None;
                true
            }
            SettingItem::RetainedModelTurns => {
                self.preferences.retained_model_turns = None;
                true
            }
            SettingItem::Subagents => {
                self.preferences.subagents_enabled = None;
                true
            }
            SettingItem::MaxThreads => {
                self.preferences.subagent_max_threads = None;
                true
            }
            SettingItem::SubagentLimitBehavior => {
                self.preferences.subagent_limit_behavior = None;
                true
            }
            SettingItem::MaxParallelToolCalls => {
                self.preferences.max_parallel_tool_calls = None;
                true
            }
            SettingItem::Shortcut(action) => {
                let Some(config_key) = action.config_key() else {
                    return false;
                };
                self.preferences.keymap.remove(config_key);
                self.sync_active_ui_preferences();
                true
            }
            item if self.reset_role_setting(item) => true,
            item if self.reset_permission_setting(item) => true,
            _ => false,
        };
        if reset {
            self.set_settings_notice(Some("Reset to inherited value".to_owned()));
        }
        reset
    }

    pub(crate) fn apply_shortcut_capture(&mut self, action: KeyAction, key: KeyEvent) -> bool {
        let normalized = normalize_command_key(key);
        let is_reserved_palette = matches!(normalized.code, KeyCode::Char('x' | 'X'))
            && normalized.modifiers.contains(KeyModifiers::ALT)
            && !normalized.modifiers.contains(KeyModifiers::CONTROL)
            && !normalized.modifiers.contains(KeyModifiers::SUPER);
        if is_reserved_palette {
            self.set_settings_notice(Some(format!(
                "{}+X is reserved for Command palette",
                alt_modifier_name()
            )));
            return false;
        }
        let Some(canonical) = KeyBinding::canonical_from_key_event(normalized) else {
            self.set_settings_notice(Some(
                "Unsupported key (use Ctrl+<key>, Alt+<key>, Enter, Alt+Enter, Up/Down, or PageUp/PageDown)"
                    .to_owned(),
            ));
            return false;
        };
        let Some(config_key) = action.config_key() else {
            return false;
        };
        if let Ok(new_binding) = parse_binding_str(&canonical) {
            self.preferences
                .keymap
                .retain(|_, existing| parse_binding_str(existing).ok() != Some(new_binding));
        }
        self.preferences
            .keymap
            .insert(config_key.to_owned(), canonical.clone());
        self.sync_active_ui_preferences();
        let label = self.keymap.binding_label_for(action).unwrap_or(canonical);
        self.set_settings_notice(Some(format!("Bound to {label}")));
        true
    }

    pub(crate) fn is_setting_overridden(&self, item: SettingItem) -> bool {
        let provider = self.effective_provider_alias();
        match item {
            SettingItem::CodeTheme => self.preferences.code_theme != CodeTheme::default(),
            SettingItem::UiTheme => self.preferences.theme_preset.is_some(),
            SettingItem::TransparentBackground => self.preferences.transparent_background.is_some(),
            SettingItem::Animations => self.preferences.animations.is_some(),
            SettingItem::BrandMark => self.preferences.brand_mark.is_some(),
            SettingItem::MaxReadingWidth => self.preferences.max_reading_width.is_some(),
            SettingItem::TextSizing => self.preferences.text_sizing.is_some(),
            SettingItem::HeaderItem(_) => self.preferences.header_items.is_some(),
            SettingItem::MouseScrollLines => self.preferences.mouse_scroll_lines.is_some(),
            SettingItem::ShowSuccessfulCommandOutput => {
                self.preferences.show_successful_command_output.is_some()
            }
            SettingItem::CopyButtonPosition => self.preferences.copy_button_position.is_some(),
            SettingItem::ActiveProvider => self.preferences.provider.is_some(),
            SettingItem::ActiveModel => {
                provider.is_some_and(|p| self.preferences.model_for_provider(p).is_some())
            }
            SettingItem::ReasoningEffort => provider
                .is_some_and(|p| self.preferences.reasoning_effort_for_provider(p).is_some()),
            SettingItem::ServiceTier => {
                provider.is_some_and(|p| self.preferences.service_tier_for_provider(p).is_some())
            }
            SettingItem::ContextWindow => self.preferences.context_window_tokens.is_some(),
            SettingItem::AutoCompaction => self.preferences.auto_compaction_enabled.is_some(),
            SettingItem::ContextStrategy => self.preferences.compaction_strategy.is_some(),
            SettingItem::RetainedModelTurns => self.preferences.retained_model_turns.is_some(),
            SettingItem::Subagents => self.preferences.subagents_enabled.is_some(),
            SettingItem::MaxThreads => self.preferences.subagent_max_threads.is_some(),
            SettingItem::SubagentLimitBehavior => {
                self.preferences.subagent_limit_behavior.is_some()
            }
            SettingItem::MaxParallelToolCalls => self.preferences.max_parallel_tool_calls.is_some(),
            SettingItem::Shortcut(action) => action
                .config_key()
                .is_some_and(|key| self.preferences.keymap.contains_key(key)),
            item if self.is_role_setting_overridden(item) => true,
            item if self.is_permission_setting_overridden(item) => true,
            _ => false,
        }
    }

    pub(crate) fn setting_value(&self, item: SettingItem) -> String {
        match item {
            SettingItem::CodeTheme => self.preferences.code_theme.label().to_owned(),
            SettingItem::UiTheme => self
                .preferences
                .theme_preset
                .map(|preset| preset.label().to_owned())
                .unwrap_or_else(|| {
                    format!("Inherit ({})", self.settings_defaults.theme_preset.label())
                }),
            SettingItem::TransparentBackground => opt_bool_value(
                self.preferences.transparent_background,
                self.settings_defaults.transparent_background,
            ),
            SettingItem::Animations => opt_bool_value(
                self.preferences.animations,
                self.settings_defaults.animations,
            ),
            SettingItem::BrandMark => self
                .preferences
                .brand_mark
                .map(brand_mark_label)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({})",
                        brand_mark_label(self.settings_defaults.brand_mark)
                    )
                }),
            SettingItem::MaxReadingWidth => self
                .preferences
                .max_reading_width
                .map(format_reading_width)
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({})",
                        format_reading_width(self.settings_defaults.max_reading_width)
                    )
                }),
            SettingItem::TextSizing => self
                .preferences
                .text_sizing
                .map(text_sizing_label)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({})",
                        text_sizing_label(self.settings_defaults.text_sizing)
                    )
                }),
            SettingItem::HeaderItem(kind) => {
                let shown = self.header_items.contains(&kind);
                if self.preferences.header_items.is_some() {
                    if shown { "Shown" } else { "Hidden" }.to_owned()
                } else {
                    format!("Inherit ({})", if shown { "Shown" } else { "Hidden" })
                }
            }
            SettingItem::MouseScrollLines => self
                .preferences
                .mouse_scroll_lines
                .map(|lines| format!("{lines} lines"))
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({} lines)",
                        self.settings_defaults.mouse_scroll_lines
                    )
                }),
            SettingItem::ShowSuccessfulCommandOutput => opt_bool_value(
                self.preferences.show_successful_command_output,
                self.settings_defaults.show_successful_command_output,
            ),
            SettingItem::CopyButtonPosition => self.preferences.copy_button_position.map_or_else(
                || {
                    format!(
                        "Inherit ({})",
                        self.settings_defaults.copy_button_position.label()
                    )
                },
                |position| position.label().to_owned(),
            ),
            SettingItem::ActiveProvider => inherited_value(
                self.preferences.provider.as_deref(),
                self.settings_defaults.provider.as_deref(),
            ),
            SettingItem::ActiveModel => {
                let provider = self.effective_provider_alias();
                let value = inherited_value(
                    provider.and_then(|provider| self.preferences.model_for_provider(provider)),
                    self.settings_defaults.default_model_for_provider(provider),
                );
                format!("{value} ▸")
            }
            SettingItem::ReasoningEffort => {
                let value = inherited_value(
                    self.effective_provider_alias().and_then(|provider| {
                        self.preferences.reasoning_label_for_provider(provider)
                    }),
                    self.settings_defaults
                        .reasoning_effort_for_provider(self.effective_provider_alias())
                        .map(merry_llm::ReasoningEffort::as_str),
                );
                format!("{value} ▸")
            }
            SettingItem::ServiceTier => {
                let provider = self.effective_provider_alias();
                if self.settings_defaults.provider_kind(provider)
                    == Some(ConfiguredProviderKind::Anthropic)
                {
                    "N/A (Anthropic)".to_owned()
                } else {
                    inherited_value(
                        provider
                            .and_then(|p| self.preferences.service_tier_for_provider(p))
                            .map(ServiceTier::as_str),
                        self.settings_defaults
                            .service_tier_for_provider(provider)
                            .map(ServiceTier::as_str),
                    )
                }
            }
            SettingItem::ManageProviders => "Manage ▸".to_owned(),
            SettingItem::ContextWindow => self
                .preferences
                .context_window_tokens
                .map(format_context_window_tokens)
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({} fallback)",
                        format_context_window_tokens(self.settings_defaults.context_window_tokens)
                    )
                }),
            SettingItem::AutoCompaction => opt_bool_value(
                self.preferences.auto_compaction_enabled,
                self.settings_defaults.auto_compaction_enabled,
            ),
            SettingItem::ContextStrategy => self
                .preferences
                .compaction_strategy
                .map(CompactionStrategy::label)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!("Inherit ({})", self.settings_defaults.compaction_strategy)
                }),
            SettingItem::RetainedModelTurns => self
                .preferences
                .retained_model_turns
                .map(|turns| format!("{turns} turns"))
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({} turns)",
                        self.settings_defaults.retained_model_turns
                    )
                }),
            SettingItem::Subagents => opt_bool_value(
                self.preferences.subagents_enabled,
                self.settings_defaults.subagents_enabled,
            ),
            SettingItem::MaxThreads => self
                .preferences
                .subagent_max_threads
                .map(|value| value.to_string())
                .unwrap_or_else(|| {
                    format!("Inherit ({})", self.settings_defaults.subagent_max_threads)
                }),
            SettingItem::SubagentLimitBehavior => self
                .preferences
                .subagent_limit_behavior
                .map(SubagentLimitBehaviorPref::label)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({})",
                        self.settings_defaults.subagent_limit_behavior.label()
                    )
                }),
            SettingItem::MaxParallelToolCalls => self
                .preferences
                .max_parallel_tool_calls
                .map(|value| value.to_string())
                .unwrap_or_else(|| {
                    format!(
                        "Inherit ({})",
                        self.settings_defaults.max_parallel_tool_calls
                    )
                }),
            SettingItem::CompactionProvider
            | SettingItem::CompactionModel
            | SettingItem::CompactionReasoning
            | SettingItem::ApprovalReviewProvider
            | SettingItem::ApprovalReviewModel
            | SettingItem::ApprovalReviewReasoning
            | SettingItem::SubagentDefaultProvider
            | SettingItem::SubagentDefaultModel
            | SettingItem::SubagentDefaultReasoning
            | SettingItem::SubagentRoleSelector
            | SettingItem::SubagentRoleEnabled
            | SettingItem::SubagentRoleProvider
            | SettingItem::SubagentRoleModel
            | SettingItem::SubagentRoleReasoning
            | SettingItem::SubagentRoleMaxTurns
            | SettingItem::SubagentRoleTimeout => self.role_setting_value(item),
            SettingItem::PermissionSandbox
            | SettingItem::PermissionMode
            | SettingItem::PermissionAutoFallback
            | SettingItem::PermissionNetwork
            | SettingItem::PermissionSshAgent
            | SettingItem::PermissionGpgAgent
            | SettingItem::PermissionDbus => self.permission_setting_value(item),
            SettingItem::Shortcut(action) => self
                .keymap
                .binding_label_for(action)
                .unwrap_or_else(|| "Unbound".to_owned()),
        }
    }

    fn adjust_provider(&mut self, direction: SettingDirection) -> bool {
        let Some(next) = self.cycle_provider_alias(self.preferences.provider.as_deref(), direction)
        else {
            return false;
        };
        self.preferences.provider = next;
        true
    }

    pub(super) fn effective_provider_alias(&self) -> Option<&str> {
        self.preferences
            .provider
            .as_deref()
            .or(self.settings_defaults.provider.as_deref())
    }

    fn adjust_reasoning(&mut self, direction: SettingDirection) -> bool {
        let Some(provider) = self.effective_provider_alias().map(str::to_owned) else {
            return false;
        };
        let reasoning_effort = roles::cycle_reasoning_preset(
            self.preferences.reasoning_label_for_provider(&provider),
            direction,
        );
        let _ = self
            .preferences
            .set_reasoning_effort_for_provider(&provider, reasoning_effort);
        true
    }

    pub(super) fn service_tier_choices(&self, provider: &str) -> Vec<Option<ServiceTier>> {
        let protocol = self
            .settings_defaults
            .provider_protocol(Some(provider))
            .unwrap_or_default();
        let mut choices = vec![None];
        for tier in ServiceTier::ALL {
            if protocol.supports_service_tier(tier) {
                choices.push(Some(tier));
            }
        }
        choices
    }

    fn adjust_service_tier(&mut self, direction: SettingDirection) -> bool {
        let Some(provider) = self.effective_provider_alias().map(str::to_owned) else {
            return false;
        };
        if self.settings_defaults.provider_kind(Some(&provider))
            == Some(ConfiguredProviderKind::Anthropic)
        {
            self.set_settings_notice(Some(
                "Service tier is only supported for OpenAI-compatible providers".to_owned(),
            ));
            return false;
        }
        let choices = self.service_tier_choices(&provider);
        let current = choices
            .iter()
            .position(|choice| *choice == self.preferences.service_tier_for_provider(&provider))
            .unwrap_or(0);
        let len = choices.len();
        let next = match direction {
            SettingDirection::Previous => (current + len - 1) % len,
            SettingDirection::Next => (current + 1) % len,
        };
        let _ = self
            .preferences
            .set_service_tier_for_provider(&provider, choices[next]);
        true
    }

    fn toggle_header_item(&mut self, kind: HeaderItemKind) {
        let mut items = self
            .preferences
            .header_items
            .clone()
            .unwrap_or_else(|| self.settings_defaults.header_items.clone());
        if let Some(pos) = items.iter().position(|existing| *existing == kind) {
            items.remove(pos);
        } else {
            items.push(kind);
            items.sort_by_key(|item| {
                HeaderItemKind::ALL
                    .iter()
                    .position(|candidate| candidate == item)
                    .unwrap_or(usize::MAX)
            });
        }
        self.preferences.header_items = Some(items);
    }

    pub(crate) fn set_settings_notice(&mut self, notice: Option<String>) {
        if let Some(Overlay::Settings(settings)) = self.overlay_mut() {
            settings.set_notice(notice);
        }
    }
}

pub(super) fn cycle_slice<T: Copy + PartialEq>(
    slot: &mut T,
    values: &[T],
    direction: SettingDirection,
) {
    let current = values.iter().position(|value| *value == *slot).unwrap_or(0);
    let next = match direction {
        SettingDirection::Previous => (current + values.len() - 1) % values.len(),
        SettingDirection::Next => (current + 1) % values.len(),
    };
    *slot = values[next];
}

pub(super) fn cycle_opt_bool(slot: &mut Option<bool>, direction: SettingDirection) {
    const VALUES: [Option<bool>; 3] = [None, Some(true), Some(false)];
    cycle_slice(slot, &VALUES, direction);
}

pub(super) fn opt_bool_value(value: Option<bool>, inherited: bool) -> String {
    value
        .map(on_off)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Inherit ({})", on_off(inherited)))
}

pub(super) fn inherited_value(value: Option<&str>, inherited: Option<&str>) -> String {
    value.map(str::to_owned).unwrap_or_else(|| {
        inherited
            .map(|value| format!("Inherit ({value})"))
            .unwrap_or_else(|| "Inherit".to_owned())
    })
}

fn on_off(value: bool) -> &'static str {
    if value { "On" } else { "Off" }
}

pub(super) fn brand_mark_label(mode: BrandMarkMode) -> &'static str {
    match mode {
        BrandMarkMode::Emoji => "Emoji",
        BrandMarkMode::Snowflake => "Snowflake",
    }
}

pub(super) fn text_sizing_label(mode: TextSizingMode) -> &'static str {
    match mode {
        TextSizingMode::Off => "Off",
        TextSizingMode::Auto => "Auto",
        TextSizingMode::On => "On",
    }
}

fn format_reading_width(width: u16) -> String {
    if width == 0 {
        "Unlimited".to_owned()
    } else {
        format!("{width} cols")
    }
}

fn parse_context_window_tokens(value: &str) -> Result<u64, String> {
    let normalized = value.trim().to_ascii_lowercase();
    let (digits, multiplier) = if let Some(digits) = normalized.strip_suffix('k') {
        (digits, 1_000)
    } else if let Some(digits) = normalized.strip_suffix('m') {
        (digits, 1_000_000)
    } else {
        (normalized.as_str(), 1)
    };
    let tokens = digits
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(multiplier))
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            "Context window must be a positive token count (for example 272k)".to_owned()
        })?;
    Ok(tokens)
}

fn format_context_window_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 && tokens.is_multiple_of(1_000_000) {
        format!("{}m", tokens / 1_000_000)
    } else if tokens >= 1_000 && tokens.is_multiple_of(1_000) {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}
