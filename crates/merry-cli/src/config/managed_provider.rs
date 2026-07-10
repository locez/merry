use super::{
    ConfigError, XdgPaths,
    provider::{NamedProviderToml, validate_api_key_text, validate_provider_display_name},
};
use merry_llm::ModelName;
use merry_provider_anthropic::AnthropicProviderConfig;
use merry_provider_openai::{OpenAiProtocol, OpenAiProviderConfig};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

const MAX_PROVIDER_ALIAS_LEN: usize = 64;
const RESERVED_PROVIDER_ALIASES: [&str; 3] = ["default", "managed", "retry"];
const MANAGED_PROVIDER_VERSION: u32 = 1;
static MANAGED_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub(crate) struct ProviderAlias(String);

impl ProviderAlias {
    pub(crate) fn new(value: &str) -> Result<Self, ConfigError> {
        validate_provider_alias(value)?;
        Ok(Self(value.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderAlias {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProviderAlias {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(de::Error::custom)
    }
}

pub(crate) fn derive_provider_alias(
    display_name: &str,
    used: &BTreeSet<String>,
) -> Result<ProviderAlias, ConfigError> {
    let mut base = String::new();
    let mut separator_pending = false;

    for character in display_name.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if separator_pending && !base.is_empty() {
                base.push('-');
            }
            base.push(character.to_ascii_lowercase());
            separator_pending = false;
        } else if !base.is_empty() {
            separator_pending = true;
        }
    }

    if base.is_empty() {
        base.push_str("provider");
    } else if !base.starts_with(|character: char| character.is_ascii_lowercase()) {
        base.insert_str(0, "provider-");
    }
    if RESERVED_PROVIDER_ALIASES.contains(&base.as_str()) {
        return Err(invalid_alias(&base, "is reserved"));
    }

    let mut candidate = base.clone();
    let mut suffix = 2usize;
    while used.contains(&candidate) {
        candidate = format!("{base}-{suffix}");
        suffix = suffix.saturating_add(1);
    }
    ProviderAlias::new(&candidate)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedProvidersToml {
    #[serde(default = "managed_provider_version")]
    version: u32,
    #[serde(default)]
    providers: BTreeMap<String, NamedProviderToml>,
}

impl Default for ManagedProvidersToml {
    fn default() -> Self {
        Self {
            version: MANAGED_PROVIDER_VERSION,
            providers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedProviderKind {
    OpenAiCompatible,
    Anthropic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedProviderDefinition {
    alias: ProviderAlias,
    display_name: String,
    default_model: ModelName,
    kind: ManagedProviderKind,
    base_url: String,
    protocol: Option<OpenAiProtocol>,
}

impl ManagedProviderDefinition {
    pub(crate) fn new(
        alias: ProviderAlias,
        display_name: &str,
        default_model: ModelName,
        kind: ManagedProviderKind,
        protocol: Option<OpenAiProtocol>,
        base_url: &str,
    ) -> Result<Self, ConfigError> {
        validate_provider_display_name(display_name)?;
        let protocol = match (kind, protocol) {
            (ManagedProviderKind::OpenAiCompatible, Some(protocol)) => {
                OpenAiProviderConfig::new("managed-provider-validation-key")
                    .map_err(|error| ConfigError::Invalid(error.to_string()))?
                    .with_base_url(base_url)
                    .map_err(|error| ConfigError::Invalid(error.to_string()))?;
                Some(protocol)
            }
            (ManagedProviderKind::OpenAiCompatible, None) => {
                return Err(ConfigError::Invalid(
                    "OpenAI-compatible managed providers must select Responses or Chat Completions"
                        .to_owned(),
                ));
            }
            (ManagedProviderKind::Anthropic, None) => {
                AnthropicProviderConfig::new("managed-provider-validation-key")
                    .map_err(|error| ConfigError::Invalid(error.to_string()))?
                    .with_base_url(base_url)
                    .map_err(|error| ConfigError::Invalid(error.to_string()))?;
                None
            }
            (ManagedProviderKind::Anthropic, Some(_)) => {
                return Err(ConfigError::Invalid(
                    "Anthropic managed providers use the Messages protocol".to_owned(),
                ));
            }
        };
        Ok(Self {
            alias,
            display_name: display_name.to_owned(),
            default_model,
            kind,
            base_url: base_url.to_owned(),
            protocol,
        })
    }

    fn into_toml(self, api_key_file: String) -> NamedProviderToml {
        NamedProviderToml {
            display_name: Some(self.display_name),
            default_model: Some(self.default_model.as_str().to_owned()),
            kind: Some(
                match self.kind {
                    ManagedProviderKind::OpenAiCompatible => "openai-compatible",
                    ManagedProviderKind::Anthropic => "anthropic",
                }
                .to_owned(),
            ),
            protocol: self.protocol,
            base_url: Some(self.base_url),
            api_version: None,
            default_max_output_tokens: None,
            api_key: None,
            api_key_file: Some(api_key_file),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedProviderStore {
    providers_file: PathBuf,
    secrets_dir: PathBuf,
    #[cfg(test)]
    fail_registry_replace: bool,
}

impl ManagedProviderStore {
    pub(crate) fn new(paths: &XdgPaths) -> Self {
        Self {
            providers_file: paths.managed_providers_file(),
            secrets_dir: paths.managed_secrets_dir(),
            #[cfg(test)]
            fail_registry_replace: false,
        }
    }

    #[cfg(test)]
    fn with_registry_replace_failure_for_test(mut self) -> Self {
        self.fail_registry_replace = true;
        self
    }

    pub(crate) async fn upsert(
        &self,
        definition: ManagedProviderDefinition,
        api_key: &str,
    ) -> Result<(), ManagedProviderStoreError> {
        validate_api_key_text("api_key", api_key)?;
        ensure_private_dir(&self.secrets_dir).await?;
        let registry_dir = self.providers_file.parent().ok_or_else(|| {
            ConfigError::Invalid(format!(
                "managed provider path {} has no parent",
                self.providers_file.display()
            ))
        })?;
        ensure_private_dir(registry_dir).await?;

        let mut registry = self.load().await?;
        let alias = definition.alias.as_str().to_owned();
        let secret_name = unique_secret_name(&definition.alias);
        let secret_path = self.secrets_dir.join(&secret_name);
        write_new_private_file(&secret_path, api_key.as_bytes()).await?;
        registry.providers.insert(
            alias,
            definition.into_toml(format!("managed/secrets/{secret_name}")),
        );
        #[cfg(test)]
        if self.fail_registry_replace {
            let _ = tokio::fs::remove_file(&secret_path).await;
            return Err(
                ConfigError::Invalid("injected registry replacement failure".to_owned()).into(),
            );
        }
        let registry_text = toml::to_string_pretty(&registry)?;
        if let Err(error) =
            replace_private_file(&self.providers_file, registry_text.as_bytes()).await
        {
            let _ = tokio::fs::remove_file(&secret_path).await;
            return Err(error);
        }
        self.remove_unreferenced_secrets(&registry).await;
        Ok(())
    }

    pub(crate) async fn update(
        &self,
        original_alias: &ProviderAlias,
        definition: ManagedProviderDefinition,
        api_key: Option<&str>,
    ) -> Result<(), ManagedProviderStoreError> {
        if definition.alias != *original_alias {
            return Err(ConfigError::Invalid(
                "managed provider config alias is a stable ID and cannot be renamed".to_owned(),
            )
            .into());
        }
        if let Some(api_key) = api_key {
            validate_api_key_text("api_key", api_key)?;
        }

        let mut registry = self.load().await?;
        let existing = registry
            .providers
            .remove(original_alias.as_str())
            .ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "managed provider {:?} does not exist",
                    original_alias.as_str()
                ))
            })?;
        let mut new_secret_path = None;
        let api_key_file = if let Some(api_key) = api_key {
            ensure_private_dir(&self.secrets_dir).await?;
            let secret_name = unique_secret_name(original_alias);
            let secret_path = self.secrets_dir.join(&secret_name);
            write_new_private_file(&secret_path, api_key.as_bytes()).await?;
            new_secret_path = Some(secret_path);
            format!("managed/secrets/{secret_name}")
        } else {
            existing.api_key_file.ok_or_else(|| {
                ConfigError::Invalid(format!(
                    "managed provider {:?} has no retained API key file",
                    original_alias.as_str()
                ))
            })?
        };
        registry.providers.insert(
            original_alias.as_str().to_owned(),
            definition.into_toml(api_key_file),
        );
        #[cfg(test)]
        if self.fail_registry_replace {
            if let Some(path) = new_secret_path.as_ref() {
                let _ = tokio::fs::remove_file(path).await;
            }
            return Err(
                ConfigError::Invalid("injected registry replacement failure".to_owned()).into(),
            );
        }
        let registry_text = toml::to_string_pretty(&registry)?;
        if let Err(error) =
            replace_private_file(&self.providers_file, registry_text.as_bytes()).await
        {
            if let Some(path) = new_secret_path.as_ref() {
                let _ = tokio::fs::remove_file(path).await;
            }
            return Err(error);
        }
        self.remove_unreferenced_secrets(&registry).await;
        Ok(())
    }

    pub(crate) async fn delete(
        &self,
        alias: &ProviderAlias,
    ) -> Result<(), ManagedProviderStoreError> {
        let mut registry = self.load().await?;
        if registry.providers.remove(alias.as_str()).is_none() {
            return Err(ConfigError::Invalid(format!(
                "managed provider {:?} does not exist",
                alias.as_str()
            ))
            .into());
        }
        let registry_dir = self.providers_file.parent().ok_or_else(|| {
            ConfigError::Invalid(format!(
                "managed provider path {} has no parent",
                self.providers_file.display()
            ))
        })?;
        ensure_private_dir(registry_dir).await?;
        let registry_text = toml::to_string_pretty(&registry)?;
        replace_private_file(&self.providers_file, registry_text.as_bytes()).await?;
        self.remove_unreferenced_secrets(&registry).await;
        Ok(())
    }

    async fn load(&self) -> Result<ManagedProvidersToml, ManagedProviderStoreError> {
        match tokio::fs::read_to_string(&self.providers_file).await {
            Ok(text) => {
                let registry = toml::from_str::<ManagedProvidersToml>(&text).map_err(|source| {
                    ConfigError::Parse {
                        path: self.providers_file.clone(),
                        source,
                    }
                })?;
                if registry.version != MANAGED_PROVIDER_VERSION {
                    return Err(ConfigError::Invalid(format!(
                        "managed provider config version {} is unsupported; expected {MANAGED_PROVIDER_VERSION}",
                        registry.version
                    ))
                    .into());
                }
                Ok(registry)
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                Ok(ManagedProvidersToml::default())
            }
            Err(source) => Err(store_io("read", &self.providers_file, source)),
        }
    }

    async fn remove_unreferenced_secrets(&self, registry: &ManagedProvidersToml) {
        let referenced = registry
            .providers
            .values()
            .filter_map(|provider| provider.api_key_file.as_deref())
            .filter_map(|path| Path::new(path).file_name())
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        let Ok(mut entries) = tokio::fs::read_dir(&self.secrets_dir).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_type()
                .await
                .is_ok_and(|file_type| file_type.is_file())
                && !referenced.contains(&entry.file_name())
            {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ManagedProviderStoreError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("failed to {operation} managed provider file {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to serialize managed providers: {0}")]
    Serialize(#[from] toml::ser::Error),
}

async fn ensure_private_dir(path: &Path) -> Result<(), ManagedProviderStoreError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|source| store_io("create directory for", path, source))?;
    #[cfg(unix)]
    tokio::fs::set_permissions(path, unix_permissions(0o700))
        .await
        .map_err(|source| store_io("set permissions on", path, source))?;
    Ok(())
}

async fn write_new_private_file(
    path: &Path,
    content: &[u8],
) -> Result<(), ManagedProviderStoreError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .await
        .map_err(|source| store_io("create", path, source))?;
    file.write_all(content)
        .await
        .map_err(|source| store_io("write", path, source))?;
    file.write_all(b"\n")
        .await
        .map_err(|source| store_io("write", path, source))?;
    file.sync_all()
        .await
        .map_err(|source| store_io("sync", path, source))?;
    Ok(())
}

async fn replace_private_file(
    path: &Path,
    content: &[u8],
) -> Result<(), ManagedProviderStoreError> {
    let parent = path.parent().ok_or_else(|| {
        ConfigError::Invalid(format!(
            "managed provider path {} has no parent",
            path.display()
        ))
    })?;
    let temp_path = parent.join(format!(
        ".providers.toml.tmp-{}-{}",
        std::process::id(),
        MANAGED_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    write_new_private_file(&temp_path, content).await?;
    if let Err(source) = tokio::fs::rename(&temp_path, path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(store_io("replace", path, source));
    }
    Ok(())
}

fn unique_secret_name(alias: &ProviderAlias) -> String {
    format!(
        "{}-{}-{}.key",
        alias.as_str(),
        std::process::id(),
        MANAGED_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn store_io(operation: &'static str, path: &Path, source: io::Error) -> ManagedProviderStoreError {
    ManagedProviderStoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(unix)]
fn unix_permissions(mode: u32) -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    std::fs::Permissions::from_mode(mode)
}

pub(super) fn parse_managed_providers(
    text: &str,
    path: &Path,
) -> Result<BTreeMap<String, NamedProviderToml>, ConfigError> {
    let managed =
        toml::from_str::<ManagedProvidersToml>(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if managed.version != MANAGED_PROVIDER_VERSION {
        return Err(ConfigError::Invalid(format!(
            "managed provider config version {} is unsupported; expected {MANAGED_PROVIDER_VERSION}",
            managed.version
        )));
    }
    for alias in managed.providers.keys() {
        ProviderAlias::new(alias)?;
    }
    Ok(managed.providers)
}

fn managed_provider_version() -> u32 {
    MANAGED_PROVIDER_VERSION
}

fn validate_provider_alias(value: &str) -> Result<(), ConfigError> {
    if value.is_empty() {
        return Err(invalid_alias(value, "must not be empty"));
    }
    if value.len() > MAX_PROVIDER_ALIAS_LEN {
        return Err(invalid_alias(value, "must be at most 64 bytes"));
    }
    if RESERVED_PROVIDER_ALIASES.contains(&value) {
        return Err(invalid_alias(value, "is reserved"));
    }
    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase()) {
        return Err(invalid_alias(
            value,
            "must start with a lowercase ASCII letter",
        ));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid_alias(
            value,
            "must contain only lowercase ASCII letters, digits, or '-'",
        ));
    }
    if value.ends_with('-') || value.contains("--") {
        return Err(invalid_alias(
            value,
            "must not end with '-' or contain repeated '-'",
        ));
    }
    Ok(())
}

fn invalid_alias(value: &str, reason: &str) -> ConfigError {
    ConfigError::Invalid(format!("provider alias {value:?} {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MerryConfig, XdgPaths};
    use merry_llm::ModelName;

    #[tokio::test]
    async fn managed_store_keeps_duplicate_endpoints_under_different_aliases() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = XdgPaths::from_parts(
            temp.path().join("home"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        let store = ManagedProviderStore::new(&paths);
        let first = ManagedProviderDefinition::new(
            ProviderAlias::new("opencode-work").expect("valid alias"),
            "OpenCode Work",
            ModelName::new("deepseek-v4-pro").expect("valid model"),
            ManagedProviderKind::OpenAiCompatible,
            Some(OpenAiProtocol::ChatCompletions),
            "https://opencode.example.test/v1",
        )
        .expect("valid provider");
        let second = ManagedProviderDefinition::new(
            ProviderAlias::new("opencode-personal").expect("valid alias"),
            "OpenCode Personal",
            ModelName::new("claude-sonnet-test").expect("valid model"),
            ManagedProviderKind::OpenAiCompatible,
            Some(OpenAiProtocol::Responses),
            "https://opencode.example.test/v1",
        )
        .expect("valid provider");

        store.upsert(first, "sk-work").await.expect("first save");
        store
            .upsert(second, "sk-personal")
            .await
            .expect("second save");

        let registry = tokio::fs::read_to_string(paths.managed_providers_file())
            .await
            .expect("registry should exist");
        assert!(registry.contains("[providers.opencode-work]"));
        assert!(registry.contains("[providers.opencode-personal]"));
        assert!(!registry.contains("sk-work"));
        assert!(!registry.contains("sk-personal"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(paths.managed_config_dir())
                    .expect("managed directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(paths.managed_secrets_dir())
                    .expect("secrets directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(paths.managed_providers_file())
                    .expect("registry metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            let mut secrets =
                std::fs::read_dir(paths.managed_secrets_dir()).expect("secret entries");
            while let Some(entry) = secrets.next().transpose().expect("secret entry") {
                assert_eq!(
                    entry
                        .metadata()
                        .expect("secret metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }

        let config = crate::config::MerryConfig::load_optional(&paths)
            .expect("merged config should load")
            .expect("providers should exist");
        assert_eq!(
            config
                .provider_profile("opencode-work")
                .expect("work profile")
                .display_name(),
            "OpenCode Work"
        );
        assert_eq!(
            config
                .provider_profile("opencode-personal")
                .expect("personal profile")
                .display_name(),
            "OpenCode Personal"
        );
        assert_eq!(
            config
                .provider_profile("opencode-work")
                .expect("work profile")
                .protocol(),
            Some(OpenAiProtocol::ChatCompletions)
        );
        assert_eq!(
            config
                .provider_profile("opencode-personal")
                .expect("personal profile")
                .protocol(),
            Some(OpenAiProtocol::Responses)
        );
    }

    #[tokio::test]
    async fn failed_registry_replace_preserves_previous_registry_and_secret() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = XdgPaths::from_parts(
            temp.path().join("home"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        let store = ManagedProviderStore::new(&paths);
        let definition = || {
            ManagedProviderDefinition::new(
                ProviderAlias::new("opencode").expect("valid alias"),
                "OpenCode",
                ModelName::new("deepseek-v4-pro").expect("valid model"),
                ManagedProviderKind::OpenAiCompatible,
                Some(OpenAiProtocol::ChatCompletions),
                "https://opencode.example.test/v1",
            )
            .expect("valid provider")
        };
        store
            .upsert(definition(), "sk-original")
            .await
            .expect("initial save");
        let original_registry = tokio::fs::read(paths.managed_providers_file())
            .await
            .expect("original registry");
        let original_secrets = secret_file_names(&paths).await;

        let error = store
            .with_registry_replace_failure_for_test()
            .upsert(definition(), "sk-replacement")
            .await
            .expect_err("injected replacement should fail");

        assert!(
            error
                .to_string()
                .contains("injected registry replacement failure")
        );
        assert_eq!(
            tokio::fs::read(paths.managed_providers_file())
                .await
                .expect("registry remains"),
            original_registry
        );
        assert_eq!(secret_file_names(&paths).await, original_secrets);
    }

    #[tokio::test]
    async fn managed_store_updates_provider_without_rewriting_retained_secret() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = XdgPaths::from_parts(
            temp.path().join("home"),
            Some(temp.path().join("config")),
            Some(temp.path().join("state")),
        );
        let store = ManagedProviderStore::new(&paths);
        let alias = ProviderAlias::new("opencode").expect("valid alias");
        store
            .upsert(
                ManagedProviderDefinition::new(
                    alias.clone(),
                    "OpenCode",
                    ModelName::new("model-a").expect("valid model"),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::ChatCompletions),
                    "https://old.example.test/v1",
                )
                .expect("valid provider"),
                "sk-retained",
            )
            .await
            .expect("initial save");
        let original_secrets = secret_file_names(&paths).await;

        store
            .update(
                &alias,
                ManagedProviderDefinition::new(
                    alias.clone(),
                    "OpenCode Edited",
                    ModelName::new("model-b").expect("valid model"),
                    ManagedProviderKind::OpenAiCompatible,
                    Some(OpenAiProtocol::Responses),
                    "https://new.example.test/v1",
                )
                .expect("valid provider"),
                None,
            )
            .await
            .expect("provider update");

        assert_eq!(secret_file_names(&paths).await, original_secrets);
        let secret = tokio::fs::read_to_string(
            paths
                .managed_secrets_dir()
                .join(original_secrets.first().expect("secret file")),
        )
        .await
        .expect("secret contents");
        assert_eq!(secret.trim(), "sk-retained");
        let config = MerryConfig::load_optional(&paths)
            .expect("config load")
            .expect("config exists");
        let profile = config.provider_profile("opencode").expect("profile");
        assert_eq!(profile.display_name(), "OpenCode Edited");
        assert_eq!(profile.protocol(), Some(OpenAiProtocol::Responses));
        assert_eq!(
            profile.default_model().map(ModelName::as_str),
            Some("model-b")
        );
    }

    async fn secret_file_names(paths: &XdgPaths) -> BTreeSet<std::ffi::OsString> {
        let mut names = BTreeSet::new();
        let mut entries = tokio::fs::read_dir(paths.managed_secrets_dir())
            .await
            .expect("secrets dir");
        while let Some(entry) = entries.next_entry().await.expect("secret entry") {
            names.insert(entry.file_name());
        }
        names
    }
}
