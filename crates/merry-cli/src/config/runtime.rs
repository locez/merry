use super::{ConfigError, MerryConfig, RuntimeModelToml, default_true, validate_model_text};
use merry_runtime::{AutomaticCompactionConfig, CitationCompactionPolicy};
use serde::Deserialize;

impl MerryConfig {
    pub fn automatic_compaction_config(&self) -> Result<AutomaticCompactionConfig, ConfigError> {
        let Some(auto_compaction) = self
            .raw
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.auto_compaction.as_ref())
        else {
            return Ok(AutomaticCompactionConfig::default());
        };

        auto_compaction.to_config()
    }

    pub fn subagents_config(&self) -> Result<SubagentsConfig, ConfigError> {
        let Some(subagents) = self
            .raw
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.subagents.as_ref())
        else {
            return Ok(SubagentsConfig::default());
        };

        subagents.to_config()
    }

    pub fn runtime_models(&self) -> Result<EffectiveRuntimeModelsConfig, ConfigError> {
        let Some(models) = self.raw.models.as_ref() else {
            return Ok(EffectiveRuntimeModelsConfig::default());
        };

        let context_compaction = models
            .context_compaction
            .as_ref()
            .map(|model| self.effective_runtime_model("context_compaction", model))
            .transpose()?;
        let approval_review = models
            .approval_review
            .as_ref()
            .map(|model| self.effective_runtime_model("approval_review", model))
            .transpose()?;

        Ok(EffectiveRuntimeModelsConfig {
            context_compaction,
            approval_review,
        })
    }

    fn effective_runtime_model(
        &self,
        role: &str,
        model: &RuntimeModelToml,
    ) -> Result<EffectiveRuntimeModelConfig, ConfigError> {
        validate_model_text(&format!("models.{role}.model"), &model.model)?;
        let provider = match model.provider.as_deref() {
            Some(provider) => provider,
            None => self
                .raw
                .providers
                .as_ref()
                .and_then(|providers| providers.default.as_ref())
                .map(|default| default.provider.as_str())
                .ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "[providers.default] is required when [models.{role}].provider is omitted"
                    ))
                })?,
        };
        self.validate_runtime_model_provider(role, provider)?;
        Ok(EffectiveRuntimeModelConfig {
            provider: provider.to_owned(),
            model: model.model.clone(),
        })
    }

    fn validate_runtime_model_provider(
        &self,
        role: &str,
        provider_alias: &str,
    ) -> Result<(), ConfigError> {
        self.validate_provider_alias(provider_alias)
            .map_err(|error| {
                ConfigError::Invalid(format!(
                    "invalid provider {provider_alias:?} for [models.{role}]: {error}"
                ))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRuntimeModelConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectiveRuntimeModelsConfig {
    pub context_compaction: Option<EffectiveRuntimeModelConfig>,
    pub approval_review: Option<EffectiveRuntimeModelConfig>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeToml {
    auto_compaction: Option<AutoCompactionToml>,
    subagents: Option<SubagentsToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AutoCompactionToml {
    #[serde(default = "default_true")]
    enabled: bool,
    target_output_tokens: Option<u64>,
    model_output_token_limit: Option<u64>,
    max_accepted_output_bytes: Option<usize>,
    retained_raw_tail_items: Option<usize>,
    max_ref_excerpt_bytes: Option<usize>,
    max_carried_prior_refs: Option<usize>,
}

impl AutoCompactionToml {
    fn to_config(&self) -> Result<AutomaticCompactionConfig, ConfigError> {
        if !self.enabled {
            return Ok(AutomaticCompactionConfig::disabled());
        }

        let defaults = AutomaticCompactionConfig::default().policy();
        let policy = CitationCompactionPolicy::new(
            self.target_output_tokens
                .unwrap_or_else(|| defaults.target_output_tokens()),
            self.model_output_token_limit
                .or_else(|| defaults.model_output_token_limit()),
            self.max_accepted_output_bytes
                .unwrap_or_else(|| defaults.max_accepted_output_bytes()),
            self.retained_raw_tail_items
                .unwrap_or_else(|| defaults.retained_raw_tail_items()),
            self.max_ref_excerpt_bytes
                .unwrap_or_else(|| defaults.max_ref_excerpt_bytes()),
            self.max_carried_prior_refs
                .unwrap_or_else(|| defaults.max_carried_prior_refs()),
        )
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        Ok(AutomaticCompactionConfig::enabled(policy))
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SubagentsToml {
    #[serde(default)]
    enabled: bool,
    max_threads: Option<usize>,
    max_depth: Option<u8>,
}

impl SubagentsToml {
    fn to_config(&self) -> Result<SubagentsConfig, ConfigError> {
        let defaults = SubagentsConfig::default().limits;
        let limits = merry_runtime::SubagentConfig::new(
            self.max_threads.unwrap_or(defaults.max_threads()),
            self.max_depth.unwrap_or(defaults.max_depth()),
        )
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        Ok(SubagentsConfig {
            enabled: self.enabled,
            limits,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubagentsConfig {
    enabled: bool,
    limits: merry_runtime::SubagentConfig,
}

impl SubagentsConfig {
    #[must_use]
    pub fn is_enabled(self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn limits(self) -> merry_runtime::SubagentConfig {
        self.limits
    }

    pub fn with_overrides(
        self,
        enabled: Option<bool>,
        max_threads: Option<usize>,
    ) -> Result<Self, ConfigError> {
        let limits = merry_runtime::SubagentConfig::new(
            max_threads.unwrap_or_else(|| self.limits.max_threads()),
            self.limits.max_depth(),
        )
        .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        Ok(Self {
            enabled: enabled.unwrap_or(self.enabled),
            limits,
        })
    }
}

impl From<SubagentsConfig> for crate::coding_runtime::CodingSubagentsConfig {
    fn from(config: SubagentsConfig) -> Self {
        if config.is_enabled() {
            crate::coding_runtime::CodingSubagentsConfig::enabled(config.limits())
        } else {
            crate::coding_runtime::CodingSubagentsConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::XdgPaths;
    use std::path::PathBuf;

    fn home() -> PathBuf {
        PathBuf::from("/home/alice")
    }

    #[test]
    fn parses_runtime_auto_compaction_config() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
enabled = true
target_output_tokens = 160
model_output_token_limit = 256
max_accepted_output_bytes = 4096
retained_raw_tail_items = 4
max_ref_excerpt_bytes = 900
max_carried_prior_refs = 12
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let auto_compaction = config
            .automatic_compaction_config()
            .expect("auto compaction config should validate");
        assert!(auto_compaction.is_enabled());
        let policy = auto_compaction.policy();
        assert_eq!(policy.target_output_tokens(), 160);
        assert_eq!(policy.model_output_token_limit(), Some(256));
        assert_eq!(policy.max_accepted_output_bytes(), 4096);
        assert_eq!(policy.retained_raw_tail_items(), 4);
        assert_eq!(policy.max_ref_excerpt_bytes(), 900);
        assert_eq!(policy.max_carried_prior_refs(), 12);
    }

    #[test]
    fn runtime_auto_compaction_config_defaults_and_disabled_mode() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("empty config should parse")
            .expect("config should be present")
            .automatic_compaction_config()
            .expect("default auto compaction config should validate");
        assert_eq!(missing, merry_runtime::AutomaticCompactionConfig::default());

        let disabled = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
enabled = false
retained_raw_tail_items = 4
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present")
        .automatic_compaction_config()
        .expect("disabled auto compaction config should validate");
        assert!(!disabled.is_enabled());
        assert_eq!(
            disabled.policy(),
            merry_runtime::AutomaticCompactionConfig::default().policy()
        );
    }

    #[test]
    fn runtime_subagents_config_defaults_disabled_and_parses_limits() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("empty config should parse")
            .expect("config should be present")
            .subagents_config()
            .expect("default subagents config should validate");
        assert!(!missing.is_enabled());
        assert_eq!(missing.limits(), merry_runtime::SubagentConfig::default());

        let enabled = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.subagents]
enabled = true
max_threads = 3
max_depth = 1
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present")
        .subagents_config()
        .expect("subagents config should validate");
        assert!(enabled.is_enabled());
        assert_eq!(
            enabled.limits(),
            merry_runtime::SubagentConfig::new(3, 1).expect("valid subagent config")
        );

        let invalid = MerryConfig::load_optional_from_text(
            Some("[runtime.subagents]\nenabled = true\nmax_threads = 0\n"),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present")
        .subagents_config()
        .expect_err("zero max_threads should be invalid");
        assert!(invalid.to_string().contains("max_threads"));
    }

    #[test]
    fn runtime_subagents_preferences_override_enabled_and_threads_only() {
        let base = SubagentsConfig {
            enabled: false,
            limits: merry_runtime::SubagentConfig::new(3, 2).unwrap(),
        };

        let overridden = base
            .with_overrides(Some(true), Some(6))
            .expect("preference overrides should validate");

        assert!(overridden.is_enabled());
        assert_eq!(overridden.limits().max_threads(), 6);
        assert_eq!(overridden.limits().max_depth(), 2);
    }

    #[test]
    fn context_compaction_model_role_defaults_to_default_provider() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.openai-compatible]
api_key = "sk-inline-secret"

[models.context_compaction]
model = "gpt-compact"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let models = config
            .runtime_models()
            .expect("runtime model role config should validate");
        let context_compaction = models
            .context_compaction
            .expect("context compaction model role should be configured");
        assert_eq!(context_compaction.provider, "openai-compatible");
        assert_eq!(context_compaction.model, "gpt-compact");
    }

    #[test]
    fn approval_review_model_role_defaults_to_default_provider() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.openai-compatible]
api_key = "sk-inline-secret"

[models.approval_review]
model = "gpt-review"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let models = config
            .runtime_models()
            .expect("runtime model role config should validate");
        let approval_review = models
            .approval_review
            .expect("approval review model role should be configured");
        assert_eq!(approval_review.provider, "openai-compatible");
        assert_eq!(approval_review.model, "gpt-review");
    }

    #[test]
    fn runtime_model_roles_default_to_no_overrides() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let models = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("empty config should parse")
            .expect("config should be present")
            .runtime_models()
            .expect("empty runtime model role config should validate");

        assert!(models.context_compaction.is_none());
        assert!(models.approval_review.is_none());
    }

    #[test]
    fn rejects_unknown_runtime_model_role_or_provider_alias() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let unknown_role = MerryConfig::load_optional_from_text(
            Some(
                r#"
[models.tool_planner]
provider = "openai-compatible"
model = "gpt-other"
"#,
            ),
            &paths,
        )
        .expect_err("unknown runtime model role should fail parsing");
        assert!(unknown_role.to_string().contains("tool_planner"));

        let unknown_provider = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.openai-compatible]
api_key = "sk-inline-secret"

[models.context_compaction]
provider = "not-configured"
model = "gpt-compact"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present")
        .runtime_models()
        .expect_err("unknown model role provider should fail validation");
        assert!(unknown_provider.to_string().contains("not-configured"));
    }
}
