//! Unified `[subagents]` configuration and `.merry/roles/*.toml` project discovery.

use super::{
    ConfigError, MerryConfig, default_true, provider::parse_reasoning_effort, validate_model_text,
};
use merry::profiles::{
    DEFAULT_CODING_SUBAGENT_MAX_MODEL_TURNS, MIN_CODING_SUBAGENT_MODEL_TURNS,
    SubagentWorkspaceAccess,
};
use merry_core::ToolName;
use merry_llm::ReasoningEffort;
use merry_runtime::{DEFAULT_FOREGROUND_WAIT_BUDGET_SECS, SubagentLimitBehavior};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Maximum size of a `.merry/roles/<name>.toml` file.
const MAX_ROLE_TOML_BYTES: usize = 64 * 1024;

impl MerryConfig {
    /// Returns validated `[subagents]` configuration.
    ///
    /// Omitting `[subagents]` or omitting `enabled` defaults to enabled (`true`)
    /// so tuning limits or role models never silently disables subagents.
    pub fn subagents_config(&self) -> Result<SubagentsConfig, ConfigError> {
        let Some(subagents) = self.raw.subagents.as_ref() else {
            return Ok(SubagentsConfig::default());
        };
        subagents.to_config(self)
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SubagentLimitBehaviorToml {
    Queue,
    Fail,
}

impl From<SubagentLimitBehaviorToml> for SubagentLimitBehavior {
    fn from(value: SubagentLimitBehaviorToml) -> Self {
        match value {
            SubagentLimitBehaviorToml::Queue => Self::Queue,
            SubagentLimitBehaviorToml::Fail => Self::Fail,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SubagentWorkspaceAccessToml {
    ReadOnly,
    ReadWrite,
}

impl From<SubagentWorkspaceAccessToml> for SubagentWorkspaceAccess {
    fn from(value: SubagentWorkspaceAccessToml) -> Self {
        match value {
            SubagentWorkspaceAccessToml::ReadOnly => Self::ReadOnlyWorkspace,
            SubagentWorkspaceAccessToml::ReadWrite => Self::ReadWriteWorkspace,
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct SubagentsToml {
    #[serde(default = "default_true")]
    enabled: bool,
    max_threads: Option<usize>,
    max_depth: Option<u8>,
    foreground_budget_secs: Option<u64>,
    limit_behavior: Option<SubagentLimitBehaviorToml>,
    #[serde(default)]
    toggle: BTreeMap<String, bool>,
    default: Option<SubagentDefaultToml>,
    #[serde(default)]
    roles: BTreeMap<String, SubagentRoleToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SubagentDefaultToml {
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    max_model_turns: Option<u32>,
    timeout_secs: Option<u64>,
    max_total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SubagentRoleToml {
    base: Option<String>,
    enabled: Option<bool>,
    description: Option<String>,
    workspace_access: Option<SubagentWorkspaceAccessToml>,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    max_model_turns: Option<u32>,
    timeout_secs: Option<u64>,
    max_total_tokens: Option<u64>,
    allowed_tools: Option<Vec<String>>,
    prompt_file: Option<String>,
    instructions: Option<String>,
}

/// Effective validated `[subagents.default]` overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveSubagentDefaultConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_model_turns: Option<u32>,
    pub timeout_secs: Option<u64>,
    pub max_total_tokens: Option<u64>,
}

/// Effective validated role overlay (from `[subagents.roles.<name>]` or `.merry/roles/<name>.toml`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveSubagentRoleConfig {
    pub base: Option<String>,
    pub enabled: Option<bool>,
    pub description: Option<String>,
    pub workspace_access: Option<SubagentWorkspaceAccess>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_model_turns: Option<u32>,
    pub timeout_secs: Option<u64>,
    pub max_total_tokens: Option<u64>,
    pub allowed_tools: Option<Vec<ToolName>>,
    pub prompt_file: Option<String>,
    pub instructions: Option<String>,
}

/// Validated `[subagents]` configuration produced by `MerryConfig::subagents_config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentsConfig {
    enabled: bool,
    limits: merry_runtime::SubagentConfig,
    toggle: BTreeMap<String, bool>,
    default: EffectiveSubagentDefaultConfig,
    roles: BTreeMap<String, EffectiveSubagentRoleConfig>,
}

impl Default for SubagentsConfig {
    fn default() -> Self {
        let limits = merry_runtime::SubagentConfig::default()
            .with_model_turn_bounds(
                MIN_CODING_SUBAGENT_MODEL_TURNS,
                DEFAULT_CODING_SUBAGENT_MAX_MODEL_TURNS,
            )
            .expect("coding subagent default model-turn bounds are valid")
            .with_foreground_wait_budget_secs(DEFAULT_FOREGROUND_WAIT_BUDGET_SECS)
            .expect("default foreground budget is valid");
        Self {
            enabled: true,
            limits,
            toggle: BTreeMap::new(),
            default: EffectiveSubagentDefaultConfig::default(),
            roles: BTreeMap::new(),
        }
    }
}

impl SubagentsConfig {
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn limits(&self) -> merry_runtime::SubagentConfig {
        self.limits
    }

    #[must_use]
    pub fn toggle(&self) -> &BTreeMap<String, bool> {
        &self.toggle
    }

    #[must_use]
    pub fn default_config(&self) -> &EffectiveSubagentDefaultConfig {
        &self.default
    }

    #[must_use]
    pub fn roles(&self) -> &BTreeMap<String, EffectiveSubagentRoleConfig> {
        &self.roles
    }

    pub fn with_overrides(
        mut self,
        enabled: Option<bool>,
        max_threads: Option<usize>,
        limit_behavior: Option<SubagentLimitBehavior>,
    ) -> Result<Self, ConfigError> {
        let mut limits = merry_runtime::SubagentConfig::new(
            max_threads.unwrap_or_else(|| self.limits.max_threads()),
            self.limits.max_depth(),
        )
        .map_err(|error| ConfigError::Invalid(error.to_string()))?
        .with_model_turn_bounds(self.limits.min_model_turns(), self.limits.max_model_turns())
        .map_err(|error| ConfigError::Invalid(error.to_string()))?
        .with_default_timeout_secs(self.limits.default_timeout_secs())
        .map_err(|error| ConfigError::Invalid(error.to_string()))?
        .with_foreground_wait_budget_secs(self.limits.foreground_wait_budget_secs())
        .map_err(|error| ConfigError::Invalid(error.to_string()))?
        .with_max_total_tokens(self.limits.max_total_tokens())
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        limits = limits
            .with_limit_behavior(limit_behavior.unwrap_or_else(|| self.limits.limit_behavior()));
        self.enabled = enabled.unwrap_or(self.enabled);
        self.limits = limits;
        Ok(self)
    }

    pub(crate) fn apply_tui_overrides(
        &mut self,
        default_provider: Option<&str>,
        default_model: Option<&str>,
        default_reasoning: Option<ReasoningEffort>,
        roles: &BTreeMap<String, crate::tui::preferences::SubagentRolePreferenceOverride>,
    ) -> Result<(), ConfigError> {
        if let Some(provider) = default_provider {
            self.default.provider = Some(provider.to_owned());
        }
        if let Some(model) = default_model {
            self.default.model = Some(model.to_owned());
        }
        if let Some(reasoning) = default_reasoning {
            self.default.reasoning_effort = Some(reasoning);
        }
        let mut highest_turns = self.limits.max_model_turns();
        for (name, role_override) in roles {
            if let Some(enabled) = role_override.enabled {
                self.toggle.insert(name.clone(), enabled);
            }
            if role_override.provider.is_some()
                || role_override.model.is_some()
                || role_override.reasoning_effort.is_some()
                || role_override.max_model_turns.is_some()
                || role_override.timeout_secs.is_some()
            {
                let entry = self.roles.entry(name.clone()).or_default();
                if let Some(provider) = role_override.provider.as_ref() {
                    entry.provider = Some(provider.clone());
                }
                if let Some(model) = role_override.model.as_ref() {
                    entry.model = Some(model.clone());
                }
                if let Some(reasoning) = role_override.reasoning_effort.as_ref() {
                    entry.reasoning_effort = Some(reasoning.clone());
                }
                if let Some(turns) = role_override.max_model_turns {
                    entry.max_model_turns = Some(turns);
                    highest_turns = highest_turns.max(turns);
                }
                if let Some(timeout) = role_override.timeout_secs {
                    entry.timeout_secs = Some(timeout);
                }
            }
        }
        if highest_turns > self.limits.max_model_turns() {
            self.limits = self
                .limits
                .with_model_turn_bounds(self.limits.min_model_turns(), highest_turns)
                .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        }
        Ok(())
    }
}

impl SubagentsToml {
    fn to_config(&self, config: &MerryConfig) -> Result<SubagentsConfig, ConfigError> {
        let defaults = SubagentsConfig::default().limits;
        let default_cfg = self
            .default
            .as_ref()
            .map(|d| d.to_effective(config))
            .transpose()?
            .unwrap_or_default();

        let mut roles = BTreeMap::new();
        let mut highest_role_turns = default_cfg
            .max_model_turns
            .unwrap_or_else(|| defaults.max_model_turns());
        for (name, role_toml) in &self.roles {
            validate_role_identifier(name)?;
            let role = role_toml.to_effective(name, Some(config))?;
            if let Some(turns) = role.max_model_turns {
                highest_role_turns = highest_role_turns.max(turns);
            }
            roles.insert(name.clone(), role);
        }

        let mut limits = merry_runtime::SubagentConfig::new(
            self.max_threads.unwrap_or(defaults.max_threads()),
            self.max_depth.unwrap_or(defaults.max_depth()),
        )
        .map_err(|error| ConfigError::Invalid(error.to_string()))?
        .with_model_turn_bounds(MIN_CODING_SUBAGENT_MODEL_TURNS, highest_role_turns)
        .map_err(|error| ConfigError::Invalid(error.to_string()))?
        .with_default_timeout_secs(default_cfg.timeout_secs)
        .map_err(|error| ConfigError::Invalid(error.to_string()))?
        .with_foreground_wait_budget_secs(
            self.foreground_budget_secs
                .unwrap_or(DEFAULT_FOREGROUND_WAIT_BUDGET_SECS),
        )
        .map_err(|error| ConfigError::Invalid(error.to_string()))?
        .with_max_total_tokens(default_cfg.max_total_tokens)
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        if let Some(behavior) = self.limit_behavior {
            limits = limits.with_limit_behavior(behavior.into());
        }

        Ok(SubagentsConfig {
            enabled: self.enabled,
            limits,
            toggle: self.toggle.clone(),
            default: default_cfg,
            roles,
        })
    }
}

impl SubagentDefaultToml {
    fn to_effective(
        &self,
        config: &MerryConfig,
    ) -> Result<EffectiveSubagentDefaultConfig, ConfigError> {
        if let Some(model) = self.model.as_deref() {
            validate_model_text("subagents.default.model", model)?;
        }
        if let Some(provider) = self.provider.as_deref() {
            config.validate_provider_alias(provider).map_err(|error| {
                ConfigError::Invalid(format!(
                    "invalid provider {provider:?} for [subagents.default]: {error}"
                ))
            })?;
        }
        if self.provider.is_some() && self.model.is_none() {
            return Err(ConfigError::Invalid(
                "[subagents.default].model is required when provider is set".to_owned(),
            ));
        }
        let reasoning_effort = parse_reasoning_effort(
            "subagents.default.reasoning_effort",
            self.reasoning_effort.as_deref(),
        )?;
        if self.max_model_turns == Some(0) {
            return Err(ConfigError::Invalid(
                "subagents.default.max_model_turns must be greater than zero".to_owned(),
            ));
        }
        if self.timeout_secs == Some(0) {
            return Err(ConfigError::Invalid(
                "subagents.default.timeout_secs must be greater than zero".to_owned(),
            ));
        }
        if self.max_total_tokens == Some(0) {
            return Err(ConfigError::Invalid(
                "subagents.default.max_total_tokens must be greater than zero".to_owned(),
            ));
        }
        Ok(EffectiveSubagentDefaultConfig {
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning_effort,
            max_model_turns: self.max_model_turns,
            timeout_secs: self.timeout_secs,
            max_total_tokens: self.max_total_tokens,
        })
    }
}

impl SubagentRoleToml {
    fn to_effective(
        &self,
        role_name: &str,
        config: Option<&MerryConfig>,
    ) -> Result<EffectiveSubagentRoleConfig, ConfigError> {
        if let Some(model) = self.model.as_deref() {
            validate_model_text(&format!("subagents.roles.{role_name}.model"), model)?;
        }
        if let (Some(provider), Some(cfg)) = (self.provider.as_deref(), config) {
            cfg.validate_provider_alias(provider).map_err(|error| {
                ConfigError::Invalid(format!(
                    "invalid provider {provider:?} for [subagents.roles.{role_name}]: {error}"
                ))
            })?;
        }
        if self.provider.is_some() && self.model.is_none() {
            return Err(ConfigError::Invalid(format!(
                "[subagents.roles.{role_name}].model is required when provider is set"
            )));
        }
        let reasoning_effort = parse_reasoning_effort(
            &format!("subagents.roles.{role_name}.reasoning_effort"),
            self.reasoning_effort.as_deref(),
        )?;
        if self.max_model_turns == Some(0) {
            return Err(ConfigError::Invalid(format!(
                "subagents.roles.{role_name}.max_model_turns must be greater than zero"
            )));
        }
        if self.timeout_secs == Some(0) {
            return Err(ConfigError::Invalid(format!(
                "subagents.roles.{role_name}.timeout_secs must be greater than zero"
            )));
        }
        if self.max_total_tokens == Some(0) {
            return Err(ConfigError::Invalid(format!(
                "subagents.roles.{role_name}.max_total_tokens must be greater than zero"
            )));
        }
        let allowed_tools = self
            .allowed_tools
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .map(|name| {
                        ToolName::new(name).map_err(|error| {
                            ConfigError::Invalid(format!(
                                "invalid tool name {name:?} in [subagents.roles.{role_name}].allowed_tools: {error}"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(EffectiveSubagentRoleConfig {
            base: self.base.clone(),
            enabled: self.enabled,
            description: self.description.clone(),
            workspace_access: self.workspace_access.map(Into::into),
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning_effort,
            max_model_turns: self.max_model_turns,
            timeout_secs: self.timeout_secs,
            max_total_tokens: self.max_total_tokens,
            allowed_tools,
            prompt_file: self.prompt_file.clone(),
            instructions: self.instructions.clone(),
        })
    }
}

/// Discovers project-local `.merry/roles/*.toml` files under `workspace_root`.
///
/// The filename stem becomes the role name. Inline `[subagents.roles.<name>]`
/// entries in user `config.toml` take precedence when overlaid afterwards.
pub fn load_project_subagent_roles(
    workspace_root: &Path,
    config: Option<&MerryConfig>,
) -> Result<BTreeMap<String, EffectiveSubagentRoleConfig>, ConfigError> {
    let roles_dir = workspace_root.join(".merry").join("roles");
    if !roles_dir.is_dir() {
        return Ok(BTreeMap::new());
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(&roles_dir)
        .map_err(|source| ConfigError::Read {
            path: roles_dir.clone(),
            source,
        })?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .collect();
    entries.sort();

    let mut roles = BTreeMap::new();
    for path in entries {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        validate_role_identifier(stem)?;
        let bytes = fs::read(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        if bytes.len() > MAX_ROLE_TOML_BYTES {
            return Err(ConfigError::Invalid(format!(
                "project role file {} exceeds {MAX_ROLE_TOML_BYTES} bytes",
                path.display()
            )));
        }
        let text = String::from_utf8(bytes).map_err(|error| {
            ConfigError::Invalid(format!(
                "project role file {} is not valid UTF-8: {error}",
                path.display()
            ))
        })?;
        let parsed: SubagentRoleToml =
            toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source: Box::new(source),
            })?;
        let effective = parsed.to_effective(stem, config)?;
        roles.insert(stem.to_owned(), effective);
    }

    Ok(roles)
}

fn validate_role_identifier(name: &str) -> Result<(), ConfigError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ConfigError::Invalid(format!(
            "invalid subagent role name {name:?}: use ASCII letters, digits, '_' or '-'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::XdgPaths;

    fn home() -> PathBuf {
        PathBuf::from("/home/alice")
    }

    #[test]
    fn subagents_defaults_to_enabled_when_section_or_flag_is_omitted() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("empty config should parse")
            .expect("config present")
            .subagents_config()
            .expect("default subagents config should validate");
        assert!(missing.is_enabled());
        assert_eq!(
            missing.limits().min_model_turns(),
            MIN_CODING_SUBAGENT_MODEL_TURNS
        );
        assert_eq!(
            missing.limits().max_model_turns(),
            DEFAULT_CODING_SUBAGENT_MAX_MODEL_TURNS
        );

        // Tuning max_threads without specifying enabled MUST keep enabled = true.
        let tuned = MerryConfig::load_optional_from_text(
            Some("[subagents]\nmax_threads = 4\nlimit_behavior = \"fail\"\n"),
            &paths,
        )
        .expect("tuned subagents config parses")
        .expect("config present")
        .subagents_config()
        .expect("tuned config validates");
        assert!(tuned.is_enabled());
        assert_eq!(tuned.limits().max_threads(), 4);
        assert_eq!(tuned.limits().limit_behavior(), SubagentLimitBehavior::Fail);

        let disabled =
            MerryConfig::load_optional_from_text(Some("[subagents]\nenabled = false\n"), &paths)
                .expect("disabled config parses")
                .expect("config present")
                .subagents_config()
                .expect("disabled config validates");
        assert!(!disabled.is_enabled());
    }

    #[test]
    fn rejects_removed_runtime_subagents_and_models_browser_with_standard_parse_errors() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let old_subagents = MerryConfig::load_optional_from_text(
            Some("[runtime.subagents]\nenabled = true\n"),
            &paths,
        )
        .expect_err("removed [runtime.subagents] must fail strict parsing");
        assert!(matches!(old_subagents, ConfigError::Parse { .. }));

        let old_browser = MerryConfig::load_optional_from_text(
            Some("[models.browser]\nmodel = \"gpt-4.1-mini\"\n"),
            &paths,
        )
        .expect_err("removed [models.browser] must fail strict parsing");
        assert!(matches!(old_browser, ConfigError::Parse { .. }));
    }

    #[test]
    fn parses_subagents_default_roles_toggle_and_project_role_files() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-4.1"

[subagents]
max_threads = 8
foreground_budget_secs = 90

[subagents.toggle]
review = false

[subagents.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"
reasoning_effort = "medium"
max_model_turns = 100
timeout_secs = 420
max_total_tokens = 500000

[subagents.roles.explore]
model = "gpt-4.1-nano"
reasoning_effort = "low"
max_model_turns = 30
timeout_secs = 120

[subagents.roles.browser]
base = "review"
description = "UI verification specialist"
model = "gpt-4.1-mini"
allowed_tools = ["browser_tab", "browser_network", "web_fetch", "read_file"]
instructions = "Check console errors."
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
type = "openai-compatible"
base_url = "https://api.openai.com/v1"
api_key_file = "managed/secrets/openai.key"
"#,
            ),
            &paths,
        )
        .expect("config parses")
        .expect("config present");

        let subagents = config.subagents_config().expect("subagents validates");
        assert!(subagents.is_enabled());
        assert_eq!(subagents.limits().max_threads(), 8);
        assert_eq!(subagents.limits().foreground_wait_budget_secs(), 90);
        assert_eq!(subagents.limits().max_model_turns(), 100);
        assert_eq!(subagents.limits().default_timeout_secs(), Some(420));
        assert_eq!(subagents.limits().max_total_tokens(), Some(500_000));
        assert_eq!(subagents.toggle().get("review"), Some(&false));
        assert_eq!(
            subagents.default_config().model.as_deref(),
            Some("gpt-4.1-mini")
        );
        assert_eq!(
            subagents.roles()["explore"].model.as_deref(),
            Some("gpt-4.1-nano")
        );
        assert_eq!(subagents.roles()["browser"].base.as_deref(), Some("review"));

        let workspace = tempfile::tempdir().expect("tempdir");
        let roles_dir = workspace.path().join(".merry/roles");
        fs::create_dir_all(&roles_dir).expect("mkdir roles");
        fs::write(
            roles_dir.join("security.toml"),
            r#"
base = "review"
description = "Security auditor"
max_model_turns = 25
instructions = "Focus on authz and sandbox boundaries."
"#,
        )
        .expect("write security role");

        let discovered = load_project_subagent_roles(workspace.path(), Some(&config))
            .expect("project role discovery succeeds");
        assert!(discovered.contains_key("security"));
        assert_eq!(
            discovered["security"].description.as_deref(),
            Some("Security auditor")
        );
        assert_eq!(discovered["security"].max_model_turns, Some(25));
    }
}
