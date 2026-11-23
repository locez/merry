//! Level-3 choice submenu builders and appliers for compact/mobile Settings navigation.

use super::{
    BRAND_MARKS, COMPACTION_STRATEGIES, SUBAGENT_LIMIT_BEHAVIORS, TEXT_SIZINGS, TuiState,
    brand_mark_label,
    permissions::{
        AUTO_FALLBACKS, PERMISSION_MODES, SANDBOX_MODES, auto_fallback_label,
        permission_mode_label, sandbox_label,
    },
    roles::{ROLE_MAX_TURNS, ROLE_TIMEOUTS},
    text_sizing_label,
};
use crate::{
    config::{ConfiguredProviderKind, MAX_MOUSE_SCROLL_LINES},
    tui::{
        overlay::{Overlay, SettingItem},
        preferences::CodeTheme,
        settings_overlay::SettingChoice,
        theme::ThemePreset,
    },
};

const CODE_THEMES: [CodeTheme; 3] = [
    CodeTheme::Dracula,
    CodeTheme::CatppuccinMocha,
    CodeTheme::MonokaiBright,
];
const UI_THEMES: [Option<ThemePreset>; 7] = [
    None,
    Some(ThemePreset::Sakura),
    Some(ThemePreset::Ansi),
    Some(ThemePreset::TokyoNight),
    Some(ThemePreset::CatppuccinMocha),
    Some(ThemePreset::Nord),
    Some(ThemePreset::PaperLight),
];
const MAX_THREADS_CHOICES: [Option<usize>; 8] = [
    None,
    Some(1),
    Some(2),
    Some(4),
    Some(6),
    Some(8),
    Some(12),
    Some(16),
];
const MAX_PARALLEL_CHOICES: [Option<usize>; 10] = [
    None,
    Some(1),
    Some(2),
    Some(4),
    Some(6),
    Some(8),
    Some(12),
    Some(16),
    Some(24),
    Some(32),
];

impl TuiState {
    pub(crate) fn open_settings_choice_menu(&mut self, item: SettingItem) -> bool {
        let Some(choices) = self.settings_choices_for(item) else {
            return false;
        };
        if let Some(Overlay::Settings(settings)) = self.overlay_mut() {
            settings.open_choice_menu(item, choices);
            return true;
        }
        false
    }

    pub(crate) fn settings_choices_for(&self, item: SettingItem) -> Option<Vec<SettingChoice>> {
        let choice = |label: String, current: bool| SettingChoice { label, current };
        match item {
            SettingItem::CodeTheme => Some(
                CODE_THEMES
                    .into_iter()
                    .map(|theme| {
                        choice(
                            theme.label().to_owned(),
                            self.preferences.code_theme == theme,
                        )
                    })
                    .collect(),
            ),
            SettingItem::UiTheme => Some(
                UI_THEMES
                    .into_iter()
                    .map(|preset| {
                        let label = preset.map_or_else(
                            || format!("Inherit ({})", self.settings_defaults.theme_preset.label()),
                            |p| p.label().to_owned(),
                        );
                        choice(label, self.preferences.theme_preset == preset)
                    })
                    .collect(),
            ),
            SettingItem::BrandMark => Some(
                BRAND_MARKS
                    .into_iter()
                    .map(|mode| {
                        let label = mode.map_or_else(
                            || {
                                format!(
                                    "Inherit ({})",
                                    brand_mark_label(self.settings_defaults.brand_mark)
                                )
                            },
                            |m| brand_mark_label(m).to_owned(),
                        );
                        choice(label, self.preferences.brand_mark == mode)
                    })
                    .collect(),
            ),
            SettingItem::TextSizing => Some(
                TEXT_SIZINGS
                    .into_iter()
                    .map(|mode| {
                        let label = mode.map_or_else(
                            || {
                                format!(
                                    "Inherit ({})",
                                    text_sizing_label(self.settings_defaults.text_sizing)
                                )
                            },
                            |m| text_sizing_label(m).to_owned(),
                        );
                        choice(label, self.preferences.text_sizing == mode)
                    })
                    .collect(),
            ),
            SettingItem::MouseScrollLines => {
                let mut choices = vec![choice(
                    format!(
                        "Inherit ({} lines)",
                        self.settings_defaults.mouse_scroll_lines
                    ),
                    self.preferences.mouse_scroll_lines.is_none(),
                )];
                for lines in 1..=MAX_MOUSE_SCROLL_LINES {
                    let suffix = if lines == 1 { "line" } else { "lines" };
                    choices.push(choice(
                        format!("{lines} {suffix}"),
                        self.preferences.mouse_scroll_lines == Some(lines),
                    ));
                }
                Some(choices)
            }
            SettingItem::ServiceTier => {
                let provider = self.effective_provider_alias()?;
                if self.settings_defaults.provider_kind(Some(provider))
                    == Some(ConfiguredProviderKind::Anthropic)
                {
                    return None;
                }
                let current = self.preferences.service_tier_for_provider(provider);
                let inherited = self
                    .settings_defaults
                    .service_tier_for_provider(Some(provider));
                Some(
                    self.service_tier_choices(provider)
                        .into_iter()
                        .map(|tier| {
                            let label = tier.map_or_else(
                                || {
                                    inherited
                                        .map(|t| format!("Inherit ({})", t.as_str()))
                                        .unwrap_or_else(|| "Inherit".to_owned())
                                },
                                |t| t.as_str().to_owned(),
                            );
                            choice(label, current == tier)
                        })
                        .collect(),
                )
            }
            SettingItem::CompactionProvider => {
                let current = self
                    .preferences
                    .context_compaction_model
                    .as_ref()
                    .and_then(|binding| binding.provider.as_deref());
                let inherited = self
                    .settings_defaults
                    .context_compaction_model
                    .as_ref()
                    .map(|(provider, _)| provider.as_str())
                    .unwrap_or("primary");
                Some(self.provider_override_choices(current, inherited))
            }
            SettingItem::ApprovalReviewProvider => {
                let current = self
                    .preferences
                    .approval_review_model
                    .as_ref()
                    .and_then(|binding| binding.provider.as_deref());
                let inherited = self
                    .settings_defaults
                    .approval_review_model
                    .as_ref()
                    .map(|(provider, _)| provider.as_str())
                    .unwrap_or("primary");
                Some(self.provider_override_choices(current, inherited))
            }
            SettingItem::ContextStrategy => Some(
                COMPACTION_STRATEGIES
                    .into_iter()
                    .map(|strategy| {
                        let label = strategy.map_or_else(
                            || format!("Inherit ({})", self.settings_defaults.compaction_strategy),
                            |s| s.label().to_owned(),
                        );
                        choice(label, self.preferences.compaction_strategy == strategy)
                    })
                    .collect(),
            ),
            SettingItem::RetainedModelTurns => {
                let mut choices = vec![choice(
                    format!("Inherit ({})", self.settings_defaults.retained_model_turns),
                    self.preferences.retained_model_turns.is_none(),
                )];
                for turns in 1..=10usize {
                    choices.push(choice(
                        format!("{turns} turns"),
                        self.preferences.retained_model_turns == Some(turns),
                    ));
                }
                Some(choices)
            }
            SettingItem::MaxThreads => Some(
                MAX_THREADS_CHOICES
                    .into_iter()
                    .map(|value| {
                        let label = value.map_or_else(
                            || format!("Inherit ({})", self.settings_defaults.subagent_max_threads),
                            |v| v.to_string(),
                        );
                        choice(label, self.preferences.subagent_max_threads == value)
                    })
                    .collect(),
            ),
            SettingItem::SubagentLimitBehavior => Some(
                SUBAGENT_LIMIT_BEHAVIORS
                    .into_iter()
                    .map(|behavior| {
                        let label = behavior.map_or_else(
                            || {
                                format!(
                                    "Inherit ({})",
                                    self.settings_defaults.subagent_limit_behavior.label()
                                )
                            },
                            |b| b.label().to_owned(),
                        );
                        choice(label, self.preferences.subagent_limit_behavior == behavior)
                    })
                    .collect(),
            ),
            SettingItem::MaxParallelToolCalls => Some(
                MAX_PARALLEL_CHOICES
                    .into_iter()
                    .map(|value| {
                        let label = value.map_or_else(
                            || {
                                format!(
                                    "Inherit ({})",
                                    self.settings_defaults.max_parallel_tool_calls
                                )
                            },
                            |v| v.to_string(),
                        );
                        choice(label, self.preferences.max_parallel_tool_calls == value)
                    })
                    .collect(),
            ),
            SettingItem::SubagentDefaultProvider => {
                let current = self
                    .preferences
                    .subagent_default_model
                    .as_ref()
                    .and_then(|binding| binding.provider.as_deref());
                let inherited = self
                    .settings_defaults
                    .subagent_default_model
                    .provider
                    .as_deref()
                    .unwrap_or("primary");
                Some(self.provider_override_choices(current, inherited))
            }
            SettingItem::SubagentRoleSelector => {
                let selected = self.selected_settings_role();
                Some(
                    self.subagent_role_names()
                        .into_iter()
                        .map(|role| {
                            let is_current = role == selected;
                            choice(role, is_current)
                        })
                        .collect(),
                )
            }
            SettingItem::SubagentRoleProvider => {
                let role = self.selected_settings_role();
                let current = self
                    .preferences
                    .subagent_roles
                    .get(role)
                    .and_then(|cfg| cfg.provider.as_deref());
                let inherited = self
                    .settings_defaults
                    .subagent_roles
                    .get(role)
                    .and_then(|cfg| cfg.provider.as_deref())
                    .unwrap_or("subagent default");
                Some(self.provider_override_choices(current, inherited))
            }
            SettingItem::SubagentRoleMaxTurns => {
                let role = self.selected_settings_role();
                let current = self
                    .preferences
                    .subagent_roles
                    .get(role)
                    .and_then(|cfg| cfg.max_model_turns);
                let inherited = self
                    .settings_defaults
                    .subagent_roles
                    .get(role)
                    .and_then(|cfg| cfg.max_model_turns);
                Some(
                    ROLE_MAX_TURNS
                        .into_iter()
                        .map(|turns| {
                            let label = turns.map_or_else(
                                || {
                                    inherited
                                        .map(|v| format!("Inherit ({v})"))
                                        .unwrap_or_else(|| "Inherit".to_owned())
                                },
                                |v| format!("{v} turns"),
                            );
                            choice(label, current == turns)
                        })
                        .collect(),
                )
            }
            SettingItem::SubagentRoleTimeout => {
                let role = self.selected_settings_role();
                let current = self
                    .preferences
                    .subagent_roles
                    .get(role)
                    .and_then(|cfg| cfg.timeout_secs);
                let inherited = self
                    .settings_defaults
                    .subagent_roles
                    .get(role)
                    .and_then(|cfg| cfg.timeout_secs);
                Some(
                    ROLE_TIMEOUTS
                        .into_iter()
                        .map(|secs| {
                            let label = secs.map_or_else(
                                || {
                                    inherited
                                        .map(|v| format!("Inherit ({v}s)"))
                                        .unwrap_or_else(|| "Inherit".to_owned())
                                },
                                |v| format!("{v}s"),
                            );
                            choice(label, current == secs)
                        })
                        .collect(),
                )
            }
            SettingItem::PermissionSandbox => Some(
                SANDBOX_MODES
                    .into_iter()
                    .map(|mode| {
                        let label = mode.map_or_else(
                            || {
                                format!(
                                    "Inherit ({})",
                                    sandbox_label(self.settings_defaults.sandbox)
                                )
                            },
                            |m| sandbox_label(m).to_owned(),
                        );
                        choice(label, self.preferences.permissions.sandbox == mode)
                    })
                    .collect(),
            ),
            SettingItem::PermissionMode => Some(
                PERMISSION_MODES
                    .into_iter()
                    .map(|mode| {
                        let label = mode.map_or_else(
                            || {
                                format!(
                                    "Inherit ({})",
                                    permission_mode_label(self.settings_defaults.permission_mode)
                                )
                            },
                            |m| permission_mode_label(m).to_owned(),
                        );
                        choice(label, self.preferences.permissions.permission_mode == mode)
                    })
                    .collect(),
            ),
            SettingItem::PermissionAutoFallback => Some(
                AUTO_FALLBACKS
                    .into_iter()
                    .map(|mode| {
                        let label = mode.map_or_else(
                            || {
                                format!(
                                    "Inherit ({})",
                                    auto_fallback_label(self.settings_defaults.auto_fallback)
                                )
                            },
                            |m| auto_fallback_label(m).to_owned(),
                        );
                        choice(label, self.preferences.permissions.auto_fallback == mode)
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    pub(crate) fn apply_setting_choice(&mut self, item: SettingItem, index: usize) -> bool {
        let changed = match item {
            SettingItem::CodeTheme => {
                let Some(&theme) = CODE_THEMES.get(index) else {
                    return false;
                };
                self.preferences.code_theme = theme;
                true
            }
            SettingItem::UiTheme => {
                let Some(&preset) = UI_THEMES.get(index) else {
                    return false;
                };
                self.preferences.theme_preset = preset;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::BrandMark => {
                let Some(&mode) = BRAND_MARKS.get(index) else {
                    return false;
                };
                self.preferences.brand_mark = mode;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::TextSizing => {
                let Some(&mode) = TEXT_SIZINGS.get(index) else {
                    return false;
                };
                self.preferences.text_sizing = mode;
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::MouseScrollLines => {
                self.preferences.mouse_scroll_lines = if index == 0 {
                    None
                } else if (1..=MAX_MOUSE_SCROLL_LINES).contains(&index) {
                    Some(index)
                } else {
                    return false;
                };
                self.sync_active_ui_preferences();
                true
            }
            SettingItem::ServiceTier => {
                let Some(provider) = self.effective_provider_alias().map(str::to_owned) else {
                    return false;
                };
                let choices = self.service_tier_choices(&provider);
                let Some(&tier) = choices.get(index) else {
                    return false;
                };
                self.preferences
                    .set_service_tier_for_provider(&provider, tier)
                    .is_ok()
            }
            SettingItem::CompactionProvider => {
                let Some(next_provider) = self.resolve_provider_choice(index) else {
                    return false;
                };
                let mut slot = self.preferences.context_compaction_model.take();
                self.set_role_override_provider(&mut slot, next_provider);
                self.preferences.context_compaction_model = slot;
                true
            }
            SettingItem::ApprovalReviewProvider => {
                let Some(next_provider) = self.resolve_provider_choice(index) else {
                    return false;
                };
                let mut slot = self.preferences.approval_review_model.take();
                self.set_role_override_provider(&mut slot, next_provider);
                self.preferences.approval_review_model = slot;
                true
            }
            SettingItem::ContextStrategy => {
                let Some(&strategy) = COMPACTION_STRATEGIES.get(index) else {
                    return false;
                };
                self.preferences.compaction_strategy = strategy;
                true
            }
            SettingItem::RetainedModelTurns => {
                self.preferences.retained_model_turns = if index == 0 {
                    None
                } else if (1..=10).contains(&index) {
                    Some(index)
                } else {
                    return false;
                };
                true
            }
            SettingItem::MaxThreads => {
                let Some(&value) = MAX_THREADS_CHOICES.get(index) else {
                    return false;
                };
                self.preferences.subagent_max_threads = value;
                true
            }
            SettingItem::SubagentLimitBehavior => {
                let Some(&behavior) = SUBAGENT_LIMIT_BEHAVIORS.get(index) else {
                    return false;
                };
                self.preferences.subagent_limit_behavior = behavior;
                true
            }
            SettingItem::MaxParallelToolCalls => {
                let Some(&value) = MAX_PARALLEL_CHOICES.get(index) else {
                    return false;
                };
                self.preferences.max_parallel_tool_calls = value;
                true
            }
            SettingItem::SubagentDefaultProvider => {
                let Some(next_provider) = self.resolve_provider_choice(index) else {
                    return false;
                };
                let mut slot = self.preferences.subagent_default_model.take();
                self.set_role_override_provider(&mut slot, next_provider);
                self.preferences.subagent_default_model = slot;
                true
            }
            SettingItem::SubagentRoleSelector => {
                let roles = self.subagent_role_names();
                let Some(role) = roles.get(index).cloned() else {
                    return false;
                };
                if let Some(Overlay::Settings(settings)) = self.overlay_mut() {
                    settings.set_selected_role(role);
                }
                return false;
            }
            SettingItem::SubagentRoleProvider => {
                let Some(next_provider) = self.resolve_provider_choice(index) else {
                    return false;
                };
                let model = next_provider
                    .as_deref()
                    .and_then(|provider| self.default_or_preferred_model(provider))
                    .map(str::to_owned);
                self.mutate_selected_subagent_role(|entry| {
                    entry.provider = next_provider;
                    entry.model = model;
                });
                true
            }
            SettingItem::SubagentRoleMaxTurns => {
                let Some(&turns) = ROLE_MAX_TURNS.get(index) else {
                    return false;
                };
                self.mutate_selected_subagent_role(|entry| entry.max_model_turns = turns);
                true
            }
            SettingItem::SubagentRoleTimeout => {
                let Some(&secs) = ROLE_TIMEOUTS.get(index) else {
                    return false;
                };
                self.mutate_selected_subagent_role(|entry| entry.timeout_secs = secs);
                true
            }
            SettingItem::PermissionSandbox => {
                let Some(&mode) = SANDBOX_MODES.get(index) else {
                    return false;
                };
                self.preferences.permissions.sandbox = mode;
                true
            }
            SettingItem::PermissionMode => {
                let Some(&mode) = PERMISSION_MODES.get(index) else {
                    return false;
                };
                self.preferences.permissions.permission_mode = mode;
                true
            }
            SettingItem::PermissionAutoFallback => {
                let Some(&mode) = AUTO_FALLBACKS.get(index) else {
                    return false;
                };
                self.preferences.permissions.auto_fallback = mode;
                true
            }
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

    fn provider_override_choices(
        &self,
        current: Option<&str>,
        inherited: &str,
    ) -> Vec<SettingChoice> {
        let mut choices = vec![SettingChoice {
            label: format!("Inherit ({inherited})"),
            current: current.is_none(),
        }];
        for alias in &self.settings_defaults.provider_aliases {
            choices.push(SettingChoice {
                label: alias.clone(),
                current: current == Some(alias.as_str()),
            });
        }
        choices
    }

    fn resolve_provider_choice(&self, index: usize) -> Option<Option<String>> {
        if index == 0 {
            Some(None)
        } else {
            self.settings_defaults
                .provider_aliases
                .get(index - 1)
                .cloned()
                .map(Some)
        }
    }
}
