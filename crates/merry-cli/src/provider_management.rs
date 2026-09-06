use crate::{
    cli_error::{CliError, debug_openai_usage_error},
    config::{
        ConfigError, ConfiguredProviderProfile, EffectiveProviderConfig, ManagedProviderDefinition,
        ManagedProviderKind, ManagedProviderStore, ManagedProviderStoreError, MerryConfig,
        ProviderAlias, ProviderConfigSource, XdgPaths,
    },
    provider_config::materialized_provider_from_config,
};
use merry_llm::{
    ModelCatalog, ModelCatalogEntry, ModelCatalogError, ModelCatalogProvider, ModelName,
    ReasoningEffort,
};
use merry_provider_anthropic::{AnthropicProvider, AnthropicProviderConfig};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use serde::{Deserialize, Serialize};
use std::{
    fmt, io,
    path::{Path, PathBuf},
    str,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

const MODEL_CACHE_VERSION: u32 = 1;
static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) struct ProviderDraft {
    display_name: String,
    alias: ProviderAlias,
    kind: ManagedProviderKind,
    protocol: Option<merry_provider_openai::OpenAiProtocol>,
    base_url: String,
    api_key: Option<SecretString>,
    reasoning_effort: Option<ReasoningEffort>,
    default_model: ModelName,
}

pub(crate) struct ProviderDiscoveryDraft {
    alias: ProviderAlias,
    original_alias: Option<ProviderAlias>,
    kind: ManagedProviderKind,
    protocol: Option<merry_provider_openai::OpenAiProtocol>,
    base_url: String,
    api_key: Option<SecretString>,
}

impl ProviderDiscoveryDraft {
    pub(crate) fn new(
        alias: ProviderAlias,
        original_alias: Option<ProviderAlias>,
        kind: ManagedProviderKind,
        protocol: Option<merry_provider_openai::OpenAiProtocol>,
        base_url: &str,
        api_key: Option<&str>,
    ) -> Result<Self, ProviderManagementError> {
        match (kind, protocol) {
            (ManagedProviderKind::OpenAiCompatible, Some(protocol)) => {
                OpenAiProviderConfig::new("provider-discovery-validation-key")
                    .map_err(provider_adapter_error)?
                    .with_protocol(protocol)
                    .with_base_url(base_url)
                    .map_err(provider_adapter_error)?;
            }
            (ManagedProviderKind::OpenAiCompatible, None) => {
                return Err(ProviderManagementError::Invalid(
                    "OpenAI-compatible providers must select Responses or Chat Completions"
                        .to_owned(),
                ));
            }
            (ManagedProviderKind::Anthropic, None) => {
                AnthropicProviderConfig::new("provider-discovery-validation-key")
                    .map_err(provider_adapter_error)?
                    .with_base_url(base_url)
                    .map_err(provider_adapter_error)?;
            }
            (ManagedProviderKind::Anthropic, Some(_)) => {
                return Err(ProviderManagementError::Invalid(
                    "Anthropic providers use the Messages protocol".to_owned(),
                ));
            }
        }
        let api_key = api_key.map(SecretString::new).transpose()?;
        if api_key.is_none() && original_alias.is_none() {
            return Err(ProviderManagementError::Invalid(
                "enter an API key before discovering models for a new provider".to_owned(),
            ));
        }
        Ok(Self {
            alias,
            original_alias,
            kind,
            protocol,
            base_url: base_url.to_owned(),
            api_key,
        })
    }

    pub(crate) fn alias(&self) -> &ProviderAlias {
        &self.alias
    }
}

impl fmt::Debug for ProviderDiscoveryDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderDiscoveryDraft")
            .field("alias", &self.alias)
            .field("original_alias", &self.original_alias)
            .field("kind", &self.kind)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field(
                "api_key",
                &if self.api_key.is_some() {
                    "<redacted>"
                } else {
                    "<retained>"
                },
            )
            .finish()
    }
}

impl ProviderDraft {
    pub(crate) fn new(
        display_name: &str,
        alias: ProviderAlias,
        kind: ManagedProviderKind,
        protocol: Option<merry_provider_openai::OpenAiProtocol>,
        base_url: &str,
        api_key: &str,
        default_model: ModelName,
    ) -> Result<Self, ProviderManagementError> {
        Self::for_update(
            display_name,
            alias,
            kind,
            protocol,
            base_url,
            Some(api_key),
            default_model,
        )
    }

    pub(crate) fn for_update(
        display_name: &str,
        alias: ProviderAlias,
        kind: ManagedProviderKind,
        protocol: Option<merry_provider_openai::OpenAiProtocol>,
        base_url: &str,
        api_key: Option<&str>,
        default_model: ModelName,
    ) -> Result<Self, ProviderManagementError> {
        let _ = ManagedProviderDefinition::with_reasoning_effort(
            alias.clone(),
            display_name,
            default_model.clone(),
            kind,
            protocol,
            base_url,
            None,
        )?;
        Ok(Self {
            display_name: display_name.to_owned(),
            alias,
            kind,
            protocol,
            base_url: base_url.to_owned(),
            api_key: api_key.map(SecretString::new).transpose()?,
            reasoning_effort: None,
            default_model,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_reasoning_effort(
        mut self,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    pub(crate) fn with_reasoning_effort_text(
        mut self,
        value: &str,
    ) -> Result<Self, ProviderManagementError> {
        let value = value.trim();
        self.reasoning_effort = if value.is_empty() {
            None
        } else {
            Some(ReasoningEffort::new(value).map_err(|error| {
                ProviderManagementError::Invalid(format!(
                    "provider reasoning effort is invalid: {error}"
                ))
            })?)
        };
        Ok(self)
    }

    pub(crate) fn alias(&self) -> &ProviderAlias {
        &self.alias
    }

    pub(crate) fn reasoning_effort(&self) -> Option<&ReasoningEffort> {
        self.reasoning_effort.as_ref()
    }
}

impl fmt::Debug for ProviderDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderDraft")
            .field("display_name", &self.display_name)
            .field("alias", &self.alias)
            .field("kind", &self.kind)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field(
                "api_key",
                &if self.api_key.is_some() {
                    "<redacted>"
                } else {
                    "<unchanged>"
                },
            )
            .field("default_model", &self.default_model)
            .field("reasoning_effort", &self.reasoning_effort)
            .finish()
    }
}

struct SecretString(Vec<u8>);

impl SecretString {
    fn new(value: &str) -> Result<Self, ProviderManagementError> {
        if value.trim().is_empty() || value.trim() != value || value.chars().any(char::is_control) {
            return Err(ProviderManagementError::Invalid(
                "provider API key must be non-blank, trimmed, and free of control characters"
                    .to_owned(),
            ));
        }
        Ok(Self(value.as_bytes().to_vec()))
    }

    fn expose(&self) -> &str {
        str::from_utf8(&self.0).expect("SecretString is constructed from valid UTF-8")
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditableProviderProfile {
    pub(crate) alias: ProviderAlias,
    pub(crate) display_name: String,
    pub(crate) kind: ManagedProviderKind,
    pub(crate) protocol: Option<merry_provider_openai::OpenAiProtocol>,
    pub(crate) base_url: String,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) default_model: ModelName,
}

#[derive(Clone)]
pub(crate) struct ProviderManagementService {
    paths: XdgPaths,
    managed_store: ManagedProviderStore,
    config: Option<MerryConfig>,
}

impl ProviderManagementService {
    pub(crate) fn new(paths: XdgPaths) -> Result<Self, ProviderManagementError> {
        let config = MerryConfig::load_optional(&paths)?;
        let managed_store = ManagedProviderStore::new(&paths);
        Ok(Self {
            paths,
            managed_store,
            config,
        })
    }

    pub(crate) fn config(&self) -> Option<&MerryConfig> {
        self.config.as_ref()
    }

    pub(crate) fn profiles(
        &self,
    ) -> Result<Vec<ConfiguredProviderProfile>, ProviderManagementError> {
        let Some(config) = self.config.as_ref() else {
            return Ok(Vec::new());
        };
        config
            .provider_aliases()
            .into_iter()
            .map(|alias| config.provider_profile(&alias).map_err(Into::into))
            .collect()
    }

    pub(crate) fn editable_provider(
        &self,
        alias: &ProviderAlias,
    ) -> Result<EditableProviderProfile, ProviderManagementError> {
        let config = self.config.as_ref().ok_or_else(|| {
            ProviderManagementError::Invalid("no providers are configured".to_owned())
        })?;
        let profile = config.provider_profile(alias.as_str())?;
        if profile.source() != ProviderConfigSource::Managed {
            return Err(ProviderManagementError::ReadOnlyProvider {
                alias: alias.clone(),
            });
        }
        let default_model = profile.default_model().cloned().ok_or_else(|| {
            ProviderManagementError::Invalid(format!(
                "managed provider {:?} has no default model",
                alias.as_str()
            ))
        })?;
        let (kind, protocol, base_url) = match config.provider_by_alias(alias.as_str())? {
            EffectiveProviderConfig::OpenAiCompatible(provider) => (
                ManagedProviderKind::OpenAiCompatible,
                Some(provider.protocol),
                provider
                    .base_url
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_owned()),
            ),
            EffectiveProviderConfig::Anthropic(provider) => (
                ManagedProviderKind::Anthropic,
                None,
                provider
                    .base_url
                    .unwrap_or_else(|| "https://api.anthropic.com".to_owned()),
            ),
        };
        Ok(EditableProviderProfile {
            alias: alias.clone(),
            display_name: profile.display_name().to_owned(),
            kind,
            protocol,
            base_url,
            reasoning_effort: profile.reasoning_effort().cloned(),
            default_model,
        })
    }

    pub(crate) async fn save_provider(
        &mut self,
        draft: ProviderDraft,
    ) -> Result<(), ProviderManagementError> {
        if let Some(config) = self.config.as_ref()
            && let Ok(profile) = config.provider_profile(draft.alias().as_str())
        {
            let reason = match profile.source() {
                ProviderConfigSource::User => "is owned by user config and is read-only",
                ProviderConfigSource::Managed => "already exists; choose another provider name",
            };
            return Err(ProviderManagementError::Invalid(format!(
                "provider alias {:?} {reason}",
                draft.alias().as_str()
            )));
        }
        let ProviderDraft {
            display_name,
            alias,
            kind,
            protocol,
            base_url,
            api_key,
            reasoning_effort,
            default_model,
        } = draft;
        let api_key = api_key.ok_or_else(|| {
            ProviderManagementError::Invalid("new managed providers require an API key".to_owned())
        })?;
        let definition = ManagedProviderDefinition::with_reasoning_effort(
            alias,
            &display_name,
            default_model,
            kind,
            protocol,
            &base_url,
            reasoning_effort,
        )?;
        self.managed_store
            .upsert(definition, api_key.expose())
            .await?;
        self.config = MerryConfig::load_optional(&self.paths)?;
        Ok(())
    }

    pub(crate) async fn update_provider(
        &mut self,
        original_alias: &ProviderAlias,
        draft: ProviderDraft,
    ) -> Result<(), ProviderManagementError> {
        let profile = self
            .config
            .as_ref()
            .ok_or_else(|| {
                ProviderManagementError::Invalid("no providers are configured".to_owned())
            })?
            .provider_profile(original_alias.as_str())?;
        if profile.source() != ProviderConfigSource::Managed {
            return Err(ProviderManagementError::ReadOnlyProvider {
                alias: original_alias.clone(),
            });
        }
        if draft.alias() != original_alias {
            return Err(ProviderManagementError::Invalid(
                "provider config alias is a stable ID and cannot be renamed".to_owned(),
            ));
        }
        let ProviderDraft {
            display_name,
            alias,
            kind,
            protocol,
            base_url,
            api_key,
            reasoning_effort,
            default_model,
        } = draft;
        let definition = ManagedProviderDefinition::with_reasoning_effort(
            alias,
            &display_name,
            default_model,
            kind,
            protocol,
            &base_url,
            reasoning_effort,
        )?;
        self.managed_store
            .update(
                original_alias,
                definition,
                api_key.as_ref().map(SecretString::expose),
            )
            .await?;
        self.config = MerryConfig::load_optional(&self.paths)?;
        Ok(())
    }

    pub(crate) async fn delete_provider(
        &mut self,
        alias: &ProviderAlias,
    ) -> Result<(), ProviderManagementError> {
        let profile = self
            .config
            .as_ref()
            .ok_or_else(|| {
                ProviderManagementError::Invalid("no providers are configured".to_owned())
            })?
            .provider_profile(alias.as_str())?;
        if profile.source() != ProviderConfigSource::Managed {
            return Err(ProviderManagementError::ReadOnlyProvider {
                alias: alias.clone(),
            });
        }
        self.managed_store.delete(alias).await?;
        self.config = MerryConfig::load_optional(&self.paths)?;
        let cache_path = self.model_cache_path(alias);
        match tokio::fs::remove_file(&cache_path).await {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
        Ok(())
    }

    pub(crate) async fn discover_and_cache(
        &self,
        alias: &ProviderAlias,
        cancellation_token: CancellationToken,
    ) -> Result<ModelCatalog, ProviderManagementError> {
        let config = self.config.as_ref().ok_or_else(|| {
            ProviderManagementError::Invalid("no providers are configured".to_owned())
        })?;
        let provider =
            materialized_provider_from_config(config, alias.as_str(), debug_openai_usage_error)
                .map_err(|error| ProviderManagementError::Invalid(cli_error_message(error)))?;
        if provider.profile.alias() != alias || provider.inference.name().as_str() != alias.as_str()
        {
            return Err(ProviderManagementError::Invalid(
                "materialized provider handles do not match the requested alias".to_owned(),
            ));
        }
        self.discover_and_cache_with(alias, provider.model_catalog, cancellation_token)
            .await
    }

    pub(crate) async fn discover_from_draft(
        &self,
        draft: ProviderDiscoveryDraft,
        cancellation_token: CancellationToken,
    ) -> Result<ModelCatalog, ProviderManagementError> {
        let api_key = match draft.api_key.as_ref() {
            Some(api_key) => api_key.expose().to_owned(),
            None => {
                self.resolve_retained_api_key(draft.original_alias.as_ref().ok_or_else(|| {
                    ProviderManagementError::Invalid(
                        "model discovery requires an API key".to_owned(),
                    )
                })?)?
            }
        };
        let provider: Arc<dyn ModelCatalogProvider> = match draft.kind {
            ManagedProviderKind::OpenAiCompatible => {
                let protocol = draft.protocol.ok_or_else(|| {
                    ProviderManagementError::Invalid(
                        "OpenAI-compatible providers must select a protocol".to_owned(),
                    )
                })?;
                let config = OpenAiProviderConfig::new(&api_key)
                    .map_err(provider_adapter_error)?
                    .with_protocol(protocol)
                    .with_provider_name(draft.alias.as_str())
                    .map_err(provider_adapter_error)?
                    .with_base_url(&draft.base_url)
                    .map_err(provider_adapter_error)?;
                Arc::new(OpenAiProvider::new(config))
            }
            ManagedProviderKind::Anthropic => {
                let config = AnthropicProviderConfig::new(&api_key)
                    .map_err(provider_adapter_error)?
                    .with_provider_name(draft.alias.as_str())
                    .map_err(provider_adapter_error)?
                    .with_base_url(&draft.base_url)
                    .map_err(provider_adapter_error)?;
                Arc::new(AnthropicProvider::new(config))
            }
        };
        provider
            .list_models(cancellation_token)
            .await
            .map_err(Into::into)
    }

    fn resolve_retained_api_key(
        &self,
        original_alias: &ProviderAlias,
    ) -> Result<String, ProviderManagementError> {
        let config = self.config.as_ref().ok_or_else(|| {
            ProviderManagementError::Invalid("no providers are configured".to_owned())
        })?;
        let profile = config.provider_profile(original_alias.as_str())?;
        if profile.source() != ProviderConfigSource::Managed {
            return Err(ProviderManagementError::ReadOnlyProvider {
                alias: original_alias.clone(),
            });
        }
        match config.provider_by_alias(original_alias.as_str())? {
            EffectiveProviderConfig::OpenAiCompatible(provider) => {
                provider.resolve_api_key().map_err(Into::into)
            }
            EffectiveProviderConfig::Anthropic(provider) => {
                provider.resolve_api_key().map_err(Into::into)
            }
        }
    }

    async fn discover_and_cache_with(
        &self,
        alias: &ProviderAlias,
        provider: Arc<dyn ModelCatalogProvider>,
        cancellation_token: CancellationToken,
    ) -> Result<ModelCatalog, ProviderManagementError> {
        let catalog = provider.list_models(cancellation_token).await?;
        self.save_model_cache(alias, &catalog).await?;
        Ok(catalog)
    }

    pub(crate) async fn load_model_cache(
        &self,
        alias: &ProviderAlias,
    ) -> Result<Option<ModelCatalog>, ProviderManagementError> {
        let path = self.model_cache_path(alias);
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(cache_io("read", &path, source)),
        };
        let document = serde_json::from_str::<ModelCacheDocument>(&text).map_err(|source| {
            ProviderManagementError::CacheParse {
                path: path.clone(),
                source,
            }
        })?;
        if document.version != MODEL_CACHE_VERSION {
            return Err(ProviderManagementError::Invalid(format!(
                "model cache version {} is unsupported; expected {MODEL_CACHE_VERSION}",
                document.version
            )));
        }
        let mut models = Vec::with_capacity(document.models.len());
        for model in document.models {
            let id = ModelName::new(&model.id)
                .map_err(|error| ProviderManagementError::Invalid(error.to_string()))?;
            models.push(ModelCatalogEntry::new(id, model.owner.as_deref())?);
        }
        Ok(Some(ModelCatalog::new(models)))
    }

    async fn save_model_cache(
        &self,
        alias: &ProviderAlias,
        catalog: &ModelCatalog,
    ) -> Result<(), ProviderManagementError> {
        let directory = self.paths.model_catalog_cache_dir();
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|source| cache_io("create directory for", &directory, source))?;
        let document = ModelCacheDocument {
            version: MODEL_CACHE_VERSION,
            fetched_at_unix_ms: now_unix_ms(),
            models: catalog
                .models()
                .iter()
                .map(|model| ModelCacheEntry {
                    id: model.id().as_str().to_owned(),
                    owner: model.owner().map(str::to_owned),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&document)?;
        let path = self.model_cache_path(alias);
        let temp_path = directory.join(format!(
            ".{}.json.tmp-{}-{}",
            alias.as_str(),
            std::process::id(),
            CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|source| cache_io("create temporary", &temp_path, source))?;
        file.write_all(&bytes)
            .await
            .map_err(|source| cache_io("write temporary", &temp_path, source))?;
        file.write_all(b"\n")
            .await
            .map_err(|source| cache_io("write temporary", &temp_path, source))?;
        file.sync_all()
            .await
            .map_err(|source| cache_io("sync temporary", &temp_path, source))?;
        drop(file);
        if let Err(source) = tokio::fs::rename(&temp_path, &path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(cache_io("replace", &path, source));
        }
        Ok(())
    }

    fn model_cache_path(&self, alias: &ProviderAlias) -> PathBuf {
        self.paths
            .model_catalog_cache_dir()
            .join(format!("{}.json", alias.as_str()))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelCacheDocument {
    version: u32,
    fetched_at_unix_ms: u128,
    models: Vec<ModelCacheEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelCacheEntry {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn cache_io(operation: &'static str, path: &Path, source: io::Error) -> ProviderManagementError {
    ProviderManagementError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn cli_error_message(error: CliError) -> String {
    match error {
        CliError::DebugUsage(message)
        | CliError::DebugOpenAiUsage(message)
        | CliError::ShellUsage(message)
        | CliError::Unexpected(message) => message,
        CliError::BrokenPipe => "provider construction stopped by a broken pipe".to_owned(),
    }
}

fn provider_adapter_error(error: impl fmt::Display) -> ProviderManagementError {
    ProviderManagementError::Invalid(error.to_string())
}

#[derive(Debug, Error)]
pub(crate) enum ProviderManagementError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    ManagedStore(#[from] ManagedProviderStoreError),
    #[error(transparent)]
    Catalog(#[from] ModelCatalogError),
    #[error("failed to {operation} provider management file {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse model cache {path}: {source}")]
    CacheParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize model cache: {0}")]
    CacheSerialize(#[from] serde_json::Error),
    #[error("provider {alias} is defined in config.toml and is read-only in the TUI")]
    ReadOnlyProvider { alias: ProviderAlias },
    #[error("invalid provider management request: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests;
