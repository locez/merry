mod defaults;
#[cfg(test)]
mod tests;

pub(crate) use defaults::TuiSettingsDefaults;

use crate::{
    coding::{AutoFallback, PermissionMode, SandboxMode},
    config::{
        BrandMarkMode, CopyButtonPosition, HeaderItemKind, MAX_MAX_READING_WIDTH,
        MAX_MOUSE_SCROLL_LINES, MIN_MAX_READING_WIDTH, ProviderAlias, TextSizingMode,
    },
    tui::theme::ThemePreset,
};
use merry::coding::SubagentLimitBehavior;
use merry::providers::{ModelName, ReasoningEffort, ServiceTier};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

const PREFERENCES_VERSION: u32 = 3;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) const REASONING_EFFORT_PRESETS: [&str; 7] =
    ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiProviderState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_tier: Option<ServiceTier>,
}

impl TuiProviderState {
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub(crate) fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }

    pub(crate) fn service_tier(&self) -> Option<ServiceTier> {
        self.service_tier
    }

    fn is_empty(&self) -> bool {
        self.model.is_none() && self.reasoning_effort.is_none() && self.service_tier.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoleModelOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

impl RoleModelOverride {
    pub(crate) fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none() && self.reasoning_effort.is_none()
    }

    fn validate(&self, field: &str) -> Result<(), PreferencesError> {
        if let Some(provider) = self.provider.as_deref() {
            ProviderAlias::new(provider)
                .map_err(|error| PreferencesError::Invalid(format!("{field}: {error}")))?;
        }
        if let Some(model) = self.model.as_deref() {
            ModelName::new(model)
                .map_err(|error| PreferencesError::Invalid(format!("{field}: {error}")))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubagentRolePreferenceOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_model_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout_secs: Option<u64>,
}

impl SubagentRolePreferenceOverride {
    pub(crate) fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.provider.is_none()
            && self.model.is_none()
            && self.reasoning_effort.is_none()
            && self.max_model_turns.is_none()
            && self.timeout_secs.is_none()
    }

    fn validate(&self, role: &str) -> Result<(), PreferencesError> {
        if let Some(provider) = self.provider.as_deref() {
            ProviderAlias::new(provider).map_err(|error| {
                PreferencesError::Invalid(format!("subagent_roles.{role}.provider: {error}"))
            })?;
        }
        if let Some(model) = self.model.as_deref() {
            ModelName::new(model).map_err(|error| {
                PreferencesError::Invalid(format!("subagent_roles.{role}.model: {error}"))
            })?;
        }
        if self.max_model_turns == Some(0) {
            return Err(PreferencesError::Invalid(format!(
                "subagent_roles.{role}.max_model_turns must be greater than zero"
            )));
        }
        if self.timeout_secs == Some(0) {
            return Err(PreferencesError::Invalid(format!(
                "subagent_roles.{role}.timeout_secs must be greater than zero"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiPermissionPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sandbox: Option<SandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) permission_mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) auto_fallback: Option<AutoFallback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) network: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ssh_agent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) gpg_agent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dbus: Option<bool>,
}

impl TuiPermissionPreferences {
    pub(crate) fn is_empty(&self) -> bool {
        self.sandbox.is_none()
            && self.permission_mode.is_none()
            && self.auto_fallback.is_none()
            && self.network.is_none()
            && self.ssh_agent.is_none()
            && self.gpg_agent.is_none()
            && self.dbus.is_none()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CodeTheme {
    #[default]
    Dracula,
    CatppuccinMocha,
    MonokaiBright,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CompactionStrategy {
    Compact,
    Balanced,
    PreserveDetail,
}

impl CompactionStrategy {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Balanced => "Balanced",
            Self::PreserveDetail => "Preserve detail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubagentLimitBehaviorPref {
    Queue,
    Fail,
}

impl SubagentLimitBehaviorPref {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::Fail => "Fail",
        }
    }
}

impl From<SubagentLimitBehaviorPref> for SubagentLimitBehavior {
    fn from(value: SubagentLimitBehaviorPref) -> Self {
        match value {
            SubagentLimitBehaviorPref::Queue => Self::Queue,
            SubagentLimitBehaviorPref::Fail => Self::Fail,
        }
    }
}

impl From<SubagentLimitBehavior> for SubagentLimitBehaviorPref {
    fn from(value: SubagentLimitBehavior) -> Self {
        match value {
            SubagentLimitBehavior::Queue => Self::Queue,
            SubagentLimitBehavior::Fail => Self::Fail,
        }
    }
}

impl CodeTheme {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Dracula => "Dracula",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::MonokaiBright => "Monokai Bright",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Dracula => Self::CatppuccinMocha,
            Self::CatppuccinMocha => Self::MonokaiBright,
            Self::MonokaiBright => Self::Dracula,
        }
    }

    pub(crate) fn previous(self) -> Self {
        match self {
            Self::Dracula => Self::MonokaiBright,
            Self::CatppuccinMocha => Self::Dracula,
            Self::MonokaiBright => Self::CatppuccinMocha,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiPreferences {
    #[serde(default = "preferences_version")]
    version: u32,
    #[serde(default)]
    pub(crate) code_theme: CodeTheme,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) theme_preset: Option<ThemePreset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) transparent_background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) animations: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) brand_mark: Option<BrandMarkMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_reading_width: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) text_sizing: Option<TextSizingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) header_items: Option<Vec<HeaderItemKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mouse_scroll_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) show_successful_command_output: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) copy_button_position: Option<CopyButtonPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    provider_states: BTreeMap<String, TuiProviderState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_compaction_model: Option<RoleModelOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval_review_model: Option<RoleModelOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subagent_default_model: Option<RoleModelOverride>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) subagent_roles: BTreeMap<String, SubagentRolePreferenceOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_window_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) auto_compaction_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) compaction_strategy: Option<CompactionStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) retained_model_turns: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subagents_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subagent_max_threads: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subagent_limit_behavior: Option<SubagentLimitBehaviorPref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_parallel_tool_calls: Option<usize>,
    #[serde(default, skip_serializing_if = "TuiPermissionPreferences::is_empty")]
    pub(crate) permissions: TuiPermissionPreferences,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) keymap: BTreeMap<String, String>,
}

impl Default for TuiPreferences {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            code_theme: CodeTheme::default(),
            theme_preset: None,
            transparent_background: None,
            animations: None,
            brand_mark: None,
            max_reading_width: None,
            text_sizing: None,
            header_items: None,
            mouse_scroll_lines: None,
            show_successful_command_output: None,
            copy_button_position: None,
            provider: None,
            provider_states: BTreeMap::new(),
            context_compaction_model: None,
            approval_review_model: None,
            subagent_default_model: None,
            subagent_roles: BTreeMap::new(),
            context_window_tokens: None,
            auto_compaction_enabled: None,
            compaction_strategy: None,
            retained_model_turns: None,
            subagents_enabled: None,
            subagent_max_threads: None,
            subagent_limit_behavior: None,
            max_parallel_tool_calls: None,
            permissions: TuiPermissionPreferences::default(),
            keymap: BTreeMap::new(),
        }
    }
}

impl TuiPreferences {
    pub(crate) fn reasoning_effort_for_provider(&self, provider: &str) -> Option<&ReasoningEffort> {
        self.provider_states
            .get(provider)
            .and_then(TuiProviderState::reasoning_effort)
    }

    pub(crate) fn reasoning_label_for_provider(&self, provider: &str) -> Option<&str> {
        self.reasoning_effort_for_provider(provider)
            .map(ReasoningEffort::as_str)
    }

    pub(crate) fn service_tier_for_provider(&self, provider: &str) -> Option<ServiceTier> {
        self.provider_states
            .get(provider)
            .and_then(TuiProviderState::service_tier)
    }

    pub(crate) fn model_for_provider(&self, provider: &str) -> Option<&str> {
        self.provider_states
            .get(provider)
            .and_then(TuiProviderState::model)
    }

    pub(crate) fn set_model_for_provider(
        &mut self,
        provider: &str,
        model: Option<&str>,
    ) -> Result<(), PreferencesError> {
        ProviderAlias::new(provider)
            .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        match model {
            Some(model) => {
                let model = ModelName::new(model)
                    .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
                self.provider_states
                    .entry(provider.to_owned())
                    .or_default()
                    .model = Some(model.as_str().to_owned());
            }
            None => {
                let should_remove = if let Some(state) = self.provider_states.get_mut(provider) {
                    state.model = None;
                    state.is_empty()
                } else {
                    false
                };
                if should_remove {
                    self.provider_states.remove(provider);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn set_reasoning_effort_for_provider(
        &mut self,
        provider: &str,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<(), PreferencesError> {
        ProviderAlias::new(provider)
            .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        let should_remove = {
            let state = self.provider_states.entry(provider.to_owned()).or_default();
            state.reasoning_effort = reasoning_effort;
            state.is_empty()
        };
        if should_remove {
            self.provider_states.remove(provider);
        }
        Ok(())
    }

    pub(crate) fn set_service_tier_for_provider(
        &mut self,
        provider: &str,
        service_tier: Option<ServiceTier>,
    ) -> Result<(), PreferencesError> {
        ProviderAlias::new(provider)
            .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        let should_remove = {
            let state = self.provider_states.entry(provider.to_owned()).or_default();
            state.service_tier = service_tier;
            state.is_empty()
        };
        if should_remove {
            self.provider_states.remove(provider);
        }
        Ok(())
    }

    pub(crate) fn clear_provider_state(&mut self, provider: &str) -> Result<(), PreferencesError> {
        ProviderAlias::new(provider)
            .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        self.provider_states.remove(provider);
        if self
            .context_compaction_model
            .as_ref()
            .and_then(|binding| binding.provider.as_deref())
            == Some(provider)
        {
            self.context_compaction_model = None;
        }
        if self
            .approval_review_model
            .as_ref()
            .and_then(|binding| binding.provider.as_deref())
            == Some(provider)
        {
            self.approval_review_model = None;
        }
        if let Some(default) = self.subagent_default_model.as_mut()
            && default.provider.as_deref() == Some(provider)
        {
            default.provider = None;
            default.model = None;
            if default.is_empty() {
                self.subagent_default_model = None;
            }
        }
        self.subagent_roles.retain(|_, role| {
            if role.provider.as_deref() == Some(provider) {
                role.provider = None;
                role.model = None;
            }
            !role.is_empty()
        });
        Ok(())
    }

    pub(crate) fn set_model_and_reasoning_for_provider(
        &mut self,
        provider: &str,
        model: &str,
        reasoning_effort: ReasoningEffort,
    ) -> Result<(), PreferencesError> {
        let model =
            ModelName::new(model).map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        ProviderAlias::new(provider)
            .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        let entry = self.provider_states.entry(provider.to_owned()).or_default();
        entry.model = Some(model.as_str().to_owned());
        entry.reasoning_effort = Some(reasoning_effort);
        Ok(())
    }
}

fn preferences_version() -> u32 {
    PREFERENCES_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiPreferencesStore {
    path: PathBuf,
}

impl TuiPreferencesStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) async fn load(&self) -> Result<TuiPreferences, PreferencesError> {
        self.load_with_default_provider(None).await
    }

    pub(crate) async fn load_with_default_provider(
        &self,
        default_provider: Option<&str>,
    ) -> Result<TuiPreferences, PreferencesError> {
        let text = match tokio::fs::read_to_string(&self.path).await {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(TuiPreferences::default());
            }
            Err(source) => return Err(io_error("read", &self.path, source)),
        };
        let version = toml::from_str::<PreferencesVersion>(&text)
            .map_err(|source| PreferencesError::Parse {
                path: self.path.clone(),
                source: Box::new(source),
            })?
            .version;
        let preferences = match version {
            1 => toml::from_str::<LegacyTuiPreferences>(&text)
                .map_err(|source| PreferencesError::Parse {
                    path: self.path.clone(),
                    source: Box::new(source),
                })?
                .migrate(default_provider)?,
            2 => toml::from_str::<StoredTuiPreferencesV2>(&text)
                .map_err(|source| PreferencesError::Parse {
                    path: self.path.clone(),
                    source: Box::new(source),
                })?
                .migrate(default_provider)?,
            PREFERENCES_VERSION => toml::from_str::<TuiPreferences>(&text).map_err(|source| {
                PreferencesError::Parse {
                    path: self.path.clone(),
                    source: Box::new(source),
                }
            })?,
            path_version => {
                return Err(PreferencesError::UnsupportedVersion {
                    path_version,
                    supported_version: PREFERENCES_VERSION,
                });
            }
        };
        preferences.validate()?;
        Ok(preferences)
    }

    pub(crate) async fn save(&self, preferences: &TuiPreferences) -> Result<(), PreferencesError> {
        preferences.validate()?;
        let text = toml::to_string_pretty(preferences).map_err(PreferencesError::Serialize)?;
        let parent = self.path.parent().ok_or_else(|| {
            PreferencesError::Invalid(format!(
                "preferences path {} has no parent directory",
                self.path.display()
            ))
        })?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| io_error("create parent directory for", parent, source))?;
        let temp_path = parent.join(format!(
            ".tui-preferences.toml.tmp-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|source| io_error("create temporary", &temp_path, source))?;
        file.write_all(text.as_bytes())
            .await
            .map_err(|source| io_error("write temporary", &temp_path, source))?;
        file.sync_all()
            .await
            .map_err(|source| io_error("sync temporary", &temp_path, source))?;
        drop(file);
        if let Err(source) = tokio::fs::rename(&temp_path, &self.path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(io_error("replace", &self.path, source));
        }
        Ok(())
    }
}

impl TuiPreferences {
    fn validate(&self) -> Result<(), PreferencesError> {
        if self.version != PREFERENCES_VERSION {
            return Err(PreferencesError::UnsupportedVersion {
                path_version: self.version,
                supported_version: PREFERENCES_VERSION,
            });
        }
        if let Some(provider) = self.provider.as_deref() {
            ProviderAlias::new(provider)
                .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        }
        for (provider, state) in &self.provider_states {
            ProviderAlias::new(provider)
                .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
            if let Some(model) = state.model() {
                ModelName::new(model)
                    .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
            }
        }
        if let Some(binding) = self.context_compaction_model.as_ref() {
            binding.validate("context_compaction_model")?;
        }
        if let Some(binding) = self.approval_review_model.as_ref() {
            binding.validate("approval_review_model")?;
        }
        if let Some(binding) = self.subagent_default_model.as_ref() {
            binding.validate("subagent_default_model")?;
        }
        for (role, overrides) in &self.subagent_roles {
            if role.trim().is_empty() {
                return Err(PreferencesError::Invalid(
                    "subagent_roles keys must not be blank".to_owned(),
                ));
            }
            overrides.validate(role)?;
        }
        if let Some(width) = self.max_reading_width
            && width != 0
            && !(MIN_MAX_READING_WIDTH..=MAX_MAX_READING_WIDTH).contains(&width)
        {
            return Err(PreferencesError::Invalid(format!(
                "max_reading_width must be 0 or between {MIN_MAX_READING_WIDTH} and {MAX_MAX_READING_WIDTH}"
            )));
        }
        if let Some(lines) = self.mouse_scroll_lines
            && !(1..=MAX_MOUSE_SCROLL_LINES).contains(&lines)
        {
            return Err(PreferencesError::Invalid(format!(
                "mouse_scroll_lines must be between 1 and {MAX_MOUSE_SCROLL_LINES}"
            )));
        }
        if self.retained_model_turns == Some(0) {
            return Err(PreferencesError::Invalid(
                "retained_model_turns must be greater than zero".to_owned(),
            ));
        }
        if self.max_parallel_tool_calls == Some(0) {
            return Err(PreferencesError::Invalid(
                "max_parallel_tool_calls must be greater than zero".to_owned(),
            ));
        }
        if self.subagent_max_threads == Some(0) {
            return Err(PreferencesError::Invalid(
                "subagent_max_threads must be greater than zero".to_owned(),
            ));
        }
        if self.context_window_tokens == Some(0) {
            return Err(PreferencesError::Invalid(
                "context_window_tokens must be greater than zero".to_owned(),
            ));
        }
        for (action_key, binding) in &self.keymap {
            if crate::tui::keymap::KeyAction::from_config_key(action_key).is_none() {
                return Err(PreferencesError::Invalid(format!(
                    "unknown keymap action {action_key:?}"
                )));
            }
            crate::tui::keymap::parse_binding_str(binding)
                .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct PreferencesVersion {
    #[serde(default = "legacy_preferences_version")]
    version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTuiPreferencesV2 {
    #[serde(default = "legacy_preferences_version")]
    version: u32,
    #[serde(default)]
    code_theme: CodeTheme,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    models: BTreeMap<String, String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    context_window_tokens: Option<u64>,
    #[serde(default)]
    auto_compaction_enabled: Option<bool>,
    #[serde(default)]
    compaction_strategy: Option<CompactionStrategy>,
    #[serde(default)]
    subagents_enabled: Option<bool>,
    #[serde(default)]
    subagent_max_threads: Option<usize>,
}

impl StoredTuiPreferencesV2 {
    fn migrate(self, default_provider: Option<&str>) -> Result<TuiPreferences, PreferencesError> {
        if self.version != 2 {
            return Err(PreferencesError::UnsupportedVersion {
                path_version: self.version,
                supported_version: PREFERENCES_VERSION,
            });
        }
        LegacyPreferencesMigration {
            provider: self.provider,
            models: self.models,
            reasoning_effort: self.reasoning_effort,
            code_theme: self.code_theme,
            context_window_tokens: self.context_window_tokens,
            auto_compaction_enabled: self.auto_compaction_enabled,
            compaction_strategy: self.compaction_strategy,
            subagents_enabled: self.subagents_enabled,
            subagent_max_threads: self.subagent_max_threads,
        }
        .into_preferences(default_provider)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTuiPreferences {
    #[serde(default = "legacy_preferences_version")]
    version: u32,
    #[serde(default)]
    code_theme: CodeTheme,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    subagents_enabled: Option<bool>,
    #[serde(default)]
    subagent_max_threads: Option<usize>,
}

impl LegacyTuiPreferences {
    fn migrate(self, default_provider: Option<&str>) -> Result<TuiPreferences, PreferencesError> {
        if self.version != 1 {
            return Err(PreferencesError::UnsupportedVersion {
                path_version: self.version,
                supported_version: PREFERENCES_VERSION,
            });
        }
        let mut models = BTreeMap::new();
        if let Some(model) = self.model {
            let provider = self
                .provider
                .as_deref()
                .or(default_provider)
                .ok_or_else(|| {
                    PreferencesError::Invalid(
                        "version 1 model preference cannot migrate without a provider".to_owned(),
                    )
                })?;
            models.insert(provider.to_owned(), model);
        }
        LegacyPreferencesMigration {
            provider: self.provider,
            models,
            reasoning_effort: self.reasoning_effort,
            code_theme: self.code_theme,
            context_window_tokens: None,
            auto_compaction_enabled: None,
            compaction_strategy: None,
            subagents_enabled: self.subagents_enabled,
            subagent_max_threads: self.subagent_max_threads,
        }
        .into_preferences(default_provider)
    }
}

struct LegacyPreferencesMigration {
    provider: Option<String>,
    models: BTreeMap<String, String>,
    reasoning_effort: Option<ReasoningEffort>,
    code_theme: CodeTheme,
    context_window_tokens: Option<u64>,
    auto_compaction_enabled: Option<bool>,
    compaction_strategy: Option<CompactionStrategy>,
    subagents_enabled: Option<bool>,
    subagent_max_threads: Option<usize>,
}

impl LegacyPreferencesMigration {
    fn into_preferences(
        self,
        default_provider: Option<&str>,
    ) -> Result<TuiPreferences, PreferencesError> {
        let mut provider_states = self
            .models
            .into_iter()
            .map(|(provider, model)| {
                (
                    provider,
                    TuiProviderState {
                        model: Some(model),
                        reasoning_effort: None,
                        service_tier: None,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if let Some(reasoning_effort) = self.reasoning_effort
            && let Some(provider) = self.provider.as_deref().or(default_provider)
        {
            provider_states
                .entry(provider.to_owned())
                .or_default()
                .reasoning_effort = Some(reasoning_effort);
        }
        let preferences = TuiPreferences {
            version: PREFERENCES_VERSION,
            code_theme: self.code_theme,
            provider: self.provider,
            provider_states,
            context_window_tokens: self.context_window_tokens,
            auto_compaction_enabled: self.auto_compaction_enabled,
            compaction_strategy: self.compaction_strategy,
            subagents_enabled: self.subagents_enabled,
            subagent_max_threads: self.subagent_max_threads,
            ..TuiPreferences::default()
        };
        preferences.validate()?;
        Ok(preferences)
    }
}

fn legacy_preferences_version() -> u32 {
    1
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> PreferencesError {
    PreferencesError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Error)]
pub(crate) enum PreferencesError {
    #[error("failed to {operation} TUI preferences at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse TUI preferences at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("failed to serialize TUI preferences: {0}")]
    Serialize(#[source] toml::ser::Error),
    #[error(
        "unsupported TUI preferences version {path_version}; this Merry supports version {supported_version}"
    )]
    UnsupportedVersion {
        path_version: u32,
        supported_version: u32,
    },
    #[error("invalid TUI preferences: {0}")]
    Invalid(String),
}
