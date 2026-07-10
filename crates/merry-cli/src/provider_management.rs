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
        let _ = ManagedProviderDefinition::new(
            alias.clone(),
            display_name,
            default_model.clone(),
            kind,
            protocol,
            base_url,
        )?;
        Ok(Self {
            display_name: display_name.to_owned(),
            alias,
            kind,
            protocol,
            base_url: base_url.to_owned(),
            api_key: Some(SecretString::new(api_key)?),
            default_model,
        })
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
        let _ = ManagedProviderDefinition::new(
            alias.clone(),
            display_name,
            default_model.clone(),
            kind,
            protocol,
            base_url,
        )?;
        Ok(Self {
            display_name: display_name.to_owned(),
            alias,
            kind,
            protocol,
            base_url: base_url.to_owned(),
            api_key: api_key.map(SecretString::new).transpose()?,
            default_model,
        })
    }

    pub(crate) fn alias(&self) -> &ProviderAlias {
        &self.alias
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
            default_model,
        } = draft;
        let api_key = api_key.ok_or_else(|| {
            ProviderManagementError::Invalid("new managed providers require an API key".to_owned())
        })?;
        let definition = ManagedProviderDefinition::new(
            alias,
            &display_name,
            default_model,
            kind,
            protocol,
            &base_url,
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
            default_model,
        } = draft;
        let definition = ManagedProviderDefinition::new(
            alias,
            &display_name,
            default_model,
            kind,
            protocol,
            &base_url,
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
mod tests {
    use super::*;
    use crate::config::{ManagedProviderKind, ProviderAlias, XdgPaths};
    use merry_llm::{
        ModelCatalog, ModelCatalogEntry, ModelCatalogError, ModelCatalogFuture,
        ModelCatalogProvider, ModelName,
    };
    use merry_provider_openai::OpenAiProtocol;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn provider_draft_debug_redacts_credentials() {
        let draft = ProviderDraft::new(
            "OpenCode",
            ProviderAlias::new("opencode").expect("valid alias"),
            ManagedProviderKind::OpenAiCompatible,
            Some(OpenAiProtocol::ChatCompletions),
            "https://opencode.example.test/v1",
            "sk-super-secret",
            ModelName::new("deepseek-v4-pro").expect("valid model"),
        )
        .expect("valid draft");

        let debug = format!("{draft:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sk-super-secret"));
    }

    #[test]
    fn provider_discovery_draft_requires_new_credentials_and_redacts_them() {
        let alias = ProviderAlias::new("opencode").expect("valid alias");
        let missing = ProviderDiscoveryDraft::new(
            alias.clone(),
            None,
            ManagedProviderKind::OpenAiCompatible,
            Some(OpenAiProtocol::ChatCompletions),
            "https://opencode.example.test/v1",
            None,
        )
        .expect_err("new providers need credentials for discovery");
        assert!(missing.to_string().contains("enter an API key"));

        let draft = ProviderDiscoveryDraft::new(
            alias,
            None,
            ManagedProviderKind::OpenAiCompatible,
            Some(OpenAiProtocol::ChatCompletions),
            "https://opencode.example.test/v1",
            Some("sk-super-secret"),
        )
        .expect("valid discovery draft");
        let debug = format!("{draft:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("sk-super-secret"));
    }

    #[test]
    fn provider_discovery_draft_allows_retained_credentials_when_editing() {
        let draft = ProviderDiscoveryDraft::new(
            ProviderAlias::new("opencode").expect("valid alias"),
            Some(ProviderAlias::new("opencode").expect("valid original alias")),
            ManagedProviderKind::OpenAiCompatible,
            Some(OpenAiProtocol::Responses),
            "https://opencode.example.test/v1",
            None,
        )
        .expect("editing may retain the stored credential");

        assert!(format!("{draft:?}").contains("<retained>"));
    }

    #[tokio::test]
    async fn provider_discovery_resolves_the_managed_credential_for_edits() {
        let (_temp, paths) = test_paths();
        let mut service = ProviderManagementService::new(paths).expect("service");
        let alias = ProviderAlias::new("opencode").expect("valid alias");
        service
            .save_provider(
                ProviderDraft::new(
                    "OpenCode",
                    alias.clone(),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::ChatCompletions),
                    "https://opencode.example.test/v1",
                    "sk-retained-secret",
                    ModelName::new("model-a").expect("valid model"),
                )
                .expect("valid provider"),
            )
            .await
            .expect("save provider");

        let credential = service
            .resolve_retained_api_key(&alias)
            .expect("retained credential");

        assert_eq!(credential, "sk-retained-secret");
    }

    #[tokio::test]
    async fn lists_user_and_managed_profiles_without_deduplicating_endpoints() {
        let (temp, paths) = test_paths();
        tokio::fs::create_dir_all(paths.config_dir())
            .await
            .expect("config dir");
        tokio::fs::write(
            paths.config_file(),
            r#"
[providers.user-gateway]
display_name = "User Gateway"
default_model = "model-user"
type = "openai-compatible"
base_url = "https://gateway.example.test/v1"
api_key = "sk-user"
"#,
        )
        .await
        .expect("user config");
        let mut service = ProviderManagementService::new(paths.clone()).expect("service");
        service
            .save_provider(
                ProviderDraft::new(
                    "Managed Gateway",
                    ProviderAlias::new("managed-gateway").expect("alias"),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::ChatCompletions),
                    "https://gateway.example.test/v1",
                    "sk-managed",
                    ModelName::new("model-managed").expect("model"),
                )
                .expect("draft"),
            )
            .await
            .expect("managed provider save");

        let profiles = service.profiles().expect("profiles");

        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].alias().as_str(), "managed-gateway");
        assert_eq!(profiles[1].alias().as_str(), "user-gateway");
        let error = service
            .editable_provider(&ProviderAlias::new("user-gateway").expect("alias"))
            .expect_err("config.toml provider should be read-only");
        assert!(error.to_string().contains("read-only"));
        assert!(
            !error
                .to_string()
                .contains("invalid provider management request")
        );
        drop(temp);
    }

    #[tokio::test]
    async fn user_alias_collision_fails_before_managed_registry_mutation() {
        let (_temp, paths) = test_paths();
        tokio::fs::create_dir_all(paths.config_dir())
            .await
            .expect("config dir");
        tokio::fs::write(
            paths.config_file(),
            r#"
[providers.opencode]
type = "openai-compatible"
api_key = "sk-user"
"#,
        )
        .await
        .expect("user config");
        let mut service = ProviderManagementService::new(paths.clone()).expect("service");
        let draft = ProviderDraft::new(
            "OpenCode",
            ProviderAlias::new("opencode").expect("alias"),
            ManagedProviderKind::OpenAiCompatible,
            Some(OpenAiProtocol::ChatCompletions),
            "https://opencode.example.test/v1",
            "sk-managed",
            ModelName::new("model-a").expect("model"),
        )
        .expect("draft");

        let error = service
            .save_provider(draft)
            .await
            .expect_err("user alias collision should fail");

        assert!(error.to_string().contains("opencode"));
        assert!(!paths.managed_providers_file().exists());
        assert!(!paths.managed_secrets_dir().exists());
    }

    #[tokio::test]
    async fn add_provider_does_not_overwrite_existing_managed_alias() {
        let (_temp, paths) = test_paths();
        let mut service = ProviderManagementService::new(paths.clone()).expect("service");
        service
            .save_provider(
                ProviderDraft::new(
                    "OpenCode Original",
                    ProviderAlias::new("opencode").expect("alias"),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::ChatCompletions),
                    "https://first.example.test/v1",
                    "sk-first",
                    ModelName::new("model-a").expect("model"),
                )
                .expect("draft"),
            )
            .await
            .expect("first provider save");

        let error = service
            .save_provider(
                ProviderDraft::new(
                    "OpenCode Replacement",
                    ProviderAlias::new("opencode").expect("alias"),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::Responses),
                    "https://second.example.test/v1",
                    "sk-second",
                    ModelName::new("model-b").expect("model"),
                )
                .expect("draft"),
            )
            .await
            .expect_err("add must not overwrite an existing managed alias");

        assert!(error.to_string().contains("already exists"));
        assert_eq!(secret_file_count(&paths).await, 1);
        assert_eq!(
            service
                .profiles()
                .expect("profiles")
                .into_iter()
                .next()
                .expect("profile")
                .display_name(),
            "OpenCode Original"
        );
    }

    #[tokio::test]
    async fn deletes_only_managed_provider_and_removes_its_secret() {
        let (_temp, paths) = test_paths();
        let mut service = ProviderManagementService::new(paths.clone()).expect("service");
        service
            .save_provider(
                ProviderDraft::new(
                    "OpenCode",
                    ProviderAlias::new("opencode").expect("alias"),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::ChatCompletions),
                    "https://opencode.example.test/v1",
                    "sk-managed",
                    ModelName::new("model-a").expect("model"),
                )
                .expect("draft"),
            )
            .await
            .expect("provider save");
        assert_eq!(secret_file_count(&paths).await, 1);

        service
            .delete_provider(&ProviderAlias::new("opencode").expect("alias"))
            .await
            .expect("managed provider delete");

        assert!(service.profiles().expect("profiles").is_empty());
        assert_eq!(secret_file_count(&paths).await, 0);
    }

    #[tokio::test]
    async fn edits_managed_provider_and_retains_api_key_when_blank() {
        let (_temp, paths) = test_paths();
        let mut service = ProviderManagementService::new(paths.clone()).expect("service");
        let alias = ProviderAlias::new("opencode").expect("alias");
        service
            .save_provider(
                ProviderDraft::new(
                    "OpenCode",
                    alias.clone(),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::ChatCompletions),
                    "https://old.example.test/v1",
                    "sk-retained",
                    ModelName::new("model-a").expect("model"),
                )
                .expect("draft"),
            )
            .await
            .expect("provider save");

        let editable = service.editable_provider(&alias).expect("editable profile");
        assert_eq!(editable.display_name, "OpenCode");
        assert_eq!(editable.protocol, Some(OpenAiProtocol::ChatCompletions));
        service
            .update_provider(
                &alias,
                ProviderDraft::for_update(
                    "OpenCode Edited",
                    alias.clone(),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::Responses),
                    "https://new.example.test/v1",
                    None,
                    ModelName::new("model-b").expect("model"),
                )
                .expect("update draft"),
            )
            .await
            .expect("provider update");

        let updated = service.editable_provider(&alias).expect("updated profile");
        assert_eq!(updated.display_name, "OpenCode Edited");
        assert_eq!(updated.protocol, Some(OpenAiProtocol::Responses));
        assert_eq!(updated.base_url, "https://new.example.test/v1");
        assert_eq!(updated.default_model.as_str(), "model-b");
        assert_eq!(secret_file_count(&paths).await, 1);
        let mut secrets = tokio::fs::read_dir(paths.managed_secrets_dir())
            .await
            .expect("secret directory");
        let secret_path = secrets
            .next_entry()
            .await
            .expect("secret entry")
            .expect("secret file")
            .path();
        assert_eq!(
            tokio::fs::read_to_string(secret_path)
                .await
                .expect("secret contents")
                .trim(),
            "sk-retained"
        );
    }

    #[tokio::test]
    async fn model_cache_is_available_before_refresh_and_survives_failure() {
        let (_temp, paths) = test_paths();
        let service = ProviderManagementService::new(paths).expect("service");
        let alias = ProviderAlias::new("opencode").expect("alias");
        let cached = catalog(&["cached-model"]);
        service
            .save_model_cache(&alias, &cached)
            .await
            .expect("cache save");

        assert_eq!(
            service
                .load_model_cache(&alias)
                .await
                .expect("cache load")
                .expect("cache exists")
                .models()[0]
                .id()
                .as_str(),
            "cached-model"
        );
        let failing: Arc<dyn ModelCatalogProvider> = Arc::new(ScriptedCatalog::Failure);
        service
            .discover_and_cache_with(&alias, failing, CancellationToken::new())
            .await
            .expect_err("refresh should fail");
        assert_eq!(
            service
                .load_model_cache(&alias)
                .await
                .expect("cache load")
                .expect("old cache remains")
                .models()[0]
                .id()
                .as_str(),
            "cached-model"
        );
    }

    #[tokio::test]
    async fn successful_refresh_replaces_cache_and_cancelled_refresh_does_not() {
        let (_temp, paths) = test_paths();
        let service = ProviderManagementService::new(paths).expect("service");
        let alias = ProviderAlias::new("opencode").expect("alias");
        service
            .save_model_cache(&alias, &catalog(&["old-model"]))
            .await
            .expect("old cache");
        let success: Arc<dyn ModelCatalogProvider> =
            Arc::new(ScriptedCatalog::Success(catalog(&["new-model"])));
        service
            .discover_and_cache_with(&alias, success, CancellationToken::new())
            .await
            .expect("refresh succeeds");
        assert_eq!(
            service
                .load_model_cache(&alias)
                .await
                .expect("cache load")
                .expect("cache exists")
                .models()[0]
                .id()
                .as_str(),
            "new-model"
        );

        let token = CancellationToken::new();
        token.cancel();
        let cancelled: Arc<dyn ModelCatalogProvider> = Arc::new(ScriptedCatalog::Cancelled);
        service
            .discover_and_cache_with(&alias, cancelled, token)
            .await
            .expect_err("cancelled refresh");
        assert_eq!(
            service
                .load_model_cache(&alias)
                .await
                .expect("cache load")
                .expect("cache remains")
                .models()[0]
                .id()
                .as_str(),
            "new-model"
        );
    }

    enum ScriptedCatalog {
        Success(ModelCatalog),
        Failure,
        Cancelled,
    }

    impl ModelCatalogProvider for ScriptedCatalog {
        fn list_models<'a>(&'a self, token: CancellationToken) -> ModelCatalogFuture<'a> {
            Box::pin(async move {
                if token.is_cancelled() || matches!(self, Self::Cancelled) {
                    return Err(ModelCatalogError::cancelled());
                }
                match self {
                    Self::Success(catalog) => Ok(catalog.clone()),
                    Self::Failure => Err(ModelCatalogError::new(
                        merry_llm::ModelCatalogErrorKind::Transport,
                        "fixture transport failure",
                    )),
                    Self::Cancelled => unreachable!(),
                }
            })
        }
    }

    fn catalog(models: &[&str]) -> ModelCatalog {
        ModelCatalog::new(
            models
                .iter()
                .map(|model| {
                    ModelCatalogEntry::new(
                        ModelName::new(model).expect("valid model"),
                        Some("fixture"),
                    )
                    .expect("catalog entry")
                })
                .collect(),
        )
    }

    fn test_paths() -> (tempfile::TempDir, XdgPaths) {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = XdgPaths::from_parts(
            temp.path().join("home"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        (temp, paths)
    }

    async fn secret_file_count(paths: &XdgPaths) -> usize {
        let mut entries = tokio::fs::read_dir(paths.managed_secrets_dir())
            .await
            .expect("secrets directory");
        let mut count = 0;
        while entries.next_entry().await.expect("secret entry").is_some() {
            count += 1;
        }
        count
    }
}
