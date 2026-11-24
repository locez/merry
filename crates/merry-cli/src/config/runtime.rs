use super::{
    ConfigError, MerryConfig, RuntimeModelToml, default_true, provider::parse_reasoning_effort,
    validate_model_text,
};
use merry_runtime::{ArchiveConfig, CitationCompactionPolicy, CompactionConfig, HistoryRetention};
use serde::Deserialize;

impl MerryConfig {
    pub fn automatic_compaction_config(&self) -> Result<CompactionConfig, ConfigError> {
        let Some(auto_compaction) = self
            .raw
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.auto_compaction.as_ref())
        else {
            return Ok(CompactionConfig::default());
        };

        auto_compaction.to_config()
    }

    pub fn history_retention(&self) -> Result<HistoryRetention, ConfigError> {
        let Some(retention) = self
            .raw
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.history_retention.as_ref())
        else {
            return Ok(HistoryRetention::default());
        };

        retention.to_config()
    }

    pub fn max_parallel_tool_calls(&self) -> Result<Option<std::num::NonZeroUsize>, ConfigError> {
        let Some(limit) = self
            .raw
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.max_parallel_tool_calls)
        else {
            return Ok(None);
        };
        let limit = std::num::NonZeroUsize::new(limit).ok_or_else(|| {
            ConfigError::Invalid(
                "runtime.max_parallel_tool_calls must be greater than zero".to_owned(),
            )
        })?;
        Ok(Some(limit))
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
    max_parallel_tool_calls: Option<usize>,
    auto_compaction: Option<AutoCompactionToml>,
    history_retention: Option<HistoryRetentionToml>,
}

#[derive(Debug, Default, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum HistoryRetentionModeToml {
    #[default]
    Archive,
    ContextOnly,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HistoryRetentionToml {
    #[serde(default)]
    mode: HistoryRetentionModeToml,
    max_segments: Option<usize>,
    max_bytes: Option<u64>,
    export_markdown: Option<bool>,
}

impl HistoryRetentionToml {
    fn to_config(&self) -> Result<HistoryRetention, ConfigError> {
        match self.mode {
            HistoryRetentionModeToml::Archive => {
                let max_segments = self
                    .max_segments
                    .map(|value| {
                        std::num::NonZeroUsize::new(value).ok_or_else(|| {
                            ConfigError::Invalid(
                                "runtime.history_retention.max_segments must be greater than zero"
                                    .to_owned(),
                            )
                        })
                    })
                    .transpose()?;
                let max_bytes = self
                    .max_bytes
                    .map(|value| {
                        std::num::NonZeroU64::new(value).ok_or_else(|| {
                            ConfigError::Invalid(
                                "runtime.history_retention.max_bytes must be greater than zero"
                                    .to_owned(),
                            )
                        })
                    })
                    .transpose()?;
                let config = ArchiveConfig::new()
                    .with_max_segments(max_segments)
                    .with_max_bytes(max_bytes)
                    .with_export_markdown(self.export_markdown.unwrap_or(true));
                Ok(HistoryRetention::archive_with(config))
            }
            HistoryRetentionModeToml::ContextOnly => {
                if self.max_segments.is_some()
                    || self.max_bytes.is_some()
                    || self.export_markdown == Some(true)
                {
                    return Err(ConfigError::Invalid(
                        "runtime.history_retention max_segments, max_bytes, and export_markdown are only valid when mode = \"archive\""
                            .to_owned(),
                    ));
                }
                Ok(HistoryRetention::context_only())
            }
        }
    }
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AutoCompactionToml {
    #[serde(default = "default_true")]
    enabled: bool,
    target_output_tokens: Option<u64>,
    max_accepted_output_bytes: Option<usize>,
    retained_model_turns: Option<usize>,
    one_shot_window_percent: Option<u64>,
    one_shot_retained_tool_exchanges: Option<usize>,
    reasoning_effort: Option<String>,
    model_output_token_limit: Option<u64>,
    retained_raw_tail_items: Option<usize>,
    max_ref_excerpt_bytes: Option<usize>,
    max_carried_prior_refs: Option<usize>,
}

impl AutoCompactionToml {
    fn to_config(&self) -> Result<CompactionConfig, ConfigError> {
        self.validate_removed_fields()?;
        let reasoning_effort = parse_reasoning_effort(
            "runtime.auto_compaction.reasoning_effort",
            self.reasoning_effort.as_deref(),
        )?;
        let config = if self.enabled {
            let defaults = CompactionConfig::default().policy();
            let policy = CitationCompactionPolicy::new(
                self.target_output_tokens
                    .or_else(|| defaults.target_output_tokens()),
                self.max_accepted_output_bytes
                    .or_else(|| defaults.max_accepted_output_bytes()),
                self.retained_model_turns
                    .unwrap_or_else(|| defaults.retained_model_turns()),
            )
            .map_err(|error| ConfigError::Invalid(error.to_string()))?
            .with_one_shot_retained_tool_exchanges(
                self.one_shot_retained_tool_exchanges
                    .unwrap_or_else(|| defaults.one_shot_retained_tool_exchanges()),
            );
            CompactionConfig::enabled(policy)
        } else {
            CompactionConfig::disabled()
        };
        Ok(config.with_reasoning_effort(reasoning_effort))
    }

    fn validate_removed_fields(&self) -> Result<(), ConfigError> {
        if self.one_shot_window_percent.is_some() {
            return Err(ConfigError::Invalid("runtime.auto_compaction.one_shot_window_percent was removed; compaction now selects its strategy by request fit".to_owned()));
        }
        if self.retained_model_turns.is_some() && self.retained_raw_tail_items.is_some() {
            return Err(ConfigError::Invalid(
                "runtime.auto_compaction cannot set both retained_model_turns and removed field retained_raw_tail_items"
                    .to_owned(),
            ));
        }
        if self.model_output_token_limit.is_some() {
            return Err(removed_auto_compaction_field("model_output_token_limit"));
        }
        if self.retained_raw_tail_items.is_some() {
            return Err(ConfigError::Invalid(
                "runtime.auto_compaction.retained_raw_tail_items was removed; use retained_model_turns and choose the value explicitly"
                    .to_owned(),
            ));
        }
        if self.max_ref_excerpt_bytes.is_some() {
            return Err(removed_auto_compaction_field("max_ref_excerpt_bytes"));
        }
        if self.max_carried_prior_refs.is_some() {
            return Err(removed_auto_compaction_field("max_carried_prior_refs"));
        }
        Ok(())
    }
}

fn removed_auto_compaction_field(field: &str) -> ConfigError {
    ConfigError::Invalid(format!(
        "runtime.auto_compaction.{field} was removed; supported fields are enabled, retained_model_turns, target_output_tokens, max_accepted_output_bytes, and one_shot_retained_tool_exchanges"
    ))
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
max_accepted_output_bytes = 4096
retained_model_turns = 4
reasoning_effort = "medium"
one_shot_retained_tool_exchanges = 2
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
        assert_eq!(policy.target_output_tokens(), Some(160));
        assert_eq!(policy.max_accepted_output_bytes(), Some(4096));
        assert_eq!(policy.retained_model_turns(), 4);
        assert_eq!(policy.one_shot_retained_tool_exchanges(), 2);
        assert_eq!(
            auto_compaction
                .reasoning_effort()
                .map(merry_llm::ReasoningEffort::as_str),
            Some("medium")
        );
    }

    #[test]
    fn runtime_auto_compaction_rejects_invalid_reasoning_effort() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
reasoning_effort = " padded "
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let error = config
            .automatic_compaction_config()
            .expect_err("invalid reasoning effort must be rejected");
        assert!(
            error
                .to_string()
                .contains("runtime.auto_compaction.reasoning_effort is invalid"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn runtime_auto_compaction_config_defaults_and_disabled_mode() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("empty config should parse")
            .expect("config should be present")
            .automatic_compaction_config()
            .expect("default auto compaction config should validate");
        assert_eq!(missing, merry_runtime::CompactionConfig::default());

        let disabled = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
enabled = false
retained_model_turns = 4
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
            merry_runtime::CompactionConfig::default().policy()
        );
    }

    #[test]
    fn runtime_auto_compaction_rejects_removed_fields_even_when_disabled() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let cases = [
            (
                "model_output_token_limit = 256",
                "Merry config is invalid: runtime.auto_compaction.model_output_token_limit was removed; supported fields are enabled, retained_model_turns, target_output_tokens, max_accepted_output_bytes, and one_shot_retained_tool_exchanges",
            ),
            (
                "retained_raw_tail_items = 4",
                "Merry config is invalid: runtime.auto_compaction.retained_raw_tail_items was removed; use retained_model_turns and choose the value explicitly",
            ),
            (
                "max_ref_excerpt_bytes = 900",
                "Merry config is invalid: runtime.auto_compaction.max_ref_excerpt_bytes was removed; supported fields are enabled, retained_model_turns, target_output_tokens, max_accepted_output_bytes, and one_shot_retained_tool_exchanges",
            ),
            (
                "max_carried_prior_refs = 12",
                "Merry config is invalid: runtime.auto_compaction.max_carried_prior_refs was removed; supported fields are enabled, retained_model_turns, target_output_tokens, max_accepted_output_bytes, and one_shot_retained_tool_exchanges",
            ),
            (
                "one_shot_window_percent = 150",
                "Merry config is invalid: runtime.auto_compaction.one_shot_window_percent was removed; compaction now selects its strategy by request fit",
            ),
        ];

        for (field, expected) in cases {
            let text = format!("[runtime.auto_compaction]\nenabled = false\n{field}\n");
            let config = MerryConfig::load_optional_from_text(Some(&text), &paths)
                .expect("removed fields remain readable for precise validation")
                .expect("config should be present");
            let error = config
                .automatic_compaction_config()
                .expect_err("removed field must fail even when disabled");

            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn runtime_auto_compaction_rejects_old_and_new_retention_fields_together() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.auto_compaction]
retained_model_turns = 5
retained_raw_tail_items = 10
"#,
            ),
            &paths,
        )
        .expect("removed field remains readable for precise validation")
        .expect("config should be present");

        let error = config
            .automatic_compaction_config()
            .expect_err("old and new retention fields must conflict");

        assert_eq!(
            error.to_string(),
            "Merry config is invalid: runtime.auto_compaction cannot set both retained_model_turns and removed field retained_raw_tail_items"
        );
    }

    #[test]
    fn context_compaction_model_role_defaults_to_default_provider() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[models.context_compaction]
model = "gpt-compact"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
api_key_file = "managed/secrets/openai.key"
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
        let config = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[models.approval_review]
model = "gpt-review"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
api_key_file = "managed/secrets/openai.key"
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
    fn parses_runtime_history_retention_config_and_rejects_invalid_combinations() {
        let paths = XdgPaths::from_parts(home(), None, None);

        let default_retention = MerryConfig::load_optional_from_text(Some(""), &paths)
            .expect("empty config parses")
            .expect("config present")
            .history_retention()
            .expect("default history_retention validates");
        assert_eq!(default_retention, HistoryRetention::archive());

        let bounded = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.history_retention]
mode = "archive"
max_segments = 12
max_bytes = 10485760
export_markdown = false
"#,
            ),
            &paths,
        )
        .expect("bounded archive config parses")
        .expect("config present")
        .history_retention()
        .expect("bounded archive validates");
        assert_eq!(
            bounded,
            HistoryRetention::archive_with(
                ArchiveConfig::new()
                    .with_max_segments(std::num::NonZeroUsize::new(12))
                    .with_max_bytes(std::num::NonZeroU64::new(10_485_760))
                    .with_export_markdown(false),
            )
        );

        let context_only = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.history_retention]
mode = "context_only"
"#,
            ),
            &paths,
        )
        .expect("context_only config parses")
        .expect("config present")
        .history_retention()
        .expect("context_only validates");
        assert_eq!(context_only, HistoryRetention::context_only());

        let invalid = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime.history_retention]
mode = "context_only"
max_segments = 5
"#,
            ),
            &paths,
        )
        .expect("config syntax parses")
        .expect("config present")
        .history_retention()
        .expect_err("context_only with max_segments must fail");
        assert!(invalid.to_string().contains("mode = \"archive\""));
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

        let unknown_provider = MerryConfig::load_with_managed_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[models.context_compaction]
provider = "not-configured"
model = "gpt-compact"
"#,
            ),
            Some(
                r#"
version = 1

[providers.openai-compatible]
api_key_file = "managed/secrets/openai.key"
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

    #[test]
    fn parses_runtime_max_parallel_tool_calls_and_rejects_zero() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime]
max_parallel_tool_calls = 16
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        assert_eq!(
            config
                .max_parallel_tool_calls()
                .expect("positive limit should validate")
                .map(std::num::NonZeroUsize::get),
            Some(16)
        );

        let zero = MerryConfig::load_optional_from_text(
            Some(
                r#"
[runtime]
max_parallel_tool_calls = 0
"#,
            ),
            &paths,
        )
        .expect("zero parses before validation")
        .expect("config should be present");
        let err = zero
            .max_parallel_tool_calls()
            .expect_err("zero limit must be rejected");
        assert!(err.to_string().contains("max_parallel_tool_calls"));
    }
}
