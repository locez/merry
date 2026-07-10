use crate::config::ProviderAlias;
use merry_llm::{ModelName, ReasoningEffort};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

const PREFERENCES_VERSION: u32 = 2;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CodeTheme {
    #[default]
    Dracula,
    CatppuccinMocha,
    MonokaiBright,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CompactionStrategy {
    Compact,
    Balanced,
    PreserveDetail,
}

impl CompactionStrategy {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Balanced => "Balanced",
            Self::PreserveDetail => "Preserve detail",
        }
    }
}

impl CodeTheme {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Dracula => "Dracula",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::MonokaiBright => "Monokai Bright",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Dracula => Self::CatppuccinMocha,
            Self::CatppuccinMocha => Self::MonokaiBright,
            Self::MonokaiBright => Self::Dracula,
        }
    }

    pub(crate) fn previous(self) -> Self {
        match self {
            Self::Dracula => Self::MonokaiBright,
            Self::CatppuccinMocha => Self::Dracula,
            Self::MonokaiBright => Self::CatppuccinMocha,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TuiPreferences {
    #[serde(default = "preferences_version")]
    version: u32,
    #[serde(default)]
    pub(crate) code_theme: CodeTheme,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    models: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) auto_compaction_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) compaction_strategy: Option<CompactionStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subagents_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) subagent_max_threads: Option<usize>,
}

impl Default for TuiPreferences {
    fn default() -> Self {
        Self {
            version: PREFERENCES_VERSION,
            code_theme: CodeTheme::default(),
            provider: None,
            models: BTreeMap::new(),
            reasoning_effort: None,
            auto_compaction_enabled: None,
            compaction_strategy: None,
            subagents_enabled: None,
            subagent_max_threads: None,
        }
    }
}

impl TuiPreferences {
    pub(crate) fn reasoning_label(&self) -> Option<&str> {
        self.reasoning_effort.as_ref().map(ReasoningEffort::as_str)
    }

    pub(crate) fn model_for_provider(&self, provider: &str) -> Option<&str> {
        self.models.get(provider).map(String::as_str)
    }

    pub(crate) fn set_model_for_provider(
        &mut self,
        provider: &str,
        model: Option<&str>,
    ) -> Result<(), PreferencesError> {
        ProviderAlias::new(provider)
            .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        match model {
            Some(model) => {
                let model = ModelName::new(model)
                    .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
                self.models
                    .insert(provider.to_owned(), model.as_str().to_owned());
            }
            None => {
                self.models.remove(provider);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiSettingsDefaults {
    pub(crate) provider_aliases: Vec<String>,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) subagents_enabled: bool,
    pub(crate) subagent_max_threads: usize,
    pub(crate) auto_compaction_enabled: bool,
    pub(crate) compaction_strategy: String,
}

impl Default for TuiSettingsDefaults {
    fn default() -> Self {
        let limits = merry_runtime::SubagentConfig::default();
        Self {
            provider_aliases: Vec::new(),
            provider: None,
            model: None,
            reasoning_effort: None,
            subagents_enabled: false,
            subagent_max_threads: limits.max_threads(),
            auto_compaction_enabled: true,
            compaction_strategy: "Balanced".to_owned(),
        }
    }
}

impl TuiSettingsDefaults {
    pub(crate) fn from_config(
        config: Option<&crate::config::MerryConfig>,
    ) -> Result<Self, crate::config::ConfigError> {
        let Some(config) = config else {
            return Ok(Self::default());
        };
        let default_provider = config.configured_default_provider()?;
        let subagents = config.subagents_config()?;
        let auto_compaction = config.automatic_compaction_config()?;
        Ok(Self {
            provider_aliases: config.provider_aliases(),
            provider: default_provider
                .as_ref()
                .map(|provider| provider.alias.clone()),
            model: default_provider
                .as_ref()
                .map(|provider| provider.model.clone()),
            reasoning_effort: default_provider.and_then(|provider| provider.reasoning_effort),
            subagents_enabled: subagents.is_enabled(),
            subagent_max_threads: subagents.limits().max_threads(),
            auto_compaction_enabled: auto_compaction.is_enabled(),
            compaction_strategy: "Config".to_owned(),
        })
    }
}

fn preferences_version() -> u32 {
    PREFERENCES_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiPreferencesStore {
    path: PathBuf,
}

impl TuiPreferencesStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) async fn load(&self) -> Result<TuiPreferences, PreferencesError> {
        self.load_with_default_provider(None).await
    }

    pub(crate) async fn load_with_default_provider(
        &self,
        default_provider: Option<&str>,
    ) -> Result<TuiPreferences, PreferencesError> {
        let text = match tokio::fs::read_to_string(&self.path).await {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(TuiPreferences::default());
            }
            Err(source) => return Err(io_error("read", &self.path, source)),
        };
        let version = toml::from_str::<PreferencesVersion>(&text)
            .map_err(|source| PreferencesError::Parse {
                path: self.path.clone(),
                source,
            })?
            .version;
        let preferences = match version {
            1 => toml::from_str::<TuiPreferencesV1>(&text)
                .map_err(|source| PreferencesError::Parse {
                    path: self.path.clone(),
                    source,
                })?
                .migrate(default_provider)?,
            PREFERENCES_VERSION => toml::from_str::<TuiPreferences>(&text).map_err(|source| {
                PreferencesError::Parse {
                    path: self.path.clone(),
                    source,
                }
            })?,
            path_version => {
                return Err(PreferencesError::UnsupportedVersion {
                    path_version,
                    supported_version: PREFERENCES_VERSION,
                });
            }
        };
        preferences.validate()?;
        Ok(preferences)
    }

    pub(crate) async fn save(&self, preferences: &TuiPreferences) -> Result<(), PreferencesError> {
        preferences.validate()?;
        let text = toml::to_string_pretty(preferences).map_err(PreferencesError::Serialize)?;
        let parent = self.path.parent().ok_or_else(|| {
            PreferencesError::Invalid(format!(
                "preferences path {} has no parent directory",
                self.path.display()
            ))
        })?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| io_error("create parent directory for", parent, source))?;
        let temp_path = parent.join(format!(
            ".tui-preferences.toml.tmp-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|source| io_error("create temporary", &temp_path, source))?;
        file.write_all(text.as_bytes())
            .await
            .map_err(|source| io_error("write temporary", &temp_path, source))?;
        file.sync_all()
            .await
            .map_err(|source| io_error("sync temporary", &temp_path, source))?;
        drop(file);
        if let Err(source) = tokio::fs::rename(&temp_path, &self.path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(io_error("replace", &self.path, source));
        }
        Ok(())
    }
}

impl TuiPreferences {
    fn validate(&self) -> Result<(), PreferencesError> {
        if self.version != PREFERENCES_VERSION {
            return Err(PreferencesError::UnsupportedVersion {
                path_version: self.version,
                supported_version: PREFERENCES_VERSION,
            });
        }
        if let Some(provider) = self.provider.as_deref() {
            ProviderAlias::new(provider)
                .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        }
        for (provider, model) in &self.models {
            ProviderAlias::new(provider)
                .map_err(|error| PreferencesError::Invalid(error.to_string()))?;
            ModelName::new(model).map_err(|error| PreferencesError::Invalid(error.to_string()))?;
        }
        if self.subagent_max_threads == Some(0) {
            return Err(PreferencesError::Invalid(
                "subagent_max_threads must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct PreferencesVersion {
    #[serde(default = "legacy_preferences_version")]
    version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TuiPreferencesV1 {
    #[serde(default = "legacy_preferences_version")]
    version: u32,
    #[serde(default)]
    code_theme: CodeTheme,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    subagents_enabled: Option<bool>,
    #[serde(default)]
    subagent_max_threads: Option<usize>,
}

impl TuiPreferencesV1 {
    fn migrate(self, default_provider: Option<&str>) -> Result<TuiPreferences, PreferencesError> {
        if self.version != 1 {
            return Err(PreferencesError::UnsupportedVersion {
                path_version: self.version,
                supported_version: PREFERENCES_VERSION,
            });
        }
        let mut preferences = TuiPreferences {
            version: PREFERENCES_VERSION,
            code_theme: self.code_theme,
            provider: self.provider,
            models: BTreeMap::new(),
            reasoning_effort: self.reasoning_effort,
            auto_compaction_enabled: None,
            compaction_strategy: None,
            subagents_enabled: self.subagents_enabled,
            subagent_max_threads: self.subagent_max_threads,
        };
        if let Some(model) = self.model.as_deref() {
            let provider = preferences
                .provider
                .as_deref()
                .or(default_provider)
                .ok_or_else(|| {
                    PreferencesError::Invalid(
                        "version 1 model preference cannot migrate without a provider".to_owned(),
                    )
                })?
                .to_owned();
            preferences.set_model_for_provider(&provider, Some(model))?;
        }
        Ok(preferences)
    }
}

fn legacy_preferences_version() -> u32 {
    1
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> PreferencesError {
    PreferencesError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Error)]
pub(crate) enum PreferencesError {
    #[error("failed to {operation} TUI preferences at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse TUI preferences at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to serialize TUI preferences: {0}")]
    Serialize(#[source] toml::ser::Error),
    #[error(
        "unsupported TUI preferences version {path_version}; this Merry supports version {supported_version}"
    )]
    UnsupportedVersion {
        path_version: u32,
        supported_version: u32,
    },
    #[error("invalid TUI preferences: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preferences_store_round_trips_safe_tui_defaults() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let store = TuiPreferencesStore::new(temp.path().join("merry/tui-preferences.toml"));
        let preferences = TuiPreferences {
            code_theme: CodeTheme::CatppuccinMocha,
            provider: Some("anthropic".to_owned()),
            reasoning_effort: Some(ReasoningEffort::new("high").unwrap()),
            subagents_enabled: Some(true),
            subagent_max_threads: Some(6),
            ..TuiPreferences::default()
        };
        let mut preferences = preferences;
        preferences
            .set_model_for_provider("anthropic", Some("claude-test"))
            .expect("valid model preference");

        store.save(&preferences).await.expect("preferences save");
        let loaded = store.load().await.expect("preferences load");

        assert_eq!(loaded, preferences);
        assert!(store.path().is_file());
    }

    #[tokio::test]
    async fn migrates_v1_model_into_the_selected_provider_history() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let path = temp.path().join("merry/tui-preferences.toml");
        tokio::fs::create_dir_all(path.parent().expect("preferences parent"))
            .await
            .expect("preferences parent");
        tokio::fs::write(
            &path,
            r#"
version = 1
provider = "opencode"
model = "deepseek-v4-pro"
"#,
        )
        .await
        .expect("v1 preferences");
        let store = TuiPreferencesStore::new(path);

        let migrated = store
            .load_with_default_provider(Some("anthropic"))
            .await
            .expect("v1 preferences should migrate");

        assert_eq!(migrated.provider.as_deref(), Some("opencode"));
        assert_eq!(
            migrated.model_for_provider("opencode"),
            Some("deepseek-v4-pro")
        );
        assert_eq!(migrated.model_for_provider("anthropic"), None);
    }

    #[tokio::test]
    async fn provider_model_histories_are_independent_even_for_duplicate_endpoints() {
        let mut preferences = TuiPreferences::default();
        preferences
            .set_model_for_provider("opencode-work", Some("model-work"))
            .expect("work model");
        preferences
            .set_model_for_provider("opencode-personal", Some("model-personal"))
            .expect("personal model");

        assert_eq!(
            preferences.model_for_provider("opencode-work"),
            Some("model-work")
        );
        assert_eq!(
            preferences.model_for_provider("opencode-personal"),
            Some("model-personal")
        );
    }
}
