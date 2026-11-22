use super::{
    ConfigError, MerryConfig, default_true, managed_provider::ProviderAlias,
    resolve_provider_api_key_file_path,
};
use merry_llm::{ModelName, ModelRetryPolicy, ModelRetryPolicyError, ReasoningEffort, ServiceTier};
use merry_provider_openai::OpenAiProtocol;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

impl MerryConfig {
    pub fn provider_aliases(&self) -> Vec<String> {
        self.managed_providers.keys().cloned().collect()
    }

    pub(crate) fn provider_profile(
        &self,
        alias: &str,
    ) -> Result<ConfiguredProviderProfile, ConfigError> {
        let alias = ProviderAlias::new(alias)?;
        let provider = self
            .managed_providers
            .get(alias.as_str())
            .ok_or_else(|| self.missing_provider_error(alias.as_str()))?;
        let kind = match provider.kind.as_deref().unwrap_or(alias.as_str()) {
            "openai-compatible" => ConfiguredProviderKind::OpenAiCompatible,
            "anthropic" => ConfiguredProviderKind::Anthropic,
            other => {
                return Err(ConfigError::Invalid(format!(
                    "unsupported provider type {other:?} for [providers.{alias}]"
                )));
            }
        };
        let display_name = provider
            .display_name
            .clone()
            .unwrap_or_else(|| alias.as_str().to_owned());
        validate_provider_display_name(&display_name)?;
        let default_model = provider
            .default_model
            .as_deref()
            .or_else(|| {
                self.raw
                    .providers
                    .as_ref()
                    .and_then(|providers| providers.default.as_ref())
                    .filter(|default| default.provider == alias.as_str())
                    .map(|default| default.model.as_str())
            })
            .map(ModelName::new)
            .transpose()
            .map_err(|error| ConfigError::Invalid(error.to_string()))?;
        let reasoning_effort = parse_reasoning_effort(
            &format!("providers.{alias}.reasoning_effort"),
            provider.reasoning_effort.as_deref(),
        )?;
        let service_tier =
            parse_provider_service_tier(alias.as_str(), provider.service_tier.as_deref())?;
        let protocol = match kind {
            ConfiguredProviderKind::OpenAiCompatible => Some(provider.protocol.unwrap_or_default()),
            ConfiguredProviderKind::Anthropic => None,
        };
        if let Some(service_tier) = service_tier {
            validate_service_tier_support(
                &format!("providers.{alias}.service_tier"),
                service_tier,
                kind,
                protocol,
            )?;
        }

        Ok(ConfiguredProviderProfile {
            alias,
            display_name,
            default_model,
            kind,
            protocol,
            reasoning_effort,
            service_tier,
        })
    }

    pub(crate) fn provider_reasoning_effort(
        &self,
        alias: &str,
    ) -> Result<Option<ReasoningEffort>, ConfigError> {
        Ok(self.provider_profile(alias)?.reasoning_effort().cloned())
    }

    pub(crate) fn effective_provider_reasoning_effort(
        &self,
        alias: &str,
    ) -> Result<Option<ReasoningEffort>, ConfigError> {
        let default_reasoning_effort = self
            .raw
            .providers
            .as_ref()
            .and_then(|providers| providers.default.as_ref())
            .filter(|default| default.provider == alias)
            .and_then(|default| default.reasoning_effort.as_deref())
            .map(|effort| {
                parse_reasoning_effort("providers.default.reasoning_effort", Some(effort))
            })
            .transpose()?
            .flatten();

        match default_reasoning_effort {
            Some(reasoning_effort) => Ok(Some(reasoning_effort)),
            None => self.provider_reasoning_effort(alias),
        }
    }

    pub(crate) fn provider_service_tier(
        &self,
        alias: &str,
    ) -> Result<Option<ServiceTier>, ConfigError> {
        Ok(self.provider_profile(alias)?.service_tier())
    }

    pub(crate) fn effective_provider_service_tier(
        &self,
        alias: &str,
    ) -> Result<Option<ServiceTier>, ConfigError> {
        let default_service_tier = self
            .raw
            .providers
            .as_ref()
            .and_then(|providers| providers.default.as_ref())
            .filter(|default| default.provider == alias)
            .and_then(|default| default.service_tier.as_deref())
            .map(ServiceTier::new)
            .transpose()
            .map_err(|error| {
                ConfigError::Invalid(format!(
                    "providers.default.service_tier is invalid: {error}"
                ))
            })?;

        let Some(service_tier) = default_service_tier else {
            return self.provider_service_tier(alias);
        };
        let profile = self.provider_profile(alias)?;
        validate_service_tier_support(
            "providers.default.service_tier",
            service_tier,
            profile.kind(),
            profile.protocol(),
        )?;
        Ok(Some(service_tier))
    }

    pub fn validate_provider_settings_if_present(&self) -> Result<(), ConfigError> {
        for alias in self.managed_providers.keys() {
            let _ = self.provider_profile(alias)?;
            let _ = self.provider_by_alias(alias)?;
        }
        if self
            .raw
            .providers
            .as_ref()
            .is_some_and(|providers| providers.default.is_some())
        {
            let _ = self.default_provider()?;
        }
        Ok(())
    }

    pub fn openai_compatible_provider(&self) -> Result<EffectiveOpenAiProviderConfig, ConfigError> {
        let default_alias = self
            .raw
            .providers
            .as_ref()
            .and_then(|providers| providers.default.as_ref())
            .map(|default| default.provider.as_str());
        if let Some(alias) = default_alias
            && alias != "openai-compatible"
            && !self.managed_providers.contains_key(alias)
        {
            return Err(ConfigError::Invalid(format!(
                "unsupported default provider {alias}"
            )));
        }
        self.legacy_openai_provider()
    }

    pub fn default_provider(&self) -> Result<EffectiveDefaultProviderConfig, ConfigError> {
        let providers =
            self.raw.providers.as_ref().ok_or_else(|| {
                ConfigError::Invalid("[providers.default] is required".to_owned())
            })?;
        let default = providers
            .default
            .as_ref()
            .ok_or_else(|| ConfigError::Invalid("[providers.default] is required".to_owned()))?;
        let provider = self.provider_by_alias(&default.provider)?;
        let reasoning_effort = self.effective_provider_reasoning_effort(&default.provider)?;
        let service_tier = self.effective_provider_service_tier(&default.provider)?;
        Ok(EffectiveDefaultProviderConfig {
            alias: default.provider.clone(),
            model: default.model.clone(),
            reasoning_effort,
            service_tier,
            provider,
        })
    }

    pub fn configured_default_provider(
        &self,
    ) -> Result<Option<EffectiveDefaultProviderConfig>, ConfigError> {
        if self
            .raw
            .providers
            .as_ref()
            .and_then(|providers| providers.default.as_ref())
            .is_none()
        {
            return Ok(None);
        }
        self.default_provider().map(Some)
    }

    pub fn provider_by_alias(&self, alias: &str) -> Result<EffectiveProviderConfig, ConfigError> {
        let provider = self
            .managed_providers
            .get(alias)
            .ok_or_else(|| self.missing_provider_error(alias))?;
        let kind = provider.kind.as_deref().unwrap_or(alias);
        let api_key_file =
            resolve_api_key_file_path(alias, provider, &self.providers_dir, &self.home)?;
        let reasoning_effort = parse_reasoning_effort(
            &format!("providers.{alias}.reasoning_effort"),
            provider.reasoning_effort.as_deref(),
        )?;
        let service_tier = parse_provider_service_tier(alias, provider.service_tier.as_deref())?;
        let protocol = provider.protocol.unwrap_or_default();
        match kind {
            "openai-compatible" => {
                if let Some(service_tier) = service_tier {
                    validate_service_tier_support(
                        &format!("providers.{alias}.service_tier"),
                        service_tier,
                        ConfiguredProviderKind::OpenAiCompatible,
                        Some(protocol),
                    )?;
                }
                Ok(EffectiveProviderConfig::OpenAiCompatible(
                    EffectiveOpenAiProviderConfig {
                        model: None,
                        reasoning_effort,
                        service_tier,
                        alias: alias.to_owned(),
                        protocol,
                        base_url: provider.base_url.clone(),
                        api_key_file,
                    },
                ))
            }
            "anthropic" => {
                if let Some(service_tier) = service_tier {
                    validate_service_tier_support(
                        &format!("providers.{alias}.service_tier"),
                        service_tier,
                        ConfiguredProviderKind::Anthropic,
                        None,
                    )?;
                }
                Ok(EffectiveProviderConfig::Anthropic(
                    EffectiveAnthropicProviderConfig {
                        alias: alias.to_owned(),
                        reasoning_effort,
                        base_url: provider.base_url.clone(),
                        api_version: provider.api_version.clone(),
                        default_max_output_tokens: provider.default_max_output_tokens,
                        api_key_file,
                    },
                ))
            }
            other => Err(ConfigError::Invalid(format!(
                "unsupported provider type {other:?} for [providers.{alias}]"
            ))),
        }
    }

    pub(super) fn validate_provider_alias(&self, alias: &str) -> Result<(), ConfigError> {
        let _ = self.provider_by_alias(alias)?;
        Ok(())
    }

    fn missing_provider_error(&self, alias: &str) -> ConfigError {
        ConfigError::Invalid(format!(
            "provider {alias:?} is not configured in {} (add it in Merry Settings or edit that file directly)",
            self.providers_dir.join("providers.toml").display()
        ))
    }

    fn legacy_openai_provider(&self) -> Result<EffectiveOpenAiProviderConfig, ConfigError> {
        let default = self.default_provider()?;
        let EffectiveProviderConfig::OpenAiCompatible(mut provider) = default.provider else {
            return Err(ConfigError::Invalid(
                "default provider is not openai-compatible".to_owned(),
            ));
        };
        provider.model = Some(default.model);
        provider.reasoning_effort = default.reasoning_effort;
        provider.service_tier = default.service_tier;
        Ok(provider)
    }

    pub fn provider_retry_policy(&self) -> Result<Option<ModelRetryPolicy>, ConfigError> {
        let Some(providers) = self.raw.providers.as_ref() else {
            return Ok(None);
        };
        providers
            .retry
            .as_ref()
            .map(ProviderRetryToml::to_policy)
            .transpose()
    }
}

fn resolve_api_key_file_path(
    alias: &str,
    provider: &NamedProviderToml,
    providers_dir: &Path,
    home: &Path,
) -> Result<PathBuf, ConfigError> {
    let path = provider.api_key_file.as_deref().ok_or_else(|| {
        ConfigError::Invalid(format!(
            "managed provider {alias:?} is missing api_key_file"
        ))
    })?;
    resolve_provider_api_key_file_path(path, providers_dir, home)
}

/// Parses one reasoning-effort config value under its own config path.
///
/// Every place that accepts a reasoning-effort string shares this, so the
/// accepted values and the diagnostic shape stay identical for providers,
/// provider defaults, and runtime compaction.
pub(super) fn parse_reasoning_effort(
    field: &str,
    value: Option<&str>,
) -> Result<Option<ReasoningEffort>, ConfigError> {
    value
        .map(ReasoningEffort::new)
        .transpose()
        .map_err(|error| ConfigError::Invalid(format!("{field} is invalid: {error}")))
}

fn parse_provider_service_tier(
    alias: &str,
    value: Option<&str>,
) -> Result<Option<ServiceTier>, ConfigError> {
    value.map(ServiceTier::new).transpose().map_err(|error| {
        ConfigError::Invalid(format!(
            "providers.{alias}.service_tier is invalid: {error}"
        ))
    })
}

/// Rejects a configured service tier the selected provider cannot carry.
///
/// `location` names the setting in the user's config so the message points at
/// the key that has to change, which differs between `[providers.default]` and
/// `[providers.<alias>]`.
fn validate_service_tier_support(
    location: &str,
    service_tier: ServiceTier,
    kind: ConfiguredProviderKind,
    protocol: Option<OpenAiProtocol>,
) -> Result<(), ConfigError> {
    if kind != ConfiguredProviderKind::OpenAiCompatible {
        return Err(ConfigError::Invalid(format!(
            "{location} is only supported for openai-compatible providers"
        )));
    }
    if !protocol
        .unwrap_or_default()
        .supports_service_tier(service_tier)
    {
        return Err(ConfigError::Invalid(format!(
            "{location} value \"{service_tier}\" requires protocol = \"responses\""
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveDefaultProviderConfig {
    pub alias: String,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub service_tier: Option<ServiceTier>,
    pub provider: EffectiveProviderConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfiguredProviderKind {
    OpenAiCompatible,
    Anthropic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfiguredProviderProfile {
    alias: ProviderAlias,
    display_name: String,
    default_model: Option<ModelName>,
    kind: ConfiguredProviderKind,
    protocol: Option<OpenAiProtocol>,
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<ServiceTier>,
}

impl ConfiguredProviderProfile {
    pub(crate) fn alias(&self) -> &ProviderAlias {
        &self.alias
    }

    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    pub(crate) fn default_model(&self) -> Option<&ModelName> {
        self.default_model.as_ref()
    }

    pub(crate) fn kind(&self) -> ConfiguredProviderKind {
        self.kind
    }

    pub(crate) fn protocol(&self) -> Option<OpenAiProtocol> {
        self.protocol
    }

    pub(crate) fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }

    pub(crate) fn service_tier(&self) -> Option<ServiceTier> {
        self.service_tier
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveProviderConfig {
    OpenAiCompatible(EffectiveOpenAiProviderConfig),
    Anthropic(EffectiveAnthropicProviderConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveOpenAiProviderConfig {
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub service_tier: Option<ServiceTier>,
    pub alias: String,
    pub protocol: OpenAiProtocol,
    pub base_url: Option<String>,
    pub api_key_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveAnthropicProviderConfig {
    pub alias: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub base_url: Option<String>,
    pub api_version: Option<String>,
    pub default_max_output_tokens: Option<u64>,
    pub api_key_file: PathBuf,
}

impl EffectiveOpenAiProviderConfig {
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        read_api_key_file(&self.api_key_file)
    }
}

impl EffectiveAnthropicProviderConfig {
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        read_api_key_file(&self.api_key_file)
    }
}

fn read_api_key_file(path: &Path) -> Result<String, ConfigError> {
    let value = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let value = value.trim().to_owned();
    validate_api_key_text(&format!("api_key_file {}", path.display()), &value)?;
    Ok(value)
}

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ProvidersToml {
    pub(super) default: Option<DefaultProviderToml>,
    retry: Option<ProviderRetryToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct DefaultProviderToml {
    pub(super) provider: String,
    model: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct NamedProviderToml {
    pub(super) display_name: Option<String>,
    pub(super) default_model: Option<String>,
    pub(super) reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) service_tier: Option<String>,
    #[serde(rename = "type")]
    pub(super) kind: Option<String>,
    pub(super) protocol: Option<OpenAiProtocol>,
    pub(super) base_url: Option<String>,
    pub(super) api_version: Option<String>,
    pub(super) default_max_output_tokens: Option<u64>,
    pub(super) api_key_file: Option<String>,
}

pub(super) fn validate_provider_display_name(value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Invalid(
            "provider display_name must not be blank".to_owned(),
        ));
    }
    if value.trim() != value {
        return Err(ConfigError::Invalid(
            "provider display_name must not have leading or trailing whitespace".to_owned(),
        ));
    }
    if value.chars().count() > 128 {
        return Err(ConfigError::Invalid(
            "provider display_name must be at most 128 characters".to_owned(),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(ConfigError::Invalid(
            "provider display_name must not contain control characters".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProviderRetryToml {
    #[serde(default = "default_true")]
    enabled: bool,
    max_attempts: Option<usize>,
    initial_delay_ms: Option<u64>,
    max_delay_ms: Option<u64>,
    max_elapsed_ms: Option<u64>,
    jitter: Option<bool>,
}

impl ProviderRetryToml {
    fn to_policy(&self) -> Result<ModelRetryPolicy, ConfigError> {
        let defaults = ModelRetryPolicy::coding_agent_default();
        let policy = ModelRetryPolicy::new(
            self.enabled,
            self.max_attempts.unwrap_or(defaults.max_attempts()),
            std::time::Duration::from_millis(
                self.initial_delay_ms
                    .unwrap_or_else(|| duration_millis_u64(defaults.initial_delay())),
            ),
            std::time::Duration::from_millis(
                self.max_delay_ms
                    .unwrap_or_else(|| duration_millis_u64(defaults.max_delay())),
            ),
            std::time::Duration::from_millis(
                self.max_elapsed_ms
                    .unwrap_or_else(|| duration_millis_u64(defaults.max_elapsed())),
            ),
            self.jitter.unwrap_or_else(|| defaults.jitter()),
        )
        .map_err(provider_retry_policy_error)?;
        Ok(policy)
    }
}

pub(super) fn validate_api_key_text(label: &str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Invalid(format!("{label} must not be blank")));
    }
    if value.chars().any(char::is_control) {
        return Err(ConfigError::Invalid(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(())
}

fn duration_millis_u64(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn provider_retry_policy_error(error: ModelRetryPolicyError) -> ConfigError {
    ConfigError::Invalid(format!("providers.retry is invalid: {error}"))
}

#[cfg(test)]
mod tests;
