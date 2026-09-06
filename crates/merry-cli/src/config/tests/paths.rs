use super::home;
use crate::config::XdgPaths;
use std::path::{Path, PathBuf};

#[test]
fn xdg_paths_use_defaults_when_env_is_missing_empty_or_relative() {
    let paths = XdgPaths::from_parts(home(), None, None);
    assert_eq!(paths.config_dir(), Path::new("/home/alice/.config/merry"));
    assert_eq!(
        paths.config_file(),
        Path::new("/home/alice/.config/merry/config.toml")
    );
    assert_eq!(
        paths.state_dir(),
        Path::new("/home/alice/.local/state/merry")
    );
    assert_eq!(
        paths.default_log_file(),
        Path::new("/home/alice/.local/state/merry/logs/merry.jsonl")
    );

    let paths = XdgPaths::from_parts(home(), Some(PathBuf::new()), Some(PathBuf::from("state")));
    assert_eq!(paths.config_dir(), Path::new("/home/alice/.config/merry"));
    assert_eq!(
        paths.state_dir(),
        Path::new("/home/alice/.local/state/merry")
    );
}

#[test]
fn xdg_paths_use_absolute_env_values() {
    let paths = XdgPaths::from_parts(
        home(),
        Some(PathBuf::from("/tmp/config")),
        Some(PathBuf::from("/tmp/state")),
    );
    assert_eq!(paths.config_dir(), Path::new("/tmp/config/merry"));
    assert_eq!(
        paths.config_file(),
        Path::new("/tmp/config/merry/config.toml")
    );
    assert_eq!(paths.state_dir(), Path::new("/tmp/state/merry"));
    assert_eq!(
        paths.default_log_file(),
        Path::new("/tmp/state/merry/logs/merry.jsonl")
    );
}

#[test]
fn xdg_paths_normalize_absolute_environment_values() {
    let paths = XdgPaths::from_parts(
        PathBuf::from("/home/alice/../alice"),
        Some(PathBuf::from("/tmp/config/../config")),
        Some(PathBuf::from("/tmp/state/./nested/..")),
    );

    assert_eq!(paths.home(), Path::new("/home/alice"));
    assert_eq!(paths.config_base_dir(), Path::new("/tmp/config"));
    assert_eq!(paths.state_base_dir(), Path::new("/tmp/state"));
    assert_eq!(
        paths.managed_config_dir(),
        Path::new("/tmp/config/merry/managed")
    );
}
