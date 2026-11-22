use super::ConfigError;
use merry_core::host_paths::{
    platform_config_base, platform_data_base, platform_state_base, strip_home_tilde_prefix,
    user_home_dir,
};
use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdgPaths {
    home: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    state_dir: PathBuf,
    data_dir: PathBuf,
    default_log_file: PathBuf,
}

impl XdgPaths {
    pub fn from_env() -> Result<Self, ConfigError> {
        let home = user_home_dir().ok_or(ConfigError::HomeMissingOrRelative)?;
        let xdg_config_home = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let xdg_state_home = env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        let xdg_data_home = env::var_os("XDG_DATA_HOME").map(PathBuf::from);
        let appdata = env::var_os("APPDATA").map(PathBuf::from);
        let local_appdata = env::var_os("LOCALAPPDATA").map(PathBuf::from);
        Ok(Self::from_env_parts(
            home,
            xdg_config_home.as_deref(),
            xdg_state_home.as_deref(),
            xdg_data_home.as_deref(),
            appdata.as_deref(),
            local_appdata.as_deref(),
        ))
    }

    #[cfg(test)]
    pub fn from_parts(
        home: PathBuf,
        xdg_config_home: Option<PathBuf>,
        xdg_state_home: Option<PathBuf>,
    ) -> Self {
        let home = normalize_path_lexically(&home);
        let config_base = absolute_or_default(xdg_config_home, home.join(".config"));
        let state_base = absolute_or_default(xdg_state_home, home.join(".local/state"));
        let data_base = normalize_path_lexically(&home.join(".local/share"));
        Self::from_resolved_bases(home, config_base, state_base, data_base)
    }

    #[cfg(test)]
    pub fn with_data_home(mut self, xdg_data_home: PathBuf) -> Self {
        let data_base = absolute_or_default(Some(xdg_data_home), self.home.join(".local/share"));
        self.data_dir = data_base.join("merry");
        self
    }

    fn from_env_parts(
        home: PathBuf,
        xdg_config_home: Option<&Path>,
        xdg_state_home: Option<&Path>,
        xdg_data_home: Option<&Path>,
        appdata: Option<&Path>,
        local_appdata: Option<&Path>,
    ) -> Self {
        let home = normalize_path_lexically(&home);
        let config_base =
            normalize_path_lexically(&platform_config_base(&home, xdg_config_home, appdata));
        let state_base =
            normalize_path_lexically(&platform_state_base(&home, xdg_state_home, local_appdata));
        let data_base =
            normalize_path_lexically(&platform_data_base(&home, xdg_data_home, local_appdata));
        Self::from_resolved_bases(home, config_base, state_base, data_base)
    }

    fn from_resolved_bases(
        home: PathBuf,
        config_base: PathBuf,
        state_base: PathBuf,
        data_base: PathBuf,
    ) -> Self {
        let config_dir = config_base.join("merry");
        let state_dir = state_base.join("merry");
        let data_dir = data_base.join("merry");
        let config_file = config_dir.join("config.toml");
        let default_log_file = state_dir.join("logs").join("merry.jsonl");
        Self {
            home,
            config_dir,
            config_file,
            state_dir,
            data_dir,
            default_log_file,
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn default_log_file(&self) -> &Path {
        &self.default_log_file
    }

    pub fn tui_preferences_file(&self) -> PathBuf {
        self.state_dir.join("tui-preferences.toml")
    }

    pub fn managed_config_dir(&self) -> PathBuf {
        self.data_dir.join("providers")
    }

    /// Previous location of the managed provider registry before it moved out
    /// of writable `$XDG_CONFIG_HOME/merry`.
    pub(super) fn legacy_managed_config_dir(&self) -> PathBuf {
        self.config_dir.join("managed")
    }

    pub fn managed_providers_file(&self) -> PathBuf {
        self.managed_config_dir().join("providers.toml")
    }

    pub fn managed_secrets_dir(&self) -> PathBuf {
        self.managed_config_dir().join("secrets")
    }

    pub fn model_catalog_cache_dir(&self) -> PathBuf {
        self.state_dir.join("model-catalogs")
    }
}

#[cfg(test)]
fn absolute_or_default(value: Option<PathBuf>, default: PathBuf) -> PathBuf {
    let path = match value {
        Some(path) if path.is_absolute() && !path.as_os_str().is_empty() => path,
        _ => default,
    };
    normalize_path_lexically(&path)
}

pub(super) fn resolve_user_path(
    value: &str,
    config_dir: &Path,
    home: &Path,
) -> Result<PathBuf, ConfigError> {
    if let Some(rest) = strip_home_tilde_prefix(value) {
        return Ok(home.join(rest));
    }
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }
    Err(ConfigError::Invalid(format!(
        "log path must be absolute or start with ~/; relative path {value:?} was configured under {}",
        config_dir.display()
    )))
}

pub(super) fn resolve_config_relative_path(
    value: &str,
    config_dir: &Path,
    home: &Path,
) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else if strip_home_tilde_prefix(value).is_some() {
        resolve_user_path(value, config_dir, home)
    } else {
        Ok(config_dir.join(path))
    }
}

/// Resolves a provider `api_key_file` relative to `$XDG_DATA_HOME/merry/providers`.
///
/// Compatibility shim for registries relocated from `$XDG_CONFIG_HOME/merry/managed`,
/// whose `providers.toml` stored `managed/secrets/<file>` relative to `config_dir`.
/// TODO: Remove the `managed/secrets/` prefix compatibility once pre-relocation
/// managed registries are no longer supported.
pub(super) fn resolve_provider_api_key_file_path(
    value: &str,
    providers_dir: &Path,
    home: &Path,
) -> Result<PathBuf, ConfigError> {
    let normalized = value
        .strip_prefix("managed/")
        .filter(|rest| rest.starts_with("secrets/"))
        .unwrap_or(value);
    resolve_config_relative_path(normalized, providers_dir, home)
}

pub(super) fn resolve_path_access_rule_path(
    value: &str,
    config_dir: &Path,
    home: &Path,
) -> Result<PathBuf, ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Invalid(
            "permissions path entries must not be blank".to_owned(),
        ));
    }
    let path = resolve_config_relative_path(value, config_dir, home)?;
    Ok(normalize_path_lexically(&path))
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let is_absolute = path.is_absolute();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() && !is_absolute {
                    normalized.push("..");
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}
