use crate::cli_error::CliError;
use crate::coding::{CodingSubagentsConfig, RuntimeRoleProviderConfig};
use crate::config::{
    ConfiguredProviderProfile, EffectiveProviderConfig, EffectiveSubagentRoleConfig, MerryConfig,
    SubagentsConfig, load_project_subagent_roles,
};
use merry::coding::RuntimeModelRole;
use merry::profiles::{
    SubagentDefaultOverride, SubagentModelOverride, SubagentRoleCatalog, SubagentRoleOverlay,
    load_workspace_prompt_file,
};
use merry::providers::{
    AnthropicProvider, AnthropicProviderConfig, ModelCatalogProvider, OpenAiProvider,
    OpenAiProviderConfig,
};
use merry::providers::{ModelName, ModelProvider, ModelRetryPolicy};
use std::{collections::BTreeMap, num::NonZeroU64, path::Path, sync::Arc};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimeRoleOverrides<'a> {
    pub(crate) context_compaction: Option<(&'a str, &'a str)>,
    pub(crate) approval_review: Option<(&'a str, &'a str)>,
}

pub(crate) fn runtime_provider_bundle_from_config(
    merry_config: Option<&MerryConfig>,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeProviderBundle, CliError> {
    runtime_provider_bundle_from_config_with_overrides(
        merry_config,
        None,
        None,
        RuntimeRoleOverrides::default(),
        map_usage_error,
    )
}

pub(crate) fn runtime_provider_bundle_from_config_with_overrides(
    merry_config: Option<&MerryConfig>,
    provider_override: Option<&str>,
    model_override: Option<&str>,
    role_overrides: RuntimeRoleOverrides<'_>,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeProviderBundle, CliError> {
    runtime_provider_bundle_from_config_inner(
        merry_config,
        provider_override,
        model_override,
        role_overrides,
        map_usage_error,
    )
}

fn runtime_provider_bundle_from_config_inner(
    merry_config: Option<&MerryConfig>,
    provider_override: Option<&str>,
    model_override: Option<&str>,
    role_overrides: RuntimeRoleOverrides<'_>,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeProviderBundle, CliError> {
    let merry_config = merry_config.ok_or_else(|| {
        map_usage_error("Merry XDG provider config is required for runtime".to_owned())
    })?;
    let primary = runtime_primary_provider_from_config_with_override(
        Some(merry_config),
        provider_override,
        model_override,
        map_usage_error,
    )?;
    let runtime_models = merry_config
        .runtime_models()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let context_compaction_binding = role_overrides
        .context_compaction
        .map(|(provider, model)| (provider.to_owned(), model.to_owned()))
        .or_else(|| {
            runtime_models
                .context_compaction
                .map(|model| (model.provider, model.model))
        });
    let context_compaction = context_compaction_binding
        .map(|(provider_alias, model)| {
            let provider = merry_config
                .provider_by_alias(&provider_alias)
                .map_err(|error| map_usage_error(error.to_string()))?;
            runtime_role_provider(
                RuntimeModelRole::ContextCompaction,
                provider,
                model,
                map_usage_error,
            )
        })
        .transpose()?;
    let approval_review_binding = role_overrides
        .approval_review
        .map(|(provider, model)| (provider.to_owned(), model.to_owned()))
        .or_else(|| {
            runtime_models
                .approval_review
                .map(|model| (model.provider, model.model))
        });
    let approval_review = approval_review_binding
        .map(|(provider_alias, model)| {
            let provider = merry_config
                .provider_by_alias(&provider_alias)
                .map_err(|error| map_usage_error(error.to_string()))?;
            runtime_role_provider(
                RuntimeModelRole::ApprovalReview,
                provider,
                model,
                map_usage_error,
            )
        })
        .transpose()?;
    let retry_policy = merry_config
        .provider_retry_policy()
        .map_err(|error| map_usage_error(error.to_string()))?;
    Ok(RuntimeProviderBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    })
}

pub(crate) fn runtime_primary_provider_from_config_with_override(
    merry_config: Option<&MerryConfig>,
    provider_override: Option<&str>,
    model_override: Option<&str>,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimePrimaryProviderConfig, CliError> {
    let merry_config = merry_config.ok_or_else(|| {
        map_usage_error("Merry XDG provider config is required for runtime".to_owned())
    })?;
    let (provider, model) = match provider_override {
        Some(alias) => {
            let provider = merry_config
                .provider_by_alias(alias)
                .map_err(|error| map_usage_error(error.to_string()))?;
            let model = model_override
                .map(str::to_owned)
                .or_else(|| {
                    merry_config
                        .provider_profile(alias)
                        .ok()
                        .and_then(|profile| {
                            profile
                                .default_model()
                                .map(|model| model.as_str().to_owned())
                        })
                })
                .ok_or_else(|| {
                    map_usage_error(format!(
                        "provider {alias:?} has no selected or default model"
                    ))
                })?;
            (provider, model)
        }
        None => {
            let default = merry_config
                .default_provider()
                .map_err(|error| map_usage_error(error.to_string()))?;
            (
                default.provider,
                model_override.map(str::to_owned).unwrap_or(default.model),
            )
        }
    };
    runtime_primary_provider(provider, model, map_usage_error)
}

fn runtime_primary_provider(
    provider: EffectiveProviderConfig,
    model: String,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimePrimaryProviderConfig, CliError> {
    Ok(RuntimePrimaryProviderConfig {
        provider: model_provider(provider, map_usage_error)?,
        model: ModelName::new(&model).map_err(|error| map_usage_error(error.to_string()))?,
    })
}

fn runtime_role_provider(
    role: RuntimeModelRole,
    provider: EffectiveProviderConfig,
    model: String,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeRoleProviderConfig, CliError> {
    RuntimeRoleProviderConfig::new(
        role,
        model_provider(provider, map_usage_error)?,
        ModelName::new(&model).map_err(|error| map_usage_error(error.to_string()))?,
    )
    .map_err(|error| map_usage_error(error.to_string()))
}

fn model_provider(
    provider: EffectiveProviderConfig,
    map_usage_error: fn(String) -> CliError,
) -> Result<Arc<dyn ModelProvider>, CliError> {
    provider_handles(provider, map_usage_error).map(|handles| handles.inference)
}

pub(crate) fn materialized_provider_from_config(
    merry_config: &MerryConfig,
    alias: &str,
    map_usage_error: fn(String) -> CliError,
) -> Result<MaterializedProvider, CliError> {
    let profile = merry_config
        .provider_profile(alias)
        .map_err(|error| map_usage_error(error.to_string()))?;
    let provider = merry_config
        .provider_by_alias(alias)
        .map_err(|error| map_usage_error(error.to_string()))?;
    let handles = provider_handles(provider, map_usage_error)?;
    Ok(MaterializedProvider {
        profile,
        inference: handles.inference,
        model_catalog: handles.model_catalog,
    })
}

fn provider_handles(
    provider: EffectiveProviderConfig,
    map_usage_error: fn(String) -> CliError,
) -> Result<ProviderHandles, CliError> {
    match provider {
        EffectiveProviderConfig::OpenAiCompatible(config) => {
            let api_key = config
                .resolve_api_key()
                .map_err(|error| map_usage_error(error.to_string()))?;
            let mut provider = OpenAiProviderConfig::new(&api_key)
                .map_err(|error| map_usage_error(error.to_string()))?
                .with_protocol(config.protocol)
                .with_provider_name(&config.alias)
                .map_err(|error| map_usage_error(error.to_string()))?;
            if let Some(base_url) = config.base_url.as_deref() {
                provider = provider
                    .with_base_url(base_url)
                    .map_err(|error| map_usage_error(error.to_string()))?;
            }
            let provider = Arc::new(OpenAiProvider::new(provider));
            let inference: Arc<dyn ModelProvider> = provider.clone();
            let model_catalog: Arc<dyn ModelCatalogProvider> = provider;
            Ok(ProviderHandles {
                inference,
                model_catalog,
            })
        }
        EffectiveProviderConfig::Anthropic(config) => {
            let api_key = config
                .resolve_api_key()
                .map_err(|error| map_usage_error(error.to_string()))?;
            let mut provider = AnthropicProviderConfig::new(&api_key)
                .map_err(|error| map_usage_error(error.to_string()))?
                .with_provider_name(&config.alias)
                .map_err(|error| map_usage_error(error.to_string()))?;
            if let Some(base_url) = config.base_url.as_deref() {
                provider = provider
                    .with_base_url(base_url)
                    .map_err(|error| map_usage_error(error.to_string()))?;
            }
            if let Some(version) = config.api_version.as_deref() {
                provider = provider
                    .with_api_version(version)
                    .map_err(|error| map_usage_error(error.to_string()))?;
            }
            if let Some(limit) = config.default_max_output_tokens {
                let limit = NonZeroU64::new(limit).ok_or_else(|| {
                    map_usage_error(format!(
                        "providers.{}.default_max_output_tokens must be greater than zero",
                        config.alias
                    ))
                })?;
                provider = provider.with_default_max_output_tokens(limit);
            }
            let provider = Arc::new(AnthropicProvider::new(provider));
            let inference: Arc<dyn ModelProvider> = provider.clone();
            let model_catalog: Arc<dyn ModelCatalogProvider> = provider;
            Ok(ProviderHandles {
                inference,
                model_catalog,
            })
        }
    }
}

/// Builds a [`CodingSubagentsConfig`] by combining built-in roles, project
/// `.merry/roles/*.toml` files, user `[subagents.default]` / `[subagents.roles.*]`
/// config overrides, and `[subagents.toggle]` flags.
pub(crate) fn build_coding_subagents_config(
    merry_config: Option<&MerryConfig>,
    subagents: &SubagentsConfig,
    workspace_root: &Path,
    runtime_controlled: bool,
    map_usage_error: fn(String) -> CliError,
) -> Result<CodingSubagentsConfig, CliError> {
    let mut provider_cache: BTreeMap<String, Arc<dyn ModelProvider>> = BTreeMap::new();
    let retry_policy = merry_config
        .map(MerryConfig::provider_retry_policy)
        .transpose()
        .map_err(|error| map_usage_error(error.to_string()))?
        .flatten();

    let default_cfg = subagents.default_config();
    let default_model_override = resolve_subagent_model_override(
        merry_config,
        default_cfg.provider.as_deref(),
        default_cfg.model.as_deref(),
        retry_policy,
        &mut provider_cache,
        map_usage_error,
    )?;
    let default_override = SubagentDefaultOverride {
        model_override: default_model_override,
        reasoning_effort: default_cfg.reasoning_effort.clone(),
        max_model_turns: default_cfg.max_model_turns,
        timeout_secs: default_cfg.timeout_secs,
        max_total_tokens: default_cfg.max_total_tokens,
    };

    let mut catalog = SubagentRoleCatalog::builtin().with_default_override(default_override);
    let mut limits = subagents.limits();
    let mut highest_turns = limits.max_model_turns();

    // Layer 1: project-local `.merry/roles/*.toml`
    let project_roles = load_project_subagent_roles(workspace_root, merry_config)
        .map_err(|error| map_usage_error(error.to_string()))?;
    for (name, role_cfg) in project_roles {
        if let Some(turns) = role_cfg.max_model_turns {
            highest_turns = highest_turns.max(turns);
        }
        let overlay = build_role_overlay(
            &name,
            &role_cfg,
            merry_config,
            workspace_root,
            retry_policy,
            &mut provider_cache,
            map_usage_error,
        )?;
        catalog
            .apply_role_overlay(&name, overlay)
            .map_err(|error| map_usage_error(error.to_string()))?;
    }

    // Layer 2: user `[subagents.roles.<name>]` in config.toml (takes precedence)
    for (name, role_cfg) in subagents.roles() {
        if let Some(turns) = role_cfg.max_model_turns {
            highest_turns = highest_turns.max(turns);
        }
        let overlay = build_role_overlay(
            name,
            role_cfg,
            merry_config,
            workspace_root,
            retry_policy,
            &mut provider_cache,
            map_usage_error,
        )?;
        catalog
            .apply_role_overlay(name, overlay)
            .map_err(|error| map_usage_error(error.to_string()))?;
    }

    // Layer 3: `[subagents.toggle]`
    catalog
        .apply_toggles(subagents.toggle())
        .map_err(|error| map_usage_error(error.to_string()))?;

    if highest_turns > limits.max_model_turns() {
        limits = limits
            .with_model_turn_bounds(limits.min_model_turns(), highest_turns)
            .map_err(|error| map_usage_error(error.to_string()))?;
    }

    let coding_subagents = if runtime_controlled {
        CodingSubagentsConfig::runtime_controlled(subagents.is_enabled(), limits)
    } else if subagents.is_enabled() {
        CodingSubagentsConfig::enabled(limits)
    } else {
        CodingSubagentsConfig::default()
    };
    Ok(coding_subagents.with_role_catalog(catalog))
}

fn build_role_overlay(
    name: &str,
    role_cfg: &EffectiveSubagentRoleConfig,
    merry_config: Option<&MerryConfig>,
    workspace_root: &Path,
    retry_policy: Option<ModelRetryPolicy>,
    provider_cache: &mut BTreeMap<String, Arc<dyn ModelProvider>>,
    map_usage_error: fn(String) -> CliError,
) -> Result<SubagentRoleOverlay, CliError> {
    let model_override = resolve_subagent_model_override(
        merry_config,
        role_cfg.provider.as_deref(),
        role_cfg.model.as_deref(),
        retry_policy,
        provider_cache,
        map_usage_error,
    )?;
    let prompt_from_file = role_cfg
        .prompt_file
        .as_deref()
        .map(|path| {
            load_workspace_prompt_file(workspace_root, name, path)
                .map_err(|error| map_usage_error(error.to_string()))
        })
        .transpose()?;
    let instructions = match (prompt_from_file, role_cfg.instructions.as_deref()) {
        (Some(file_text), Some(inline)) if !inline.trim().is_empty() => {
            Some(format!("{}\n\n{}", file_text.trim(), inline.trim()))
        }
        (Some(file_text), _) => Some(file_text),
        (None, Some(inline)) => Some(inline.to_owned()),
        (None, None) => None,
    };

    Ok(SubagentRoleOverlay {
        base: role_cfg.base.clone(),
        enabled: role_cfg.enabled,
        description: role_cfg.description.clone(),
        workspace_access: role_cfg.workspace_access,
        model_override,
        reasoning_effort: role_cfg.reasoning_effort.clone(),
        max_model_turns: role_cfg.max_model_turns,
        timeout_secs: role_cfg.timeout_secs,
        max_total_tokens: role_cfg.max_total_tokens,
        allowed_tools: role_cfg.allowed_tools.clone(),
        instructions,
    })
}

fn resolve_subagent_model_override(
    merry_config: Option<&MerryConfig>,
    provider_alias: Option<&str>,
    model: Option<&str>,
    retry_policy: Option<ModelRetryPolicy>,
    provider_cache: &mut BTreeMap<String, Arc<dyn ModelProvider>>,
    map_usage_error: fn(String) -> CliError,
) -> Result<Option<SubagentModelOverride>, CliError> {
    let Some(model) = model else {
        return Ok(None);
    };
    let merry_config = merry_config.ok_or_else(|| {
        map_usage_error(
            "Merry XDG provider config is required for subagent model overrides".to_owned(),
        )
    })?;
    let (cache_key, effective_provider) = match provider_alias {
        Some(alias) => {
            let provider = merry_config
                .provider_by_alias(alias)
                .map_err(|error| map_usage_error(error.to_string()))?;
            (alias.to_owned(), provider)
        }
        None => {
            let default = merry_config
                .default_provider()
                .map_err(|error| map_usage_error(error.to_string()))?;
            let alias = match &default.provider {
                EffectiveProviderConfig::OpenAiCompatible(cfg) => cfg.alias.clone(),
                EffectiveProviderConfig::Anthropic(cfg) => cfg.alias.clone(),
            };
            (alias, default.provider)
        }
    };
    let provider = if let Some(cached) = provider_cache.get(&cache_key) {
        Arc::clone(cached)
    } else {
        let materialized = model_provider(effective_provider, map_usage_error)?;
        provider_cache.insert(cache_key, Arc::clone(&materialized));
        materialized
    };
    Ok(Some(SubagentModelOverride {
        provider,
        model: ModelName::new(model).map_err(|error| map_usage_error(error.to_string()))?,
        retry_policy,
    }))
}

pub(crate) struct RuntimePrimaryProviderConfig {
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
}

pub(crate) struct RuntimeProviderBundle {
    pub(crate) primary: RuntimePrimaryProviderConfig,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) approval_review: Option<RuntimeRoleProviderConfig>,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
}

pub(crate) struct MaterializedProvider {
    pub(crate) profile: ConfiguredProviderProfile,
    pub(crate) inference: Arc<dyn ModelProvider>,
    pub(crate) model_catalog: Arc<dyn ModelCatalogProvider>,
}

struct ProviderHandles {
    inference: Arc<dyn ModelProvider>,
    model_catalog: Arc<dyn ModelCatalogProvider>,
}

#[cfg(test)]
mod tests {
    use crate::config::{MerryConfig, XdgPaths};
    use merry::coding::RuntimeModelRole;
    use merry::providers::ModelCatalogProvider;
    use std::path::PathBuf;

    fn load_managed_test_config(
        user_text: Option<&str>,
        managed_text: &str,
        secrets: &[(&str, &str)],
    ) -> (tempfile::TempDir, MerryConfig) {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let paths = XdgPaths::from_parts(
            PathBuf::from("/home/alice"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        )
        .with_data_home(temp.path().join("data"));
        std::fs::create_dir_all(paths.managed_secrets_dir()).expect("secrets dir should exist");
        for (name, value) in secrets {
            std::fs::write(paths.managed_secrets_dir().join(name), value)
                .expect("secret file should write");
        }
        let config =
            MerryConfig::load_with_managed_from_text(user_text, Some(managed_text), &paths)
                .expect("config should parse")
                .expect("config should exist");
        (temp, config)
    }

    #[test]
    fn materialized_provider_exposes_profile_and_catalog_boundaries() {
        let (_temp, config) = load_managed_test_config(
            Some(
                r#"
[providers.default]
provider = "opencode"
model = "deepseek-v4-pro"
"#,
            ),
            r#"
version = 1

[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
type = "openai-compatible"
protocol = "chat_completions"
base_url = "https://opencode.example.test/v1"
api_key_file = "secrets/opencode.key"
"#,
            &[("opencode.key", "sk-test\n")],
        );

        let resolved = super::materialized_provider_from_config(
            &config,
            "opencode",
            crate::cli_error::usage_error,
        )
        .expect("provider should materialize");
        let _catalog: std::sync::Arc<dyn ModelCatalogProvider> = resolved.model_catalog.clone();

        assert_eq!(resolved.profile.display_name(), "OpenCode");
        assert_eq!(resolved.profile.alias().as_str(), "opencode");
        assert_eq!(resolved.inference.name().as_str(), "opencode");
    }

    #[test]
    fn managed_only_provider_selection_builds_without_global_default() {
        let (_temp, config) = load_managed_test_config(
            None,
            r#"
version = 1

[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
type = "openai-compatible"
protocol = "chat_completions"
api_key_file = "secrets/opencode.key"
"#,
            &[("opencode.key", "sk-test\n")],
        );

        let primary = super::runtime_primary_provider_from_config_with_override(
            Some(&config),
            Some("opencode"),
            None,
            crate::cli_error::usage_error,
        )
        .expect("profile default model should resolve");

        assert_eq!(primary.provider.name().as_str(), "opencode");
        assert_eq!(primary.model.as_str(), "deepseek-v4-pro");
    }

    #[test]
    fn runtime_bundle_builds_anthropic_primary_and_openai_role_override() {
        let (_temp, config) = load_managed_test_config(
            Some(
                r#"
[providers.default]
provider = "claude"
model = "claude-primary"

[models.context_compaction]
provider = "compat"
model = "gpt-compact"
"#,
            ),
            r#"
version = 1

[providers.claude]
type = "anthropic"
api_key_file = "secrets/claude.key"

[providers.compat]
type = "openai-compatible"
protocol = "chat_completions"
api_key_file = "secrets/compat.key"
"#,
            &[
                ("claude.key", "sk-ant-test\n"),
                ("compat.key", "sk-openai-test\n"),
            ],
        );

        let bundle = super::runtime_provider_bundle_from_config(
            Some(&config),
            crate::cli_error::usage_error,
        )
        .expect("runtime provider bundle should build");
        assert_eq!(bundle.primary.provider.name().as_str(), "claude");
        assert_eq!(bundle.primary.model.as_str(), "claude-primary");
        let compaction = bundle
            .context_compaction
            .expect("compaction override should be present");
        assert_eq!(compaction.provider().name().as_str(), "compat");
        assert_eq!(compaction.model().as_str(), "gpt-compact");
    }

    #[test]
    fn runtime_bundle_overrides_only_the_primary_provider_and_model() {
        let (_temp, config) = load_managed_test_config(
            Some(
                r#"
[providers.default]
provider = "compat"
model = "gpt-default"

[models.context_compaction]
provider = "compat"
model = "gpt-compact"
"#,
            ),
            r#"
version = 1

[providers.compat]
type = "openai-compatible"
api_key_file = "secrets/compat.key"

[providers.claude]
type = "anthropic"
api_key_file = "secrets/claude.key"
"#,
            &[
                ("compat.key", "sk-openai-test\n"),
                ("claude.key", "sk-ant-test\n"),
            ],
        );

        let bundle = super::runtime_provider_bundle_from_config_with_overrides(
            Some(&config),
            Some("claude"),
            Some("claude-next"),
            super::RuntimeRoleOverrides::default(),
            crate::cli_error::usage_error,
        )
        .expect("runtime provider bundle should build");

        assert_eq!(bundle.primary.provider.name().as_str(), "claude");
        assert_eq!(bundle.primary.model.as_str(), "claude-next");
        let compaction = bundle
            .context_compaction
            .expect("compaction override should remain configured");
        assert_eq!(compaction.provider().name().as_str(), "compat");
        assert_eq!(compaction.model().as_str(), "gpt-compact");
    }

    #[test]
    fn openai_provider_bundle_materializes_models_and_roles() {
        let (_temp, config) = load_managed_test_config(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.retry]
enabled = true
max_attempts = 3

[models.context_compaction]
model = "gpt-compact"

[models.approval_review]
model = "gpt-review"
"#,
            ),
            r#"
version = 1

[providers.openai-compatible]
protocol = "chat_completions"
api_key_file = "secrets/openai.key"
"#,
            &[("openai.key", "sk-managed-secret\n")],
        );
        let bundle = super::runtime_provider_bundle_from_config(Some(&config), |message| {
            crate::cli_error::CliError::Unexpected(message)
        })
        .expect("runtime provider bundle should build");

        assert_eq!(bundle.primary.model.as_str(), "gpt-primary");
        let context_compaction = bundle
            .context_compaction
            .expect("context compaction provider should be materialized");
        assert_eq!(
            context_compaction.role(),
            RuntimeModelRole::ContextCompaction
        );
        assert_eq!(context_compaction.model().as_str(), "gpt-compact");
        let approval_review = bundle
            .approval_review
            .expect("approval review provider should be materialized");
        assert_eq!(approval_review.role(), RuntimeModelRole::ApprovalReview);
        assert_eq!(approval_review.model().as_str(), "gpt-review");
        assert_eq!(
            bundle
                .retry_policy
                .expect("retry policy should be materialized")
                .max_attempts(),
            3
        );
    }
}
