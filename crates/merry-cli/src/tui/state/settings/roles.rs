//! Role model binding helpers for the Settings Workbench (Stage 2).

use super::TuiState;
use crate::tui::{
    overlay::{Overlay, SettingDirection, SettingItem},
    preferences::{REASONING_EFFORT_PRESETS, RoleModelOverride, SubagentRolePreferenceOverride},
    provider_selection::{ModelBindingTarget, ModelPickerTarget, ReasoningBindingTarget},
};
use merry_llm::ReasoningEffort;

pub(super) const ROLE_MAX_TURNS: [Option<u32>; 10] = [
    None,
    Some(4),
    Some(8),
    Some(12),
    Some(16),
    Some(24),
    Some(32),
    Some(48),
    Some(64),
    Some(96),
];
pub(super) const ROLE_TIMEOUTS: [Option<u64>; 7] = [
    None,
    Some(60),
    Some(120),
    Some(180),
    Some(300),
    Some(600),
    Some(900),
];

impl TuiState {
    pub(crate) fn selected_settings_role(&self) -> &str {
        match self.overlay() {
            Some(Overlay::Settings(settings)) => settings.selected_role(),
            _ => "explore",
        }
    }

    pub(crate) fn active_or_first_provider_alias(&self) -> Option<String> {
        self.effective_provider_alias()
            .or_else(|| {
                self.settings_defaults
                    .provider_aliases
                    .first()
                    .map(String::as_str)
            })
            .map(str::to_owned)
    }

    pub(crate) fn settings_model_picker_request(
        &self,
        item: SettingItem,
    ) -> Option<(String, ModelPickerTarget)> {
        if item == SettingItem::ActiveModel {
            let provider = self.active_or_first_provider_alias()?;
            return Some((provider, ModelPickerTarget::SettingsActiveModel));
        }
        let target = match item {
            SettingItem::CompactionModel => ModelBindingTarget::ContextCompaction,
            SettingItem::ApprovalReviewModel => ModelBindingTarget::ApprovalReview,
            SettingItem::SubagentDefaultModel => ModelBindingTarget::SubagentDefault,
            SettingItem::SubagentRoleModel => {
                ModelBindingTarget::SubagentRole(self.selected_settings_role().to_owned())
            }
            _ => return None,
        };
        let provider = self.effective_provider_for_role_target(&target)?;
        Some((provider.to_owned(), ModelPickerTarget::RoleBinding(target)))
    }

    pub(crate) fn open_settings_reasoning_picker(&mut self, item: SettingItem) -> bool {
        let (alias, model, initial, target) = match item {
            SettingItem::ReasoningEffort => {
                let Some(alias) = self.effective_provider_alias().map(str::to_owned) else {
                    return false;
                };
                let model = self
                    .preferences
                    .model_for_provider(&alias)
                    .or_else(|| {
                        self.settings_defaults
                            .default_model_for_provider(Some(&alias))
                    })
                    .unwrap_or("Active model")
                    .to_owned();
                let initial = self
                    .preferences
                    .reasoning_label_for_provider(&alias)
                    .or_else(|| {
                        self.settings_defaults
                            .reasoning_effort_for_provider(Some(&alias))
                            .map(ReasoningEffort::as_str)
                    })
                    .map(str::to_owned);
                (
                    alias,
                    model,
                    initial,
                    ReasoningBindingTarget::ActiveProvider,
                )
            }
            SettingItem::SubagentDefaultReasoning => {
                let alias = self
                    .effective_provider_for_role_target(&ModelBindingTarget::SubagentDefault)
                    .unwrap_or("default")
                    .to_owned();
                let model = self
                    .preferences
                    .subagent_default_model
                    .as_ref()
                    .and_then(|binding| binding.model.clone())
                    .or_else(|| self.settings_defaults.subagent_default_model.model.clone())
                    .unwrap_or_else(|| "Subagent default".to_owned());
                let initial = self
                    .preferences
                    .subagent_default_model
                    .as_ref()
                    .and_then(|binding| binding.reasoning_effort.as_ref())
                    .or(self
                        .settings_defaults
                        .subagent_default_model
                        .reasoning_effort
                        .as_ref())
                    .map(|effort| effort.as_str().to_owned());
                (
                    alias,
                    model,
                    initial,
                    ReasoningBindingTarget::SubagentDefault,
                )
            }
            SettingItem::SubagentRoleReasoning => {
                let role = self.selected_settings_role().to_owned();
                let alias = self
                    .effective_provider_for_role_target(&ModelBindingTarget::SubagentRole(
                        role.clone(),
                    ))
                    .unwrap_or("default")
                    .to_owned();
                let model = self
                    .preferences
                    .subagent_roles
                    .get(&role)
                    .and_then(|cfg| cfg.model.clone())
                    .or_else(|| {
                        self.settings_defaults
                            .subagent_roles
                            .get(&role)
                            .and_then(|cfg| cfg.model.clone())
                    })
                    .unwrap_or_else(|| format!("Subagent role: {role}"));
                let initial = self
                    .preferences
                    .subagent_roles
                    .get(&role)
                    .and_then(|cfg| cfg.reasoning_effort.as_ref())
                    .or_else(|| {
                        self.settings_defaults
                            .subagent_roles
                            .get(&role)
                            .and_then(|cfg| cfg.reasoning_effort.as_ref())
                    })
                    .map(|effort| effort.as_str().to_owned());
                (
                    alias,
                    model,
                    initial,
                    ReasoningBindingTarget::SubagentRole(role),
                )
            }
            SettingItem::CompactionReasoning => {
                let alias = self
                    .effective_provider_for_role_target(&ModelBindingTarget::ContextCompaction)
                    .unwrap_or("default")
                    .to_owned();
                let model = self
                    .preferences
                    .context_compaction_model
                    .as_ref()
                    .and_then(|binding| binding.model.clone())
                    .or_else(|| {
                        self.settings_defaults
                            .context_compaction_model
                            .as_ref()
                            .map(|(_, model)| model.clone())
                    })
                    .unwrap_or_else(|| "Compaction".to_owned());
                let initial = self
                    .preferences
                    .context_compaction_model
                    .as_ref()
                    .and_then(|binding| binding.reasoning_effort.as_ref())
                    .map(|effort| effort.as_str().to_owned());
                (alias, model, initial, ReasoningBindingTarget::Compaction)
            }
            SettingItem::ApprovalReviewReasoning => {
                let alias = self
                    .effective_provider_for_role_target(&ModelBindingTarget::ApprovalReview)
                    .unwrap_or("default")
                    .to_owned();
                let model = self
                    .preferences
                    .approval_review_model
                    .as_ref()
                    .and_then(|binding| binding.model.clone())
                    .or_else(|| {
                        self.settings_defaults
                            .approval_review_model
                            .as_ref()
                            .map(|(_, model)| model.clone())
                    })
                    .unwrap_or_else(|| "Approval review".to_owned());
                let initial = self
                    .preferences
                    .approval_review_model
                    .as_ref()
                    .and_then(|binding| binding.reasoning_effort.as_ref())
                    .map(|effort| effort.as_str().to_owned());
                (
                    alias,
                    model,
                    initial,
                    ReasoningBindingTarget::ApprovalReview,
                )
            }
            _ => return false,
        };
        self.open_reasoning_picker_with_initial(
            alias,
            model,
            initial.as_deref(),
            ModelPickerTarget::SettingsReasoning(target),
        )
    }

    pub(crate) fn apply_reasoning_binding(
        &mut self,
        target: &ReasoningBindingTarget,
        alias: &str,
        reasoning_effort: ReasoningEffort,
    ) {
        match target {
            ReasoningBindingTarget::ActiveProvider => {
                let _ = self
                    .preferences
                    .set_reasoning_effort_for_provider(alias, Some(reasoning_effort));
                self.set_settings_notice(Some("Applied".to_owned()));
            }
            ReasoningBindingTarget::Compaction => {
                let entry = self
                    .preferences
                    .context_compaction_model
                    .get_or_insert_with(RoleModelOverride::default);
                entry.reasoning_effort = Some(reasoning_effort);
                self.set_settings_notice(Some("Applied".to_owned()));
            }
            ReasoningBindingTarget::ApprovalReview => {
                let entry = self
                    .preferences
                    .approval_review_model
                    .get_or_insert_with(RoleModelOverride::default);
                entry.reasoning_effort = Some(reasoning_effort);
                self.set_settings_notice(Some("Applied".to_owned()));
            }
            ReasoningBindingTarget::SubagentDefault => {
                let entry = self
                    .preferences
                    .subagent_default_model
                    .get_or_insert_with(RoleModelOverride::default);
                entry.reasoning_effort = Some(reasoning_effort);
                self.set_settings_notice(Some("Saved (applies on next session)".to_owned()));
            }
            ReasoningBindingTarget::SubagentRole(role) => {
                let entry = self
                    .preferences
                    .subagent_roles
                    .entry(role.clone())
                    .or_default();
                entry.reasoning_effort = Some(reasoning_effort);
                self.set_settings_notice(Some("Saved (applies on next session)".to_owned()));
            }
        }
    }

    pub(crate) fn apply_role_model_binding(
        &mut self,
        target: &ModelBindingTarget,
        alias: &str,
        model: &str,
        reasoning_effort: Option<ReasoningEffort>,
    ) {
        let live = matches!(
            target,
            ModelBindingTarget::ContextCompaction | ModelBindingTarget::ApprovalReview
        );
        match target {
            ModelBindingTarget::ContextCompaction => {
                set_role_override_binding(
                    &mut self.preferences.context_compaction_model,
                    alias,
                    model,
                    reasoning_effort,
                );
            }
            ModelBindingTarget::ApprovalReview => {
                set_role_override_binding(
                    &mut self.preferences.approval_review_model,
                    alias,
                    model,
                    reasoning_effort,
                );
            }
            ModelBindingTarget::SubagentDefault => {
                set_role_override_binding(
                    &mut self.preferences.subagent_default_model,
                    alias,
                    model,
                    reasoning_effort,
                );
            }
            ModelBindingTarget::SubagentRole(role) => {
                let entry = self
                    .preferences
                    .subagent_roles
                    .entry(role.clone())
                    .or_default();
                entry.provider = Some(alias.to_owned());
                entry.model = Some(model.to_owned());
                if let Some(effort) = reasoning_effort {
                    entry.reasoning_effort = Some(effort);
                }
            }
        }
        let notice = if live {
            "Applied"
        } else {
            "Saved (applies on next session)"
        };
        self.set_settings_notice(Some(notice.to_owned()));
    }

    pub(super) fn effective_provider_for_role_target(
        &self,
        target: &ModelBindingTarget,
    ) -> Option<&str> {
        let fallback = self.effective_provider_alias().or_else(|| {
            self.settings_defaults
                .provider_aliases
                .first()
                .map(String::as_str)
        });
        let specific = match target {
            ModelBindingTarget::ContextCompaction => self
                .preferences
                .context_compaction_model
                .as_ref()
                .and_then(|binding| binding.provider.as_deref())
                .or_else(|| {
                    self.settings_defaults
                        .context_compaction_model
                        .as_ref()
                        .map(|(provider, _)| provider.as_str())
                }),
            ModelBindingTarget::ApprovalReview => self
                .preferences
                .approval_review_model
                .as_ref()
                .and_then(|binding| binding.provider.as_deref())
                .or_else(|| {
                    self.settings_defaults
                        .approval_review_model
                        .as_ref()
                        .map(|(provider, _)| provider.as_str())
                }),
            ModelBindingTarget::SubagentDefault => self
                .preferences
                .subagent_default_model
                .as_ref()
                .and_then(|binding| binding.provider.as_deref())
                .or(self
                    .settings_defaults
                    .subagent_default_model
                    .provider
                    .as_deref()),
            ModelBindingTarget::SubagentRole(role) => self
                .preferences
                .subagent_roles
                .get(role)
                .and_then(|cfg| cfg.provider.as_deref())
                .or_else(|| {
                    self.settings_defaults
                        .subagent_roles
                        .get(role)
                        .and_then(|cfg| cfg.provider.as_deref())
                })
                .or_else(|| {
                    self.preferences
                        .subagent_default_model
                        .as_ref()
                        .and_then(|binding| binding.provider.as_deref())
                })
                .or(self
                    .settings_defaults
                    .subagent_default_model
                    .provider
                    .as_deref()),
        };
        specific.or(fallback)
    }

    pub(super) fn set_role_override_provider(
        &self,
        slot: &mut Option<RoleModelOverride>,
        next_provider: Option<String>,
    ) {
        let model = next_provider
            .as_deref()
            .and_then(|provider| self.default_or_preferred_model(provider))
            .map(str::to_owned);
        let entry = slot.get_or_insert_with(RoleModelOverride::default);
        match next_provider {
            Some(provider) => {
                entry.provider = Some(provider);
                entry.model = model;
            }
            None => {
                entry.provider = None;
                entry.model = None;
            }
        }
        if entry.is_empty() {
            *slot = None;
        }
    }

    pub(super) fn mutate_selected_subagent_role(
        &mut self,
        mutate: impl FnOnce(&mut SubagentRolePreferenceOverride),
    ) {
        let role = self.selected_settings_role().to_owned();
        let entry = self
            .preferences
            .subagent_roles
            .entry(role.clone())
            .or_default();
        mutate(entry);
        if entry.is_empty() {
            self.preferences.subagent_roles.remove(&role);
        }
    }

    fn clear_selected_subagent_role_field(
        &mut self,
        clear: impl FnOnce(&mut SubagentRolePreferenceOverride),
    ) {
        let role = self.selected_settings_role().to_owned();
        if let Some(entry) = self.preferences.subagent_roles.get_mut(&role) {
            clear(entry);
            if entry.is_empty() {
                self.preferences.subagent_roles.remove(&role);
            }
        }
    }

    fn cycle_role_override_provider(
        &mut self,
        slot: &mut Option<RoleModelOverride>,
        direction: SettingDirection,
    ) -> bool {
        let current = slot
            .as_ref()
            .and_then(|binding| binding.provider.as_deref())
            .map(str::to_owned);
        let Some(next_provider) = self.cycle_provider_alias(current.as_deref(), direction) else {
            return false;
        };
        self.set_role_override_provider(slot, next_provider);
        true
    }

    pub(super) fn adjust_role_setting(
        &mut self,
        item: SettingItem,
        direction: SettingDirection,
    ) -> bool {
        match item {
            SettingItem::CompactionProvider => {
                let mut slot = self.preferences.context_compaction_model.take();
                let changed = self.cycle_role_override_provider(&mut slot, direction);
                self.preferences.context_compaction_model = slot;
                changed
            }
            SettingItem::CompactionReasoning => {
                cycle_role_override_reasoning(
                    &mut self.preferences.context_compaction_model,
                    direction,
                );
                true
            }
            SettingItem::ApprovalReviewProvider => {
                let mut slot = self.preferences.approval_review_model.take();
                let changed = self.cycle_role_override_provider(&mut slot, direction);
                self.preferences.approval_review_model = slot;
                changed
            }
            SettingItem::ApprovalReviewReasoning => {
                cycle_role_override_reasoning(
                    &mut self.preferences.approval_review_model,
                    direction,
                );
                true
            }
            SettingItem::SubagentDefaultProvider => {
                let mut slot = self.preferences.subagent_default_model.take();
                let changed = self.cycle_role_override_provider(&mut slot, direction);
                self.preferences.subagent_default_model = slot;
                changed
            }
            SettingItem::SubagentDefaultReasoning => {
                cycle_role_override_reasoning(
                    &mut self.preferences.subagent_default_model,
                    direction,
                );
                true
            }
            SettingItem::SubagentRoleSelector => {
                let roles = self.subagent_role_names();
                if roles.is_empty() {
                    return false;
                }
                let current = self.selected_settings_role().to_owned();
                let index = roles.iter().position(|name| name == &current).unwrap_or(0);
                let len = roles.len();
                let next = match direction {
                    SettingDirection::Previous => (index + len - 1) % len,
                    SettingDirection::Next => (index + 1) % len,
                };
                if let Some(Overlay::Settings(settings)) = self.overlay_mut() {
                    settings.set_selected_role(roles[next].clone());
                }
                false
            }
            SettingItem::SubagentRoleEnabled => {
                self.mutate_selected_subagent_role(|entry| {
                    super::cycle_opt_bool(&mut entry.enabled, direction);
                });
                true
            }
            SettingItem::SubagentRoleProvider => {
                let role = self.selected_settings_role().to_owned();
                let current = self
                    .preferences
                    .subagent_roles
                    .get(&role)
                    .and_then(|cfg| cfg.provider.as_deref());
                let Some(next_provider) = self.cycle_provider_alias(current, direction) else {
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
            SettingItem::SubagentRoleReasoning => {
                let current = self
                    .preferences
                    .subagent_roles
                    .get(self.selected_settings_role())
                    .and_then(|cfg| cfg.reasoning_effort.as_ref())
                    .map(ReasoningEffort::as_str);
                let next = cycle_reasoning_preset(current, direction);
                self.mutate_selected_subagent_role(|entry| {
                    entry.reasoning_effort = next;
                });
                true
            }
            SettingItem::SubagentRoleMaxTurns => {
                self.mutate_selected_subagent_role(|entry| {
                    super::cycle_slice(&mut entry.max_model_turns, &ROLE_MAX_TURNS, direction);
                });
                true
            }
            SettingItem::SubagentRoleTimeout => {
                self.mutate_selected_subagent_role(|entry| {
                    super::cycle_slice(&mut entry.timeout_secs, &ROLE_TIMEOUTS, direction);
                });
                true
            }
            _ => false,
        }
    }

    pub(super) fn reset_role_setting(&mut self, item: SettingItem) -> bool {
        match item {
            SettingItem::CompactionProvider | SettingItem::CompactionModel => {
                clear_role_provider_model(&mut self.preferences.context_compaction_model);
                true
            }
            SettingItem::CompactionReasoning => {
                clear_role_reasoning(&mut self.preferences.context_compaction_model);
                true
            }
            SettingItem::ApprovalReviewProvider | SettingItem::ApprovalReviewModel => {
                clear_role_provider_model(&mut self.preferences.approval_review_model);
                true
            }
            SettingItem::ApprovalReviewReasoning => {
                clear_role_reasoning(&mut self.preferences.approval_review_model);
                true
            }
            SettingItem::SubagentDefaultProvider | SettingItem::SubagentDefaultModel => {
                clear_role_provider_model(&mut self.preferences.subagent_default_model);
                true
            }
            SettingItem::SubagentDefaultReasoning => {
                clear_role_reasoning(&mut self.preferences.subagent_default_model);
                true
            }
            SettingItem::SubagentRoleEnabled => {
                self.clear_selected_subagent_role_field(|entry| entry.enabled = None);
                true
            }
            SettingItem::SubagentRoleProvider | SettingItem::SubagentRoleModel => {
                self.clear_selected_subagent_role_field(|entry| {
                    entry.provider = None;
                    entry.model = None;
                });
                true
            }
            SettingItem::SubagentRoleReasoning => {
                self.clear_selected_subagent_role_field(|entry| entry.reasoning_effort = None);
                true
            }
            SettingItem::SubagentRoleMaxTurns => {
                self.clear_selected_subagent_role_field(|entry| entry.max_model_turns = None);
                true
            }
            SettingItem::SubagentRoleTimeout => {
                self.clear_selected_subagent_role_field(|entry| entry.timeout_secs = None);
                true
            }
            _ => false,
        }
    }

    pub(super) fn is_role_setting_overridden(&self, item: SettingItem) -> bool {
        let role = self.selected_settings_role();
        let role_pref = self.preferences.subagent_roles.get(role);
        match item {
            SettingItem::CompactionProvider => self
                .preferences
                .context_compaction_model
                .as_ref()
                .is_some_and(|b| b.provider.is_some()),
            SettingItem::CompactionModel => self
                .preferences
                .context_compaction_model
                .as_ref()
                .is_some_and(|b| b.model.is_some()),
            SettingItem::CompactionReasoning => self
                .preferences
                .context_compaction_model
                .as_ref()
                .is_some_and(|b| b.reasoning_effort.is_some()),
            SettingItem::ApprovalReviewProvider => self
                .preferences
                .approval_review_model
                .as_ref()
                .is_some_and(|b| b.provider.is_some()),
            SettingItem::ApprovalReviewModel => self
                .preferences
                .approval_review_model
                .as_ref()
                .is_some_and(|b| b.model.is_some()),
            SettingItem::ApprovalReviewReasoning => self
                .preferences
                .approval_review_model
                .as_ref()
                .is_some_and(|b| b.reasoning_effort.is_some()),
            SettingItem::SubagentDefaultProvider => self
                .preferences
                .subagent_default_model
                .as_ref()
                .is_some_and(|b| b.provider.is_some()),
            SettingItem::SubagentDefaultModel => self
                .preferences
                .subagent_default_model
                .as_ref()
                .is_some_and(|b| b.model.is_some()),
            SettingItem::SubagentDefaultReasoning => self
                .preferences
                .subagent_default_model
                .as_ref()
                .is_some_and(|b| b.reasoning_effort.is_some()),
            SettingItem::SubagentRoleEnabled => role_pref.is_some_and(|r| r.enabled.is_some()),
            SettingItem::SubagentRoleProvider => role_pref.is_some_and(|r| r.provider.is_some()),
            SettingItem::SubagentRoleModel => role_pref.is_some_and(|r| r.model.is_some()),
            SettingItem::SubagentRoleReasoning => {
                role_pref.is_some_and(|r| r.reasoning_effort.is_some())
            }
            SettingItem::SubagentRoleMaxTurns => {
                role_pref.is_some_and(|r| r.max_model_turns.is_some())
            }
            SettingItem::SubagentRoleTimeout => role_pref.is_some_and(|r| r.timeout_secs.is_some()),
            _ => false,
        }
    }

    pub(super) fn role_setting_value(&self, item: SettingItem) -> String {
        let role = self.selected_settings_role();
        let role_pref = self.preferences.subagent_roles.get(role);
        let role_default = self.settings_defaults.subagent_roles.get(role);
        match item {
            SettingItem::CompactionProvider => super::inherited_value(
                self.preferences
                    .context_compaction_model
                    .as_ref()
                    .and_then(|b| b.provider.as_deref()),
                self.settings_defaults
                    .context_compaction_model
                    .as_ref()
                    .map(|(provider, _)| provider.as_str())
                    .or(Some("primary")),
            ),
            SettingItem::CompactionModel => format_pickable_model(
                self.preferences
                    .context_compaction_model
                    .as_ref()
                    .and_then(|b| b.model.as_deref()),
                self.settings_defaults
                    .context_compaction_model
                    .as_ref()
                    .map(|(_, model)| model.as_str()),
            ),
            SettingItem::CompactionReasoning => format!(
                "{} ▸",
                super::inherited_value(
                    self.preferences
                        .context_compaction_model
                        .as_ref()
                        .and_then(|b| b.reasoning_effort.as_ref())
                        .map(ReasoningEffort::as_str),
                    Some("primary"),
                )
            ),
            SettingItem::ApprovalReviewProvider => super::inherited_value(
                self.preferences
                    .approval_review_model
                    .as_ref()
                    .and_then(|b| b.provider.as_deref()),
                self.settings_defaults
                    .approval_review_model
                    .as_ref()
                    .map(|(provider, _)| provider.as_str())
                    .or(Some("primary")),
            ),
            SettingItem::ApprovalReviewModel => format_pickable_model(
                self.preferences
                    .approval_review_model
                    .as_ref()
                    .and_then(|b| b.model.as_deref()),
                self.settings_defaults
                    .approval_review_model
                    .as_ref()
                    .map(|(_, model)| model.as_str()),
            ),
            SettingItem::ApprovalReviewReasoning => format!(
                "{} ▸",
                super::inherited_value(
                    self.preferences
                        .approval_review_model
                        .as_ref()
                        .and_then(|b| b.reasoning_effort.as_ref())
                        .map(ReasoningEffort::as_str),
                    Some("low"),
                )
            ),
            SettingItem::SubagentDefaultProvider => super::inherited_value(
                self.preferences
                    .subagent_default_model
                    .as_ref()
                    .and_then(|b| b.provider.as_deref()),
                self.settings_defaults
                    .subagent_default_model
                    .provider
                    .as_deref()
                    .or(Some("primary")),
            ),
            SettingItem::SubagentDefaultModel => format_pickable_model(
                self.preferences
                    .subagent_default_model
                    .as_ref()
                    .and_then(|b| b.model.as_deref()),
                self.settings_defaults
                    .subagent_default_model
                    .model
                    .as_deref(),
            ),
            SettingItem::SubagentDefaultReasoning => format!(
                "{} ▸",
                super::inherited_value(
                    self.preferences
                        .subagent_default_model
                        .as_ref()
                        .and_then(|b| b.reasoning_effort.as_ref())
                        .map(ReasoningEffort::as_str),
                    self.settings_defaults
                        .subagent_default_model
                        .reasoning_effort
                        .as_ref()
                        .map(ReasoningEffort::as_str),
                )
            ),
            SettingItem::SubagentRoleSelector => format!("◂ {role} ▸"),
            SettingItem::SubagentRoleEnabled => super::opt_bool_value(
                role_pref.and_then(|r| r.enabled),
                role_default.is_none_or(|r| r.enabled),
            ),
            SettingItem::SubagentRoleProvider => super::inherited_value(
                role_pref.and_then(|r| r.provider.as_deref()),
                role_default
                    .and_then(|r| r.provider.as_deref())
                    .or(Some("subagent default")),
            ),
            SettingItem::SubagentRoleModel => format_pickable_model(
                role_pref.and_then(|r| r.model.as_deref()),
                role_default.and_then(|r| r.model.as_deref()),
            ),
            SettingItem::SubagentRoleReasoning => format!(
                "{} ▸",
                super::inherited_value(
                    role_pref
                        .and_then(|r| r.reasoning_effort.as_ref())
                        .map(ReasoningEffort::as_str),
                    role_default
                        .and_then(|r| r.reasoning_effort.as_ref())
                        .map(ReasoningEffort::as_str),
                )
            ),
            SettingItem::SubagentRoleMaxTurns => role_pref
                .and_then(|r| r.max_model_turns)
                .map(|turns| format!("{turns} turns"))
                .unwrap_or_else(|| {
                    role_default
                        .and_then(|r| r.max_model_turns)
                        .map(|turns| format!("Inherit ({turns})"))
                        .unwrap_or_else(|| "Inherit".to_owned())
                }),
            SettingItem::SubagentRoleTimeout => role_pref
                .and_then(|r| r.timeout_secs)
                .map(|secs| format!("{secs}s"))
                .unwrap_or_else(|| {
                    role_default
                        .and_then(|r| r.timeout_secs)
                        .map(|secs| format!("Inherit ({secs}s)"))
                        .unwrap_or_else(|| "Inherit".to_owned())
                }),
            _ => String::new(),
        }
    }

    pub(super) fn subagent_role_names(&self) -> Vec<String> {
        let mut names = self.settings_defaults.subagent_role_names.clone();
        for name in self.preferences.subagent_roles.keys() {
            if !names.iter().any(|existing| existing == name) {
                names.push(name.clone());
            }
        }
        names
    }

    pub(super) fn cycle_provider_alias(
        &self,
        current: Option<&str>,
        direction: SettingDirection,
    ) -> Option<Option<String>> {
        let choices = &self.settings_defaults.provider_aliases;
        if choices.is_empty() {
            return None;
        }
        let current_idx = current
            .and_then(|provider| choices.iter().position(|choice| choice == provider))
            .map(|index| index + 1)
            .unwrap_or(0);
        let count = choices.len() + 1;
        let next = match direction {
            SettingDirection::Previous => (current_idx + count - 1) % count,
            SettingDirection::Next => (current_idx + 1) % count,
        };
        Some((next > 0).then(|| choices[next - 1].clone()))
    }

    pub(super) fn default_or_preferred_model(&self, provider: &str) -> Option<&str> {
        self.preferences.model_for_provider(provider).or_else(|| {
            self.settings_defaults
                .default_model_for_provider(Some(provider))
        })
    }
}

fn set_role_override_binding(
    slot: &mut Option<RoleModelOverride>,
    alias: &str,
    model: &str,
    reasoning_effort: Option<ReasoningEffort>,
) {
    let entry = slot.get_or_insert_with(RoleModelOverride::default);
    entry.provider = Some(alias.to_owned());
    entry.model = Some(model.to_owned());
    if let Some(effort) = reasoning_effort {
        entry.reasoning_effort = Some(effort);
    }
}

fn clear_role_provider_model(slot: &mut Option<RoleModelOverride>) {
    if let Some(entry) = slot.as_mut() {
        entry.provider = None;
        entry.model = None;
        if entry.is_empty() {
            *slot = None;
        }
    }
}

fn clear_role_reasoning(slot: &mut Option<RoleModelOverride>) {
    if let Some(entry) = slot.as_mut() {
        entry.reasoning_effort = None;
        if entry.is_empty() {
            *slot = None;
        }
    }
}

fn cycle_role_override_reasoning(
    slot: &mut Option<RoleModelOverride>,
    direction: SettingDirection,
) {
    let current = slot
        .as_ref()
        .and_then(|binding| binding.reasoning_effort.as_ref())
        .map(ReasoningEffort::as_str);
    let next = cycle_reasoning_preset(current, direction);
    let entry = slot.get_or_insert_with(RoleModelOverride::default);
    entry.reasoning_effort = next;
    if entry.is_empty() {
        *slot = None;
    }
}

pub(super) fn cycle_reasoning_preset(
    current: Option<&str>,
    direction: SettingDirection,
) -> Option<ReasoningEffort> {
    let current_idx = current
        .and_then(|effort| {
            REASONING_EFFORT_PRESETS
                .iter()
                .position(|value| *value == effort)
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let count = REASONING_EFFORT_PRESETS.len() + 1;
    let next = match direction {
        SettingDirection::Previous => (current_idx + count - 1) % count,
        SettingDirection::Next => (current_idx + 1) % count,
    };
    (next > 0).then(|| {
        ReasoningEffort::new(REASONING_EFFORT_PRESETS[next - 1])
            .expect("static reasoning effort should validate")
    })
}

fn format_pickable_model(override_model: Option<&str>, inherited: Option<&str>) -> String {
    match override_model {
        Some(model) => format!("{model} ▸"),
        None => match inherited {
            Some(model) => format!("Inherit ({model}) ▸"),
            None => "Inherit ▸".to_owned(),
        },
    }
}
