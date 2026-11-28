use super::{RoleModelOverride, SubagentLimitBehaviorPref};
use crate::{
    cli::ExplicitCliFlags,
    coding::{AutoFallback, PermissionMode, SandboxMode},
    config::{
        BrandMarkMode, ConfiguredProviderKind, CopyButtonPosition, DEFAULT_MAX_READING_WIDTH,
        DEFAULT_MOUSE_SCROLL_LINES, HeaderItemKind, TextSizingMode,
    },
    tui::theme::ThemePreset,
};
use merry::permissions::HostIntegration;
use merry::providers::OpenAiProtocol;
use merry::providers::{ReasoningEffort, ServiceTier};
use std::collections::BTreeMap;

const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 8;
pub(crate) const BUILTIN_SUBAGENT_ROLES: [&str; 3] = ["explore", "coder", "review"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubagentRoleDefaults {
    pub(crate) enabled: bool,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) max_model_turns: Option<u32>,
    pub(crate) timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiSettingsDefaults {
    pub(crate) theme_preset: ThemePreset,
    pub(crate) transparent_background: bool,
    pub(crate) animations: bool,
    pub(crate) brand_mark: BrandMarkMode,
    pub(crate) max_reading_width: u16,
    pub(crate) text_sizing: TextSizingMode,
    pub(crate) header_items: Vec<HeaderItemKind>,
    pub(crate) mouse_scroll_lines: usize,
    pub(crate) show_successful_command_output: bool,
    pub(crate) copy_button_position: CopyButtonPosition,
    pub(crate) provider_aliases: Vec<String>,
    pub(crate) provider_kinds: BTreeMap<String, ConfiguredProviderKind>,
    pub(crate) provider_protocols: BTreeMap<String, OpenAiProtocol>,
    pub(crate) provider_models: BTreeMap<String, String>,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_efforts: BTreeMap<String, ReasoningEffort>,
    pub(crate) service_tiers: BTreeMap<String, ServiceTier>,
    pub(crate) context_compaction_model: Option<(String, String)>,
    pub(crate) approval_review_model: Option<(String, String)>,
    pub(crate) context_window_tokens: u64,
    pub(crate) subagents_enabled: bool,
    pub(crate) subagent_max_threads: usize,
    pub(crate) subagent_limit_behavior: SubagentLimitBehaviorPref,
    pub(crate) subagent_default_model: RoleModelOverride,
    pub(crate) subagent_role_names: Vec<String>,
    pub(crate) subagent_roles: BTreeMap<String, SubagentRoleDefaults>,
    pub(crate) max_parallel_tool_calls: usize,
    pub(crate) auto_compaction_enabled: bool,
    pub(crate) compaction_strategy: String,
    pub(crate) retained_model_turns: usize,
    pub(crate) sandbox: SandboxMode,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) auto_fallback: AutoFallback,
    pub(crate) explicit_cli_flags: ExplicitCliFlags,
    pub(crate) network_allowed: bool,
    pub(crate) ssh_agent: bool,
    pub(crate) gpg_agent: bool,
    pub(crate) dbus: bool,
}

impl Default for TuiSettingsDefaults {
    fn default() -> Self {
        let limits = merry::coding::SubagentConfig::default();
        let compaction = merry::CompactionConfig::default();
        let subagent_role_names = BUILTIN_SUBAGENT_ROLES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        let subagent_roles = subagent_role_names
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    SubagentRoleDefaults {
                        enabled: true,
                        ..SubagentRoleDefaults::default()
                    },
                )
            })
            .collect();
        Self {
            theme_preset: ThemePreset::default(),
            transparent_background: false,
            animations: true,
            brand_mark: BrandMarkMode::default(),
            max_reading_width: DEFAULT_MAX_READING_WIDTH,
            text_sizing: TextSizingMode::default(),
            header_items: HeaderItemKind::DEFAULT_ITEMS.to_vec(),
            mouse_scroll_lines: DEFAULT_MOUSE_SCROLL_LINES,
            show_successful_command_output: true,
            copy_button_position: CopyButtonPosition::default(),
            provider_aliases: Vec::new(),
            provider_kinds: BTreeMap::new(),
            provider_protocols: BTreeMap::new(),
            provider_models: BTreeMap::new(),
            provider: None,
            model: None,
            reasoning_efforts: BTreeMap::new(),
            service_tiers: BTreeMap::new(),
            context_compaction_model: None,
            approval_review_model: None,
            context_window_tokens: merry::coding::DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS,
            subagents_enabled: true,
            subagent_max_threads: limits.max_threads(),
            subagent_limit_behavior: limits.limit_behavior().into(),
            subagent_default_model: RoleModelOverride::default(),
            subagent_role_names,
            subagent_roles,
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            auto_compaction_enabled: true,
            compaction_strategy: "Balanced".to_owned(),
            retained_model_turns: compaction.policy().retained_model_turns(),
            sandbox: SandboxMode::default(),
            permission_mode: PermissionMode::default(),
            auto_fallback: AutoFallback::default(),
            explicit_cli_flags: ExplicitCliFlags::default(),
            network_allowed: true,
            ssh_agent: false,
            gpg_agent: false,
            dbus: false,
        }
    }
}

impl TuiSettingsDefaults {
    pub(crate) fn from_config(
        config: Option<&crate::config::MerryConfig>,
    ) -> Result<Self, crate::config::ConfigError> {
        let Some(config) = config else {
            return Ok(Self::default());
        };
        let default_provider = config.configured_default_provider()?;
        let provider_aliases = config.provider_aliases();
        let mut provider_kinds = BTreeMap::new();
        let mut provider_protocols = BTreeMap::new();
        let mut provider_models = BTreeMap::new();
        let mut reasoning_efforts = BTreeMap::new();
        let mut service_tiers = BTreeMap::new();
        for alias in &provider_aliases {
            if let Ok(profile) = config.provider_profile(alias) {
                provider_kinds.insert(alias.clone(), profile.kind());
                if let Some(protocol) = profile.protocol() {
                    provider_protocols.insert(alias.clone(), protocol);
                }
                if let Some(model) = profile.default_model() {
                    provider_models.insert(alias.clone(), model.as_str().to_owned());
                }
            }
            if let Some(reasoning_effort) = config.effective_provider_reasoning_effort(alias)? {
                reasoning_efforts.insert(alias.clone(), reasoning_effort);
            }
            if let Some(service_tier) = config.effective_provider_service_tier(alias)? {
                service_tiers.insert(alias.clone(), service_tier);
            }
        }
        let runtime_models = config.runtime_models()?;
        let subagents = config.subagents_config()?;
        let mut subagent_role_names: Vec<String> = BUILTIN_SUBAGENT_ROLES
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        for name in subagents.roles().keys() {
            if !subagent_role_names.iter().any(|existing| existing == name) {
                subagent_role_names.push(name.clone());
            }
        }
        let mut subagent_roles = BTreeMap::new();
        for name in &subagent_role_names {
            let role_cfg = subagents.roles().get(name);
            let toggled = subagents.toggle().get(name).copied();
            let enabled = toggled
                .or_else(|| role_cfg.and_then(|cfg| cfg.enabled))
                .unwrap_or(true);
            subagent_roles.insert(
                name.clone(),
                SubagentRoleDefaults {
                    enabled,
                    provider: role_cfg.and_then(|cfg| cfg.provider.clone()),
                    model: role_cfg.and_then(|cfg| cfg.model.clone()),
                    reasoning_effort: role_cfg.and_then(|cfg| cfg.reasoning_effort.clone()),
                    max_model_turns: role_cfg.and_then(|cfg| cfg.max_model_turns),
                    timeout_secs: role_cfg.and_then(|cfg| cfg.timeout_secs),
                },
            );
        }
        let subagent_default_model = RoleModelOverride {
            provider: subagents.default_config().provider.clone(),
            model: subagents.default_config().model.clone(),
            reasoning_effort: subagents.default_config().reasoning_effort.clone(),
        };
        let auto_compaction = config.automatic_compaction_config()?;
        let tui = config.tui_config()?;
        let theme_preset = tui.theme.preset.unwrap_or_default();
        let transparent_background = tui.theme.transparent.unwrap_or(false);
        let cli_defaults = config.cli_defaults()?;
        let max_parallel_tool_calls = config
            .max_parallel_tool_calls()?
            .map_or(DEFAULT_MAX_PARALLEL_TOOL_CALLS, std::num::NonZeroUsize::get);
        let integrations = config.host_integrations();
        Ok(Self {
            theme_preset,
            transparent_background,
            animations: tui.animations,
            brand_mark: tui.brand_mark,
            max_reading_width: tui.max_reading_width,
            text_sizing: tui.text_sizing,
            header_items: tui.header_items,
            mouse_scroll_lines: tui.mouse_scroll_lines,
            show_successful_command_output: tui.show_successful_command_output,
            copy_button_position: tui.copy_button_position,
            provider_aliases,
            provider_kinds,
            provider_protocols,
            provider_models,
            provider: default_provider
                .as_ref()
                .map(|provider| provider.alias.clone()),
            model: default_provider
                .as_ref()
                .map(|provider| provider.model.clone()),
            reasoning_efforts,
            service_tiers,
            context_compaction_model: runtime_models
                .context_compaction
                .map(|model| (model.provider, model.model)),
            approval_review_model: runtime_models
                .approval_review
                .map(|model| (model.provider, model.model)),
            context_window_tokens: merry::coding::DEFAULT_CONTEXT_WINDOW_FALLBACK_TOKENS,
            subagents_enabled: subagents.is_enabled(),
            subagent_max_threads: subagents.limits().max_threads(),
            subagent_limit_behavior: subagents.limits().limit_behavior().into(),
            subagent_default_model,
            subagent_role_names,
            subagent_roles,
            max_parallel_tool_calls,
            auto_compaction_enabled: auto_compaction.is_enabled(),
            compaction_strategy: "Config".to_owned(),
            retained_model_turns: auto_compaction.policy().retained_model_turns(),
            sandbox: cli_defaults.sandbox().unwrap_or_default(),
            permission_mode: cli_defaults.permission_mode().unwrap_or_default(),
            auto_fallback: cli_defaults.auto_fallback().unwrap_or_default(),
            explicit_cli_flags: ExplicitCliFlags::default(),
            network_allowed: config.network_requests_allowed(),
            ssh_agent: integrations.contains(&HostIntegration::SshAgent),
            gpg_agent: integrations.contains(&HostIntegration::GpgAgent),
            dbus: integrations.contains(&HostIntegration::SessionBus),
        })
    }

    pub(crate) fn with_explicit_cli_flags(mut self, flags: ExplicitCliFlags) -> Self {
        self.explicit_cli_flags = flags;
        self
    }

    pub(crate) fn reasoning_effort_for_provider(
        &self,
        provider: Option<&str>,
    ) -> Option<&ReasoningEffort> {
        let provider = provider.or(self.provider.as_deref())?;
        self.reasoning_efforts.get(provider)
    }

    pub(crate) fn service_tier_for_provider(&self, provider: Option<&str>) -> Option<ServiceTier> {
        let provider = provider.or(self.provider.as_deref())?;
        self.service_tiers.get(provider).copied()
    }

    pub(crate) fn provider_kind(&self, provider: Option<&str>) -> Option<ConfiguredProviderKind> {
        let provider = provider.or(self.provider.as_deref())?;
        self.provider_kinds.get(provider).copied()
    }

    pub(crate) fn provider_protocol(&self, provider: Option<&str>) -> Option<OpenAiProtocol> {
        let provider = provider.or(self.provider.as_deref())?;
        self.provider_protocols.get(provider).copied()
    }

    pub(crate) fn default_model_for_provider(&self, provider: Option<&str>) -> Option<&str> {
        let provider = provider.or(self.provider.as_deref())?;
        if self.provider.as_deref() == Some(provider)
            && let Some(model) = self.model.as_deref()
        {
            return Some(model);
        }
        self.provider_models.get(provider).map(String::as_str)
    }
}
