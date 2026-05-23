use crate::config::{EffectiveLogSettings, LogFormat, LogLevel};
use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};
use thiserror::Error;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*};

#[derive(Debug, Error)]
pub enum ObservabilityError {
    #[error("failed to create log directory {path}: {source}")]
    CreateLogDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to open log file {path}: {source}")]
    OpenLogFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to install tracing subscriber: {0}")]
    InstallSubscriber(String),
}

pub fn init_observability(
    settings: Option<&EffectiveLogSettings>,
) -> Result<Option<WorkerGuard>, ObservabilityError> {
    let Some(settings) = settings else {
        return Ok(None);
    };

    let file = open_log_file(&settings.path)?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = level_filter(settings.level);

    match settings.format {
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(fmt::layer().json().with_writer(writer).with_filter(filter))
                .try_init()
                .map_err(|error| ObservabilityError::InstallSubscriber(error.to_string()))?;
        }
        LogFormat::Text => {
            tracing_subscriber::registry()
                .with(fmt::layer().with_writer(writer).with_filter(filter))
                .try_init()
                .map_err(|error| ObservabilityError::InstallSubscriber(error.to_string()))?;
        }
    }

    Ok(Some(guard))
}

pub fn open_log_file(path: &Path) -> Result<File, ObservabilityError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ObservabilityError::CreateLogDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| ObservabilityError::OpenLogFile {
            path: path.to_path_buf(),
            source,
        })
}

pub fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EffectiveLogSettings, LogFormat, LogLevel};
    use std::{fs, path::PathBuf};

    fn settings(path: PathBuf) -> EffectiveLogSettings {
        EffectiveLogSettings {
            level: LogLevel::Info,
            format: LogFormat::Json,
            path,
        }
    }

    #[test]
    fn open_log_file_creates_parent_directory() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let log_path = temp.path().join("state/merry/logs/merry.jsonl");
        let file = open_log_file(&log_path).expect("log file should open");
        drop(file);
        assert!(log_path.exists());
    }

    #[test]
    fn open_log_file_reports_clear_error_when_parent_is_a_file() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let parent_file = temp.path().join("state");
        fs::write(&parent_file, "not a directory").expect("parent stand-in should write");
        let log_path = parent_file.join("merry.jsonl");
        let error = open_log_file(&log_path).expect_err("open should fail");
        assert!(error.to_string().contains("failed to create log directory"));
    }

    #[test]
    fn level_maps_to_tracing_filter() {
        assert_eq!(level_filter(LogLevel::Error).to_string(), "error");
        assert_eq!(level_filter(LogLevel::Warn).to_string(), "warn");
        assert_eq!(level_filter(LogLevel::Info).to_string(), "info");
        assert_eq!(level_filter(LogLevel::Debug).to_string(), "debug");
        assert_eq!(level_filter(LogLevel::Trace).to_string(), "trace");
    }

    #[test]
    fn init_disabled_returns_no_guard() {
        assert!(
            init_observability(None)
                .expect("disabled logging should succeed")
                .is_none()
        );
    }

    #[test]
    fn settings_debug_does_not_include_secret_material() {
        let config = settings(PathBuf::from(
            "/home/alice/.local/state/merry/logs/merry.jsonl",
        ));
        let debug = format!("{config:?}");
        assert!(debug.contains("merry.jsonl"));
        assert!(!debug.contains("OPENAI_API_KEY="));
        assert!(!debug.contains("sk-"));
    }
}
