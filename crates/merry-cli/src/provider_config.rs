use crate::coding_runtime::RuntimeRoleProviderConfig;
use crate::config::{EffectiveOpenAiProviderConfig, MerryConfig};
use crate::{CliError, debug_openai_usage_error};
use merry_llm::{ModelName, ModelRetryPolicy};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use merry_runtime::{RuntimeBuilder, RuntimeModelRole};
use std::{env, sync::Arc};

pub(crate) const MERRY_OPENAI_DEBUG_ENV: &str = "MERRY_OPENAI_DEBUG";

pub(crate) fn openai_runtime_config(
    model_flag: Option<&str>,
    merry_config: Option<&MerryConfig>,
    map_usage_error: fn(String) -> CliError,
) -> Result<OpenAiRuntimeConfig, CliError> {
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

    Ok(OpenAiRuntimeConfig {
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
    let mut provider =
        OpenAiProviderConfig::new(api_key).map_err(|error| map_usage_error(error.to_string()))?;
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

pub(crate) fn openai_approval_review_provider(
    config: OpenAiConfig,
) -> Result<RuntimeRoleProviderConfig, CliError> {
    openai_role_provider_config(
        RuntimeModelRole::ApprovalReview,
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

pub(crate) struct OpenAiRuntimeConfig {
    pub(crate) primary: OpenAiConfig,
    pub(crate) context_compaction: Option<OpenAiConfig>,
    pub(crate) approval_review: Option<OpenAiConfig>,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
}

#[cfg(test)]
mod tests {
    use crate::config::{MerryConfig, XdgPaths};
    use crate::debug::openai::config_with_env;
    use std::path::PathBuf;

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
}
