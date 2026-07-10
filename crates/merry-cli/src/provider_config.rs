use crate::cli_error::{CliError, debug_openai_usage_error};
use crate::coding_runtime::RuntimeRoleProviderConfig;
use crate::config::{
    ConfiguredProviderProfile, EffectiveOpenAiProviderConfig, EffectiveProviderConfig, MerryConfig,
};
use merry_llm::{ModelCatalogProvider, ModelName, ModelProvider, ModelRetryPolicy};
use merry_provider_anthropic::{AnthropicProvider, AnthropicProviderConfig};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{RuntimeBuilder, RuntimeModelRole};
use std::{env, num::NonZeroU64, sync::Arc};

pub(crate) const MERRY_OPENAI_DEBUG_ENV: &str = "MERRY_OPENAI_DEBUG";

pub(crate) fn runtime_provider_bundle_from_config(
    merry_config: Option<&MerryConfig>,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeProviderBundle, CliError> {
    runtime_provider_bundle_from_config_with_primary_override(
        merry_config,
        None,
        None,
        map_usage_error,
    )
}

pub(crate) fn runtime_provider_bundle_from_config_with_primary_override(
    merry_config: Option<&MerryConfig>,
    provider_override: Option<&str>,
    model_override: Option<&str>,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeProviderBundle, CliError> {
    runtime_provider_bundle_from_config_inner(
        merry_config,
        provider_override,
        model_override,
        map_usage_error,
    )
}

fn runtime_provider_bundle_from_config_inner(
    merry_config: Option<&MerryConfig>,
    provider_override: Option<&str>,
    model_override: Option<&str>,
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
    let context_compaction = runtime_models
        .context_compaction
        .map(|model| {
            let provider = merry_config
                .provider_by_alias(&model.provider)
                .map_err(|error| map_usage_error(error.to_string()))?;
            runtime_role_provider(
                RuntimeModelRole::ContextCompaction,
                provider,
                model.model,
                map_usage_error,
            )
        })
        .transpose()?;
    let approval_review = runtime_models
        .approval_review
        .map(|model| {
            let provider = merry_config
                .provider_by_alias(&model.provider)
                .map_err(|error| map_usage_error(error.to_string()))?;
            runtime_role_provider(
                RuntimeModelRole::ApprovalReview,
                provider,
                model.model,
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
    Ok(RuntimeRoleProviderConfig {
        role,
        provider: model_provider(provider, map_usage_error)?,
        model: ModelName::new(&model).map_err(|error| map_usage_error(error.to_string()))?,
    })
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

pub(crate) fn openai_provider_config_bundle(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
    map_usage_error: fn(String) -> CliError,
) -> Result<OpenAiProviderConfigBundle, CliError> {
    let merry_config = merry_config.ok_or_else(|| {
        map_usage_error(
            "Merry XDG provider config is required for OpenAI-compatible runtime".to_owned(),
        )
    })?;
    let provider_config = merry_config
        .openai_compatible_provider()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let api_key = provider_config
        .resolve_api_key()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let primary_model = match model_flag {
        Some(model) => model.to_owned(),
        None => provider_config.model.clone().ok_or_else(|| {
            map_usage_error(
                "[providers.default].model must be set or --model must be provided".to_owned(),
            )
        })?,
    };
    let primary =
        openai_provider_config(&provider_config, &api_key, primary_model, map_usage_error)?;

    let runtime_models = merry_config
        .runtime_models()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let retry_policy = merry_config
        .provider_retry_policy()
        .map_err(|error| map_usage_error(error.to_string()))?;
    let context_compaction = runtime_models
        .context_compaction
        .map(|model| {
            openai_provider_config(&provider_config, &api_key, model.model, map_usage_error)
        })
        .transpose()?;
    let approval_review = runtime_models
        .approval_review
        .map(|model| {
            openai_provider_config(&provider_config, &api_key, model.model, map_usage_error)
        })
        .transpose()?;

    Ok(OpenAiProviderConfigBundle {
        primary,
        context_compaction,
        approval_review,
        retry_policy,
    })
}

fn openai_provider_config(
    provider_config: &EffectiveOpenAiProviderConfig,
    api_key: &str,
    model: String,
    map_usage_error: fn(String) -> CliError,
) -> Result<OpenAiConfig, CliError> {
    let mut provider = OpenAiProviderConfig::new(api_key)
        .map_err(|error| map_usage_error(error.to_string()))?
        .with_protocol(provider_config.protocol)
        .with_provider_name(&provider_config.alias)
        .map_err(|error| map_usage_error(error.to_string()))?;
    if let Some(base_url) = provider_config.base_url.as_deref() {
        provider = provider
            .with_base_url(base_url)
            .map_err(|error| map_usage_error(error.to_string()))?;
    }
    Ok(OpenAiConfig { provider, model })
}

pub(crate) fn apply_openai_context_compaction_provider(
    mut builder: RuntimeBuilder,
    context_compaction: Option<OpenAiConfig>,
) -> Result<RuntimeBuilder, CliError> {
    if let Some(config) = context_compaction {
        let role_provider = openai_context_compaction_provider(config)?;
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    Ok(builder)
}

pub(crate) fn openai_context_compaction_provider(
    config: OpenAiConfig,
) -> Result<RuntimeRoleProviderConfig, CliError> {
    openai_role_provider_config(
        RuntimeModelRole::ContextCompaction,
        config,
        debug_openai_usage_error,
    )
}

pub(crate) fn openai_role_provider_config(
    role: RuntimeModelRole,
    config: OpenAiConfig,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeRoleProviderConfig, CliError> {
    Ok(RuntimeRoleProviderConfig {
        role,
        provider: Arc::new(OpenAiProvider::new(config.provider)),
        model: ModelName::new(&config.model).map_err(|error| map_usage_error(error.to_string()))?,
    })
}

pub(crate) fn openai_provider_bundle(
    config: OpenAiProviderConfigBundle,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimeProviderBundle, CliError> {
    Ok(RuntimeProviderBundle {
        primary: openai_primary_provider(config.primary, map_usage_error)?,
        context_compaction: config
            .context_compaction
            .map(|config| {
                openai_role_provider_config(
                    RuntimeModelRole::ContextCompaction,
                    config,
                    map_usage_error,
                )
            })
            .transpose()?,
        approval_review: config
            .approval_review
            .map(|config| {
                openai_role_provider_config(
                    RuntimeModelRole::ApprovalReview,
                    config,
                    map_usage_error,
                )
            })
            .transpose()?,
        retry_policy: config.retry_policy,
    })
}

fn openai_primary_provider(
    config: OpenAiConfig,
    map_usage_error: fn(String) -> CliError,
) -> Result<RuntimePrimaryProviderConfig, CliError> {
    Ok(RuntimePrimaryProviderConfig {
        provider: Arc::new(OpenAiProvider::new(config.provider)),
        model: ModelName::new(&config.model).map_err(|error| map_usage_error(error.to_string()))?,
    })
}

pub(crate) fn optional_env(name: &'static str) -> Result<Option<String>, CliError> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(debug_openai_usage_error(format!(
            "{name} must not be blank"
        ))),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(debug_openai_usage_error(format!(
            "{name} must be valid UTF-8"
        ))),
    }
}

pub(crate) struct OpenAiConfig {
    pub(crate) provider: OpenAiProviderConfig,
    pub(crate) model: String,
}

pub(crate) struct OpenAiProviderConfigBundle {
    pub(crate) primary: OpenAiConfig,
    pub(crate) context_compaction: Option<OpenAiConfig>,
    pub(crate) approval_review: Option<OpenAiConfig>,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
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
    use crate::debug::openai::config_with_env;
    use merry_llm::ModelCatalogProvider;
    use merry_runtime::RuntimeModelRole;
    use std::path::PathBuf;

    #[test]
    fn materialized_provider_exposes_profile_and_catalog_boundaries() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "opencode"
model = "deepseek-v4-pro"

[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
type = "openai-compatible"
protocol = "chat_completions"
base_url = "https://opencode.example.test/v1"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist");

        let resolved = super::materialized_provider_from_config(
            &config,
            "opencode",
            crate::cli_error::debug_openai_usage_error,
        )
        .expect("provider should materialize");
        let _catalog: std::sync::Arc<dyn ModelCatalogProvider> = resolved.model_catalog.clone();

        assert_eq!(resolved.profile.display_name(), "OpenCode");
        assert_eq!(resolved.profile.alias().as_str(), "opencode");
        assert_eq!(resolved.inference.name().as_str(), "opencode");
    }

    #[test]
    fn managed_only_provider_selection_builds_without_global_default() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
type = "openai-compatible"
protocol = "chat_completions"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist");

        let primary = super::runtime_primary_provider_from_config_with_override(
            Some(&config),
            Some("opencode"),
            None,
            crate::cli_error::debug_openai_usage_error,
        )
        .expect("profile default model should resolve");

        assert_eq!(primary.provider.name().as_str(), "opencode");
        assert_eq!(primary.model.as_str(), "deepseek-v4-pro");
    }

    #[test]
    fn openai_debug_config_uses_xdg_toml_provider_and_secret_file() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let config_dir = temp.path().join("config/merry");
        std::fs::create_dir_all(config_dir.join("secrets")).expect("config dir should be created");
        std::fs::write(config_dir.join("secrets/openai.key"), "sk-test\n")
            .expect("secret file should write");
        let paths = XdgPaths::from_parts(
            PathBuf::from("/home/alice"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
api_key_file = "secrets/openai.key"

[models.context_compaction]
model = "gpt-compact"

[models.approval_review]
model = "gpt-review"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let loaded = config_with_env(None, Some(&config), |name| {
            Ok((name == super::MERRY_OPENAI_DEBUG_ENV).then(|| "1".to_owned()))
        })
        .expect("debug config should load");
        assert_eq!(loaded.primary.model, "gpt-test");
        assert_eq!(
            loaded.primary.provider.base_url(),
            "https://api.example.test/v1"
        );
        let context_compaction = loaded
            .context_compaction
            .expect("context compaction debug config should load");
        assert_eq!(context_compaction.model, "gpt-compact");
        assert_eq!(
            context_compaction.provider.base_url(),
            "https://api.example.test/v1"
        );
        let approval_review = loaded
            .approval_review
            .expect("approval review debug config should load");
        assert_eq!(approval_review.model, "gpt-review");
        assert_eq!(
            approval_review.provider.base_url(),
            "https://api.example.test/v1"
        );
    }

    #[test]
    fn openai_debug_model_flag_overrides_only_primary_model() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.openai-compatible]
protocol = "chat_completions"
api_key = "sk-inline-secret"

[models.context_compaction]
provider = "openai-compatible"
model = "gpt-compact"

[models.approval_review]
provider = "openai-compatible"
model = "gpt-review"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let loaded = config_with_env(Some("gpt-flag"), Some(&config), |name| {
            Ok((name == super::MERRY_OPENAI_DEBUG_ENV).then(|| "1".to_owned()))
        })
        .expect("debug config should load");

        assert_eq!(loaded.primary.model, "gpt-flag");
        assert_eq!(
            loaded
                .context_compaction
                .expect("context compaction debug config should load")
                .model,
            "gpt-compact"
        );
        assert_eq!(
            loaded
                .approval_review
                .expect("approval review debug config should load")
                .model,
            "gpt-review"
        );
    }

    #[test]
    fn runtime_bundle_builds_anthropic_primary_and_openai_role_override() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "claude"
model = "claude-primary"

[providers.claude]
type = "anthropic"
api_key = "sk-ant-test"

[providers.compat]
type = "openai-compatible"
protocol = "chat_completions"
api_key = "sk-openai-test"

[models.context_compaction]
provider = "compat"
model = "gpt-compact"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist");

        let bundle = super::runtime_provider_bundle_from_config(
            Some(&config),
            crate::cli_error::debug_openai_usage_error,
        )
        .expect("runtime provider bundle should build");
        assert_eq!(bundle.primary.provider.name().as_str(), "claude");
        assert_eq!(bundle.primary.model.as_str(), "claude-primary");
        let compaction = bundle
            .context_compaction
            .expect("compaction override should be present");
        assert_eq!(compaction.provider.name().as_str(), "compat");
        assert_eq!(compaction.model.as_str(), "gpt-compact");
    }

    #[test]
    fn runtime_bundle_overrides_only_the_primary_provider_and_model() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "compat"
model = "gpt-default"

[providers.compat]
type = "openai-compatible"
api_key = "sk-openai-test"

[providers.claude]
type = "anthropic"
api_key = "sk-ant-test"

[models.context_compaction]
provider = "compat"
model = "gpt-compact"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist");

        let bundle = super::runtime_provider_bundle_from_config_with_primary_override(
            Some(&config),
            Some("claude"),
            Some("claude-next"),
            crate::cli_error::debug_openai_usage_error,
        )
        .expect("runtime provider bundle should build");

        assert_eq!(bundle.primary.provider.name().as_str(), "claude");
        assert_eq!(bundle.primary.model.as_str(), "claude-next");
        let compaction = bundle
            .context_compaction
            .expect("compaction override should remain configured");
        assert_eq!(compaction.provider.name().as_str(), "compat");
        assert_eq!(compaction.model.as_str(), "gpt-compact");
    }

    #[test]
    fn openai_provider_bundle_materializes_models_and_roles() {
        let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-primary"

[providers.openai-compatible]
protocol = "chat_completions"
api_key = "sk-inline-secret"

[providers.retry]
enabled = true
max_attempts = 3

[models.context_compaction]
model = "gpt-compact"

[models.approval_review]
model = "gpt-review"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");
        let runtime_config = super::openai_provider_config_bundle(None, Some(&config), |message| {
            crate::cli_error::CliError::Unexpected(message)
        })
        .expect("runtime config should load");
        assert_eq!(
            runtime_config.primary.provider.protocol(),
            merry_provider_openai::OpenAiProtocol::ChatCompletions
        );

        let bundle = super::openai_provider_bundle(runtime_config, |message| {
            crate::cli_error::CliError::Unexpected(message)
        })
        .expect("runtime provider bundle should build");

        assert_eq!(bundle.primary.model.as_str(), "gpt-primary");
        let context_compaction = bundle
            .context_compaction
            .expect("context compaction provider should be materialized");
        assert_eq!(context_compaction.role, RuntimeModelRole::ContextCompaction);
        assert_eq!(context_compaction.model.as_str(), "gpt-compact");
        let approval_review = bundle
            .approval_review
            .expect("approval review provider should be materialized");
        assert_eq!(approval_review.role, RuntimeModelRole::ApprovalReview);
        assert_eq!(approval_review.model.as_str(), "gpt-review");
        assert_eq!(
            bundle
                .retry_policy
                .expect("retry policy should be materialized")
                .max_attempts(),
            3
        );
    }
}
