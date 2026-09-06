use super::ConfigError;
use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdgPaths {
    home: PathBuf,
    config_base: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    state_base: PathBuf,
    state_dir: PathBuf,
    default_log_file: PathBuf,
}

impl XdgPaths {
    pub fn from_env() -> Result<Self, ConfigError> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(ConfigError::HomeMissingOrRelative)?;
        Ok(Self::from_parts(
            home,
            env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        ))
    }

    pub fn from_parts(
        home: PathBuf,
        xdg_config_home: Option<PathBuf>,
        xdg_state_home: Option<PathBuf>,
    ) -> Self {
        let home = normalize_path_lexically(&home);
        let config_base = absolute_or_default(xdg_config_home, home.join(".config"));
        let state_base = absolute_or_default(xdg_state_home, home.join(".local/state"));
        let config_dir = config_base.join("merry");
        let state_dir = state_base.join("merry");
        let config_file = config_dir.join("config.toml");
        let default_log_file = state_dir.join("logs/merry.jsonl");
        Self {
            home,
            config_base,
            config_dir,
            config_file,
            state_base,
            state_dir,
            default_log_file,
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn config_base_dir(&self) -> &Path {
        &self.config_base
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    pub fn state_base_dir(&self) -> &Path {
        &self.state_base
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
    if let Some(rest) = value.strip_prefix("~/") {
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
    } else if value.starts_with("~/") {
        resolve_user_path(value, config_dir, home)
    } else {
        Ok(config_dir.join(path))
    }
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
