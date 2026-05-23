# Config-Backed Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the existing deterministic and live coding-loop smokes diagnosable through XDG/TOML configuration and structured logs without adding a new interactive CLI.

**Architecture:** The CLI owns config discovery, sandbox mount planning, and tracing subscriber setup. Runtime, workspace-tool, process-runner, and provider crates emit provider-neutral `tracing` fields at action boundaries; they do not own log files or terminal formatting. The sandbox planner resolves host XDG paths before bwrap re-exec, mounts config read-only, and mounts only the configured/default log directory read-write when logging is enabled.

**Tech Stack:** Rust 2024, `serde`, `toml`, `tracing`, `tracing-subscriber`, `tracing-appender`, Tokio, existing `bwrap` bootstrap, deterministic fake provider/fake runner tests.

---

## File Structure

- Modify `Cargo.toml`: add workspace dependencies for `toml`, `tracing-subscriber`, `tracing-appender`, and `tempfile`.
- Modify `crates/merry-cli/Cargo.toml`: add CLI dependencies on `serde`, `thiserror`, `toml`, `tracing`, `tracing-subscriber`, `tracing-appender`; add `tempfile` as a dev dependency.
- Create `crates/merry-cli/src/config.rs`: XDG path resolution, TOML config parsing, effective log settings, provider/model settings, and redacted diagnostics.
- Create `crates/merry-cli/src/observability.rs`: config-backed tracing subscriber setup, log-file open/create behavior, and testable log writer helpers.
- Modify `crates/merry-cli/src/main.rs`: load config during startup, initialize observability after sandbox re-exec, mount XDG config/state paths in bwrap, and migrate live smoke provider config to XDG TOML.
- Modify `crates/merry-runtime/Cargo.toml`: add `tracing-subscriber` as a dev dependency for capture tests.
- Modify `crates/merry-runtime/src/agent_loop.rs`: emit `runtime.loop.*`, `runtime.step.*`, and loop-owned tool execution trace records.
- Modify `crates/merry-runtime/src/runtime.rs`: align existing runtime/provider/tool/process traces with stable event names and correlation fields.
- Modify `crates/merry-tool-workspace/Cargo.toml`: add `tracing` and `tracing-subscriber` test capture dependency.
- Modify `crates/merry-tool-workspace/src/lib.rs`: emit workspace tool start/finish records with bounded path/query summaries and artifact/action status.
- Modify `crates/merry-provider-openai/src/provider.rs`: align provider trace field names and ensure raw provider payloads/API keys remain absent.
- Modify `crates/merry-cli/tests/debug.rs`: add CLI-level config/log smoke coverage that does not require bwrap, network, or live credentials.

## Acceptance Commands

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test -p merry-cli`
- `cargo test -p merry-runtime agent_loop`
- `cargo test -p merry-runtime tracing`
- `cargo test -p merry-tool-workspace`
- `cargo test --all`

The opt-in manual checks remain non-default:

```bash
cargo test -p merry-cli debug_coding_loop_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored
MERRY_OPENAI_DEBUG=1 cargo test -p merry-cli debug_coding_loop_live_smoke_runs_inside_real_bwrap_when_opted_in -- --ignored
```

## Task 1: XDG TOML Config Model

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/merry-cli/Cargo.toml`
- Create: `crates/merry-cli/src/config.rs`
- Modify: `crates/merry-cli/src/main.rs`

- [ ] **Step 1: Add dependencies**

Edit the workspace dependencies:

```toml
[workspace.dependencies]
toml = "0.8"
tracing-appender = "0.2"
tracing-subscriber = { version = "0.3", features = ["fmt", "json", "env-filter"] }
tempfile = "3"
```

Edit `crates/merry-cli/Cargo.toml`:

```toml
[dependencies]
serde.workspace = true
thiserror.workspace = true
toml.workspace = true
tracing.workspace = true
tracing-appender.workspace = true
tracing-subscriber.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 2: Declare the module**

At the top of `crates/merry-cli/src/main.rs`, below the module doc comment, add:

```rust
mod config;
mod observability;
```

Run:

```bash
cargo test -p merry-cli config::tests -- --nocapture
```

Expected: it fails because `crates/merry-cli/src/config.rs` does not exist yet.

- [ ] **Step 3: Write config tests**

Create `crates/merry-cli/src/config.rs` with the tests first. The implementation in the next step must make these pass.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn home() -> PathBuf {
        PathBuf::from("/home/alice")
    }

    #[test]
    fn xdg_paths_use_defaults_when_env_is_missing_empty_or_relative() {
        let paths = XdgPaths::from_parts(home(), None, None);
        assert_eq!(paths.config_dir(), Path::new("/home/alice/.config/merry"));
        assert_eq!(paths.config_file(), Path::new("/home/alice/.config/merry/config.toml"));
        assert_eq!(paths.state_dir(), Path::new("/home/alice/.local/state/merry"));
        assert_eq!(
            paths.default_log_file(),
            Path::new("/home/alice/.local/state/merry/logs/merry.jsonl")
        );

        let paths = XdgPaths::from_parts(home(), Some(PathBuf::new()), Some(PathBuf::from("state")));
        assert_eq!(paths.config_dir(), Path::new("/home/alice/.config/merry"));
        assert_eq!(paths.state_dir(), Path::new("/home/alice/.local/state/merry"));
    }

    #[test]
    fn xdg_paths_use_absolute_env_values() {
        let paths = XdgPaths::from_parts(
            home(),
            Some(PathBuf::from("/tmp/config")),
            Some(PathBuf::from("/tmp/state")),
        );
        assert_eq!(paths.config_dir(), Path::new("/tmp/config/merry"));
        assert_eq!(paths.config_file(), Path::new("/tmp/config/merry/config.toml"));
        assert_eq!(paths.state_dir(), Path::new("/tmp/state/merry"));
        assert_eq!(paths.default_log_file(), Path::new("/tmp/state/merry/logs/merry.jsonl"));
    }

    #[test]
    fn missing_config_is_allowed_for_commands_without_provider_requirement() {
        let loaded = MerryConfig::load_optional_from_text(None, &XdgPaths::from_parts(home(), None, None))
            .expect("missing config should not fail optional load");
        assert!(loaded.is_none());
    }

    #[test]
    fn parses_observability_and_provider_config() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[global]
profile = "default"

[observability.log]
enabled = true
level = "debug"
format = "json"

[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
api_key_env = "OPENAI_API_KEY"
api_key_file = "secrets/openai.key"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        let log = config.effective_log_settings(&paths).expect("log settings should validate");
        assert_eq!(log.level, LogLevel::Debug);
        assert_eq!(log.format, LogFormat::Json);
        assert_eq!(log.path, paths.default_log_file());

        let provider = config.openai_compatible_provider().expect("provider should validate");
        assert_eq!(provider.model.as_deref(), Some("gpt-4.1-mini"));
        assert_eq!(provider.base_url.as_deref(), Some("https://api.example.test/v1"));
        assert_eq!(provider.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(
            provider.api_key_file.as_deref(),
            Some(Path::new("/home/alice/.config/merry/secrets/openai.key"))
        );
    }

    #[test]
    fn disabled_logging_has_no_effective_log_settings() {
        let paths = XdgPaths::from_parts(home(), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[observability.log]
enabled = false
level = "debug"
format = "json"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should be present");

        assert!(config.effective_log_settings(&paths).expect("settings should validate").is_none());
    }

    #[test]
    fn rejects_invalid_log_level_format_and_relative_log_path() {
        let paths = XdgPaths::from_parts(home(), None, None);

        let invalid_level = MerryConfig::load_optional_from_text(
            Some("[observability.log]\nenabled = true\nlevel = \"verbose\"\nformat = \"json\"\n"),
            &paths,
        )
        .expect_err("invalid level should fail");
        assert!(invalid_level.to_string().contains("level"));

        let invalid_format = MerryConfig::load_optional_from_text(
            Some("[observability.log]\nenabled = true\nlevel = \"info\"\nformat = \"yaml\"\n"),
            &paths,
        )
        .expect_err("invalid format should fail");
        assert!(invalid_format.to_string().contains("format"));

        let relative_path = MerryConfig::load_optional_from_text(
            Some("[observability.log]\nenabled = true\nlevel = \"info\"\nformat = \"json\"\npath = \"logs/merry.jsonl\"\n"),
            &paths,
        )
        .expect("TOML should parse")
        .expect("config should be present")
        .effective_log_settings(&paths)
        .expect_err("relative log path should fail");
        assert!(relative_path.to_string().contains("absolute"));
    }

    #[test]
    fn redacted_provider_debug_does_not_include_api_key_file_contents_or_env_value() {
        let provider = EffectiveOpenAiProviderConfig {
            model: Some("gpt-test".to_owned()),
            base_url: Some("https://api.example.test/v1".to_owned()),
            api_key_env: Some("OPENAI_API_KEY".to_owned()),
            api_key_file: Some(PathBuf::from("/home/alice/.config/merry/secrets/openai.key")),
        };
        let debug = format!("{provider:?}");
        assert!(debug.contains("OPENAI_API_KEY"));
        assert!(debug.contains("openai.key"));
        assert!(!debug.contains("sk-"));
    }
}
```

- [ ] **Step 4: Implement `config.rs`**

Implement the module with these public types and behavior:

```rust
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("HOME must be set to an absolute path to resolve Merry config")]
    HomeMissingOrRelative,
    #[error("failed to read Merry config {path}: {source}")]
    Read { path: PathBuf, #[source] source: io::Error },
    #[error("failed to parse Merry config {path}: {source}")]
    Parse { path: PathBuf, #[source] source: toml::de::Error },
    #[error("Merry config is invalid: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XdgPaths {
    config_dir: PathBuf,
    config_file: PathBuf,
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
        let config_base = absolute_or_default(xdg_config_home, home.join(".config"));
        let state_base = absolute_or_default(xdg_state_home, home.join(".local/state"));
        let config_dir = config_base.join("merry");
        let state_dir = state_base.join("merry");
        let config_file = config_dir.join("config.toml");
        let default_log_file = state_dir.join("logs/merry.jsonl");
        Self {
            config_dir,
            config_file,
            state_dir,
            default_log_file,
        }
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
}

fn absolute_or_default(value: Option<PathBuf>, default: PathBuf) -> PathBuf {
    match value {
        Some(path) if path.is_absolute() && !path.as_os_str().is_empty() => path,
        _ => default,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerryConfig {
    raw: MerryConfigToml,
    config_dir: PathBuf,
}

impl MerryConfig {
    pub fn load_optional(paths: &XdgPaths) -> Result<Option<Self>, ConfigError> {
        match fs::read_to_string(paths.config_file()) {
            Ok(text) => Self::load_optional_from_text(Some(&text), paths),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ConfigError::Read {
                path: paths.config_file().to_path_buf(),
                source,
            }),
        }
    }

    pub fn load_optional_from_text(
        text: Option<&str>,
        paths: &XdgPaths,
    ) -> Result<Option<Self>, ConfigError> {
        let Some(text) = text else {
            return Ok(None);
        };
        let raw = toml::from_str::<MerryConfigToml>(text).map_err(|source| ConfigError::Parse {
            path: paths.config_file().to_path_buf(),
            source,
        })?;
        Ok(Some(Self {
            raw,
            config_dir: paths.config_dir().to_path_buf(),
        }))
    }

    pub fn effective_log_settings(
        &self,
        paths: &XdgPaths,
    ) -> Result<Option<EffectiveLogSettings>, ConfigError> {
        let Some(log) = self.raw.observability.as_ref().and_then(|value| value.log.as_ref()) else {
            return Ok(None);
        };
        if !log.enabled {
            return Ok(None);
        }
        let path = match log.path.as_deref() {
            None => paths.default_log_file().to_path_buf(),
            Some(path) => resolve_user_path(path, &self.config_dir)?,
        };
        Ok(Some(EffectiveLogSettings {
            level: log.level,
            format: log.format,
            path,
        }))
    }

    pub fn openai_compatible_provider(&self) -> Result<EffectiveOpenAiProviderConfig, ConfigError> {
        let providers = self
            .raw
            .providers
            .as_ref()
            .ok_or_else(|| ConfigError::Invalid("[providers.default] is required".to_owned()))?;
        let default = providers
            .default
            .as_ref()
            .ok_or_else(|| ConfigError::Invalid("[providers.default] is required".to_owned()))?;
        if default.provider != "openai-compatible" {
            return Err(ConfigError::Invalid(format!(
                "unsupported default provider {}",
                default.provider
            )));
        }
        let provider = providers
            .named
            .get("openai-compatible")
            .ok_or_else(|| ConfigError::Invalid("[providers.openai-compatible] is required".to_owned()))?;
        let api_key_file = match provider.api_key_file.as_deref() {
            Some(path) => Some(resolve_config_relative_path(path, &self.config_dir)?),
            None => None,
        };
        Ok(EffectiveOpenAiProviderConfig {
            model: Some(default.model.clone()),
            base_url: provider.base_url.clone(),
            api_key_env: provider.api_key_env.clone(),
            api_key_file,
        })
    }
}

fn resolve_user_path(value: &str, config_dir: &Path) -> Result<PathBuf, ConfigError> {
    if let Some(rest) = value.strip_prefix("~/") {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(ConfigError::HomeMissingOrRelative)?;
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

fn resolve_config_relative_path(value: &str, config_dir: &Path) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else if value.starts_with("~/") {
        resolve_user_path(value, config_dir)
    } else {
        Ok(config_dir.join(path))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLogSettings {
    pub level: LogLevel,
    pub format: LogFormat,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveOpenAiProviderConfig {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub api_key_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Json,
    Text,
}

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MerryConfigToml {
    #[serde(default)]
    global: GlobalToml,
    observability: Option<ObservabilityToml>,
    providers: Option<ProvidersToml>,
}

#[derive(Debug, Deserialize, Default, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GlobalToml {
    profile: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ObservabilityToml {
    log: Option<LogToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LogToml {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_log_level")]
    level: LogLevel,
    #[serde(default = "default_log_format")]
    format: LogFormat,
    path: Option<String>,
}

fn default_log_level() -> LogLevel {
    LogLevel::Info
}

fn default_log_format() -> LogFormat {
    LogFormat::Json
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ProvidersToml {
    default: Option<DefaultProviderToml>,
    #[serde(flatten)]
    named: BTreeMap<String, OpenAiCompatibleProviderToml>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DefaultProviderToml {
    provider: String,
    model: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OpenAiCompatibleProviderToml {
    base_url: Option<String>,
    api_key_env: Option<String>,
    api_key_file: Option<String>,
}
```

- [ ] **Step 5: Run focused config tests**

Run:

```bash
cargo test -p merry-cli config::tests -- --nocapture
```

Expected: all config tests pass.

- [ ] **Step 6: Commit config model**

```bash
git add Cargo.toml crates/merry-cli/Cargo.toml crates/merry-cli/src/main.rs crates/merry-cli/src/config.rs
git commit -m "feat(cli): add xdg toml config model"
```

## Task 2: Config-Backed Log Initialization

**Files:**
- Create: `crates/merry-cli/src/observability.rs`
- Modify: `crates/merry-cli/src/main.rs`

- [ ] **Step 1: Write observability tests**

Create `crates/merry-cli/src/observability.rs` with tests first:

```rust
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
        assert!(init_observability(None).expect("disabled logging should succeed").is_none());
    }

    #[test]
    fn settings_debug_does_not_include_secret_material() {
        let config = settings(PathBuf::from("/home/alice/.local/state/merry/logs/merry.jsonl"));
        let debug = format!("{config:?}");
        assert!(debug.contains("merry.jsonl"));
        assert!(!debug.contains("OPENAI_API_KEY="));
        assert!(!debug.contains("sk-"));
    }
}
```

Run:

```bash
cargo test -p merry-cli observability::tests -- --nocapture
```

Expected: tests fail because implementation is not present.

- [ ] **Step 2: Implement observability setup**

Implement `crates/merry-cli/src/observability.rs`:

```rust
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
    CreateLogDirectory { path: PathBuf, #[source] source: io::Error },
    #[error("failed to open log file {path}: {source}")]
    OpenLogFile { path: PathBuf, #[source] source: io::Error },
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
```

- [ ] **Step 3: Wire startup without changing command stdout**

In `main`, after `maybe_reexec_sandbox` and before building the Tokio runtime, load config and initialize logging:

```rust
let xdg_paths = match config::XdgPaths::from_env() {
    Ok(paths) => paths,
    Err(error) => return CliExit::Unexpected(error.to_string()),
};
let merry_config = match config::MerryConfig::load_optional(&xdg_paths) {
    Ok(config) => config,
    Err(error) => return CliExit::Unexpected(error.to_string()),
};
let log_settings = match merry_config
    .as_ref()
    .map(|config| config.effective_log_settings(&xdg_paths))
    .transpose()
{
    Ok(settings) => settings.flatten(),
    Err(error) => return CliExit::Unexpected(error.to_string()),
};
let _observability_guard = match observability::init_observability(log_settings.as_ref()) {
    Ok(guard) => guard,
    Err(error) => return CliExit::Unexpected(error.to_string()),
};
```

Pass `xdg_paths` and `merry_config` into `async_main` so later tasks can use provider config:

```rust
runtime.block_on(async_main(cli, xdg_paths, merry_config))
```

- [ ] **Step 4: Add CLI integration test for logs**

In `crates/merry-cli/tests/debug.rs`, add:

```rust
#[test]
fn debug_writes_configured_json_log_without_changing_stdout() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let config_dir = temp.path().join("config/merry");
    let state_dir = temp.path().join("state");
    std::fs::create_dir_all(&config_dir).expect("config dir should be created");
    let log_path = state_dir.join("merry/logs/merry.jsonl");
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[observability.log]\nenabled = true\nlevel = \"debug\"\nformat = \"json\"\npath = {:?}\n",
            log_path
        ),
    )
    .expect("config should write");

    let output = merry()
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_STATE_HOME", &state_dir)
        .arg("debug")
        .output()
        .expect("merry debug should run");

    assert!(output.status.success(), "debug should exit successfully");
    assert_debug_output(&output.stdout, "debug-session");
    let log = std::fs::read_to_string(&log_path).expect("log file should exist");
    assert!(log.contains("runtime.step"));
    assert!(log.contains("debug-session"));
}
```

- [ ] **Step 5: Run focused tests**

```bash
cargo test -p merry-cli observability::tests -- --nocapture
cargo test -p merry-cli debug_writes_configured_json_log_without_changing_stdout --test debug
```

Expected: all focused tests pass and stdout remains the existing JSONL event output for `merry debug`.

- [ ] **Step 6: Commit log initialization**

```bash
git add crates/merry-cli/src/observability.rs crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
git commit -m "feat(cli): initialize config-backed observability"
```

## Task 3: Sandbox Config And Log Mount Planning

**Files:**
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/src/config.rs`

- [ ] **Step 1: Add sandbox path constants**

In `main.rs`, add:

```rust
const SANDBOX_XDG_CONFIG_HOME: &str = "/home/merry/.config";
const SANDBOX_XDG_STATE_HOME: &str = "/home/merry/.local/state";
const SANDBOX_MERRY_CONFIG_DIR: &str = "/home/merry/.config/merry";
const SANDBOX_MERRY_LOG_DIR: &str = "/home/merry/.local/state/merry/logs";
```

- [ ] **Step 2: Extend sandbox host shape**

Add these fields to `SandboxHost`:

```rust
xdg_paths: config::XdgPaths,
log_settings: Option<config::EffectiveLogSettings>,
```

`SandboxHost::from_env` should resolve `XdgPaths`, load config if present, and compute `log_settings`. Missing config remains non-fatal. Invalid config returns `SandboxError::Config`.

Add the new error variant:

```rust
Config(config::ConfigError),
```

and render it as:

```rust
SandboxError::Config(error) => write!(formatter, "failed to load Merry config before sandbox bootstrap: {error}")
```

- [ ] **Step 3: Add mount plan tests**

In the `#[cfg(test)] mod tests` of `main.rs`, update `sandbox_host()` to include:

```rust
xdg_paths: config::XdgPaths::from_parts(
    PathBuf::from("/home/alice"),
    Some(PathBuf::from("/host/config")),
    Some(PathBuf::from("/host/state")),
),
log_settings: None,
```

Then add:

```rust
#[test]
fn sandbox_plan_mounts_merry_config_dir_read_only_and_sets_xdg_config_home() {
    let host = sandbox_host();
    let SandboxBootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &["--ro-bind-try", "/host/config/merry", SANDBOX_MERRY_CONFIG_DIR]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "XDG_CONFIG_HOME", SANDBOX_XDG_CONFIG_HOME]
    ));
}

#[test]
fn sandbox_plan_does_not_mount_log_dir_when_logging_is_disabled() {
    let host = sandbox_host();
    let SandboxBootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(!contains_sequence(
        &args,
        &["--bind", "/host/state/merry/logs", SANDBOX_MERRY_LOG_DIR]
    ));
}

#[test]
fn sandbox_plan_mounts_log_dir_read_write_when_file_logging_is_enabled() {
    let mut host = sandbox_host();
    host.log_settings = Some(config::EffectiveLogSettings {
        level: config::LogLevel::Info,
        format: config::LogFormat::Json,
        path: PathBuf::from("/host/state/merry/logs/merry.jsonl"),
    });
    let SandboxBootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert!(contains_sequence(
        &args,
        &["--bind", "/host/state/merry/logs", SANDBOX_MERRY_LOG_DIR]
    ));
    assert!(contains_sequence(
        &args,
        &["--setenv", "XDG_STATE_HOME", SANDBOX_XDG_STATE_HOME]
    ));
}
```

- [ ] **Step 4: Implement mount arguments**

In `build_sandbox_plan`, after `/home/merry` is created and before `--clearenv`, append:

```rust
args.extend([
    os("--ro-bind-try"),
    host.xdg_paths.config_dir().as_os_str().to_owned(),
    os(SANDBOX_MERRY_CONFIG_DIR),
]);

if let Some(log_settings) = host.log_settings.as_ref() {
    if let Some(host_log_dir) = log_settings.path.parent() {
        args.extend([
            os("--bind"),
            host_log_dir.as_os_str().to_owned(),
            os(SANDBOX_MERRY_LOG_DIR),
        ]);
    }
}
```

In the environment section, add:

```rust
os("--setenv"),
os("XDG_CONFIG_HOME"),
os(SANDBOX_XDG_CONFIG_HOME),
os("--setenv"),
os("XDG_STATE_HOME"),
os(SANDBOX_XDG_STATE_HOME),
```

- [ ] **Step 5: Ensure host log directory exists before re-exec**

Before returning `SandboxBootstrap::Reexec`, if `host.log_settings` is present, create `log_settings.path.parent()` on the host. Return `SandboxError::LogDirectory` on failure:

```rust
LogDirectory { path: PathBuf, source: io::Error },
```

Render it as:

```rust
SandboxError::LogDirectory { path, source } => {
    write!(formatter, "failed to create host log directory {} before sandbox bootstrap: {source}", path.display())
}
```

- [ ] **Step 6: Run sandbox planning tests**

```bash
cargo test -p merry-cli sandbox_plan_mounts_merry_config_dir_read_only_and_sets_xdg_config_home -- --nocapture
cargo test -p merry-cli sandbox_plan_mounts_log_dir_read_write_when_file_logging_is_enabled -- --nocapture
cargo test -p merry-cli sandbox_plan_does_not_mount_log_dir_when_logging_is_disabled -- --nocapture
```

Expected: all focused sandbox tests pass.

- [ ] **Step 7: Commit sandbox mounting**

```bash
git add crates/merry-cli/src/main.rs crates/merry-cli/src/config.rs
git commit -m "feat(cli): mount merry config and log paths in sandbox"
```

## Task 4: XDG Provider Config For OpenAI-Compatible Debug Paths

**Files:**
- Modify: `crates/merry-cli/src/config.rs`
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/tests/debug.rs`

- [ ] **Step 1: Remove live-smoke repo-local config flag**

Delete `CODING_LOOP_LIVE_SMOKE_CONFIG_PATH`, the `config_path` field from `DebugCodingLoopLiveSmokeArgs`, and the `--config` handling in `async_main`. The live command shape becomes:

```bash
merry --with-sandbox debug coding-loop-live-smoke --model gpt-4.1-mini
```

The `--model` flag remains as a per-run model override; provider, base URL, and credential source come from XDG TOML.

- [ ] **Step 2: Add credential resolution**

In `config.rs`, add:

```rust
impl EffectiveOpenAiProviderConfig {
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        if let Some(name) = self.api_key_env.as_deref() {
            match env::var(name) {
                Ok(value) if !value.trim().is_empty() => return Ok(value),
                Ok(_) => return Err(ConfigError::Invalid(format!("{name} must not be blank"))),
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(ConfigError::Invalid(format!("{name} must be valid UTF-8")));
                }
                Err(env::VarError::NotPresent) => {}
            }
        }

        if let Some(path) = self.api_key_file.as_deref() {
            let value = fs::read_to_string(path).map_err(|source| ConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            let value = value.trim().to_owned();
            if value.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "api_key_file {} must not be blank",
                    path.display()
                )));
            }
            return Ok(value);
        }

        Err(ConfigError::Invalid(
            "providers.openai-compatible must set api_key_env or api_key_file".to_owned(),
        ))
    }
}
```

This lets sandboxed live smokes use a secret file under the mounted Merry config directory without passing secrets through bwrap command arguments.

- [ ] **Step 3: Replace `LocalOpenAiConfig` with TOML-backed provider loading**

Change:

```rust
fn debug_openai_config(model_flag: Option<&str>) -> Result<DebugOpenAiConfig, CliError>
```

to:

```rust
fn debug_openai_config(
    model_flag: Option<&str>,
    merry_config: Option<&config::MerryConfig>,
) -> Result<DebugOpenAiConfig, CliError>
```

Have it delegate to a testable helper:

```rust
fn debug_openai_config_with_env(
    model_flag: Option<&str>,
    merry_config: Option<&config::MerryConfig>,
    env_value: impl Fn(&'static str) -> Result<Option<String>, CliError>,
) -> Result<DebugOpenAiConfig, CliError>
```

Use `MERRY_OPENAI_DEBUG=1` as the network opt-in through `env_value("MERRY_OPENAI_DEBUG")`. Then read the provider from `merry_config.openai_compatible_provider()`. The model selection order is:

1. `--model`
2. `[providers.default].model`
3. usage error

Build `OpenAiProviderConfig` from `resolve_api_key()`, apply `base_url`, and keep organization/project outside this slice because the approved TOML schema does not include those fields.

- [ ] **Step 4: Update live smoke function signature**

Change:

```rust
async fn run_debug_coding_loop_live_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    model_flag: Option<&str>,
    config_path: &Path,
    max_output_tokens: u64,
) -> Result<(), CliError>
```

to:

```rust
async fn run_debug_coding_loop_live_smoke(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    model_flag: Option<&str>,
    max_output_tokens: u64,
    merry_config: Option<&config::MerryConfig>,
) -> Result<(), CliError>
```

Remove the absolute-path rejection for `config_path`; the config path is now owned by `XdgPaths`.

- [ ] **Step 5: Update provider config tests**

Replace the `local_openai_config_*` tests in `main.rs` with:

```rust
#[test]
fn openai_debug_config_uses_xdg_toml_provider_and_secret_file() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let config_dir = temp.path().join("config/merry");
    std::fs::create_dir_all(config_dir.join("secrets")).expect("config dir should be created");
    std::fs::write(config_dir.join("secrets/openai.key"), "sk-test\n")
        .expect("secret file should write");
    let paths = config::XdgPaths::from_parts(
        PathBuf::from("/home/alice"),
        Some(temp.path().join("config")),
        Some(temp.path().join("state")),
    );
    let config = config::MerryConfig::load_optional_from_text(
        Some(
            r#"
[providers.default]
provider = "openai-compatible"
model = "gpt-test"

[providers.openai-compatible]
base_url = "https://api.example.test/v1"
api_key_file = "secrets/openai.key"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let loaded = debug_openai_config_with_env(None, Some(&config), |name| {
        Ok((name == "MERRY_OPENAI_DEBUG").then(|| "1".to_owned()))
    })
    .expect("debug config should load");
    assert_eq!(loaded.model, "gpt-test");
    assert_eq!(loaded.provider.base_url(), "https://api.example.test/v1");
}
```

- [ ] **Step 6: Update CLI integration test expectations**

In `crates/merry-cli/tests/debug.rs`, update the live-smoke clap test so `--config` is rejected and the default command parses without it:

```rust
#[test]
fn coding_loop_live_smoke_rejects_legacy_config_flag() {
    let output = merry()
        .args(["debug", "coding-loop-live-smoke", "--config", ".merry/secrets/openai.env"])
        .output()
        .expect("merry should run");

    assert_eq!(output.status.code(), Some(2));
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("unexpected argument") || stderr.contains("--config"));
}
```

- [ ] **Step 7: Run focused tests**

```bash
cargo test -p merry-cli openai_debug_config_uses_xdg_toml_provider_and_secret_file -- --nocapture
cargo test -p merry-cli coding_loop_live_smoke_rejects_legacy_config_flag --test debug
```

Expected: the live-smoke debug path no longer depends on `.merry/secrets/openai.env`; sandboxed live credentials can live under `~/.config/merry/secrets/`.

- [ ] **Step 8: Commit provider config migration**

```bash
git add crates/merry-cli/src/config.rs crates/merry-cli/src/main.rs crates/merry-cli/tests/debug.rs
git commit -m "feat(cli): load live provider config from xdg toml"
```

## Task 5: Runtime Loop And Process Tracing

**Files:**
- Modify: `crates/merry-runtime/Cargo.toml`
- Modify: `crates/merry-runtime/src/agent_loop.rs`
- Modify: `crates/merry-runtime/src/runtime.rs`

- [ ] **Step 1: Add runtime test dependency**

Edit `crates/merry-runtime/Cargo.toml`:

```toml
[dev-dependencies]
tracing-subscriber.workspace = true
```

- [ ] **Step 2: Add trace capture helper in runtime tests**

In `agent_loop.rs` test module, add:

```rust
fn capture_traces<F: FnOnce() -> R, R>(run: F) -> (R, String) {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{fmt, prelude::*};

    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("buffer mutex should not poison").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let bytes = Arc::new(Mutex::new(Vec::new()));
    let writer = Buffer(Arc::clone(&bytes));
    let subscriber = tracing_subscriber::registry().with(fmt::layer().json().with_writer(move || writer.clone()));
    let result = tracing::subscriber::with_default(subscriber, run);
    let text = String::from_utf8(bytes.lock().expect("buffer mutex should not poison").clone())
        .expect("trace output should be UTF-8");
    (result, text)
}
```

- [ ] **Step 3: Write loop tracing test**

Add a deterministic test using the existing fake provider/runner harness:

```rust
#[tokio::test(flavor = "current_thread")]
async fn agent_loop_traces_loop_steps_tool_and_terminal_status() {
    let runtime = runtime_with_scripted_process_loop().expect("runtime should build");
    let input = StepInput::user_text("inspect, patch, and verify").expect("valid input");
    let context = StepContext::default();
    let config = AgentLoopConfig::new(8).expect("valid config");

    let (result, logs) = capture_traces(|| {
        futures_executor::block_on(runtime.run_agent_loop(input, context, config))
    });

    let result = result.expect("loop should run");
    assert!(matches!(result.status(), AgentLoopStatus::Completed));
    assert!(logs.contains("\"event\":\"runtime.loop.start\""));
    assert!(logs.contains("\"event\":\"runtime.step.start\""));
    assert!(logs.contains("\"event\":\"runtime.tool.pending\""));
    assert!(logs.contains("\"event\":\"runtime.tool.execute.start\""));
    assert!(logs.contains("\"event\":\"runtime.tool.execute.finish\""));
    assert!(logs.contains("\"event\":\"runtime.loop.finish\""));
    assert!(logs.contains("\"status\":\"completed\""));
    assert!(logs.contains("\"tool_name\":\"run_process\""));
}
```

Use the existing fake process-loop fixture from runtime tests; if its helper is private to another test, move that helper inside the test module without changing runtime public API.

- [ ] **Step 4: Instrument `Runtime::run_agent_loop`**

In `agent_loop.rs`, add `tracing::info!` records:

```rust
tracing::info!(
    event = "runtime.loop.start",
    session_id = self.inner.session_id.as_str(),
    max_steps = config.max_steps(),
    "runtime loop start"
);
```

Before each step:

```rust
let step_index = steps_run + 1;
tracing::info!(
    event = "runtime.step.start",
    session_id = self.inner.session_id.as_str(),
    step_index,
    "runtime loop step start"
);
```

When a pending tool is selected:

```rust
tracing::info!(
    event = "runtime.tool.pending",
    session_id = self.inner.session_id.as_str(),
    step_index = steps_run,
    tool_call_id = call.id().as_str(),
    tool_name = call.name().as_str(),
    "runtime loop saw pending tool"
);
tracing::info!(
    event = "runtime.tool.execute.start",
    session_id = self.inner.session_id.as_str(),
    step_index = steps_run,
    tool_call_id = call.id().as_str(),
    tool_name = call.name().as_str(),
    "runtime loop tool execution start"
);
```

After successful execution events:

```rust
let status = tool_resolution_status(&execution_events);
let artifact_id = tool_resolution_artifact_id(&execution_events);
tracing::info!(
    event = "runtime.tool.execute.finish",
    session_id = self.inner.session_id.as_str(),
    step_index = steps_run,
    tool_call_id = call.id().as_str(),
    tool_name = call.name().as_str(),
    status,
    artifact_id,
    "runtime loop tool execution finish"
);
```

Add small helpers that inspect `RuntimeEventKind::ToolCallResolved` and return borrowed-safe `String` values:

```rust
fn tool_resolution_status(events: &[RuntimeEvent]) -> &'static str {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(match result.status() {
                merry_core::ToolCallResultStatus::Succeeded => "succeeded",
                merry_core::ToolCallResultStatus::Failed => "failed",
            }),
            _ => None,
        })
        .unwrap_or("unresolved")
}

fn tool_resolution_artifact_id(events: &[RuntimeEvent]) -> String {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RuntimeEventKind::ToolCallResolved { result } => Some(result.artifact().id().as_str().to_owned()),
            _ => None,
        })
        .unwrap_or_default()
}
```

Before every return, emit `runtime.loop.finish` with `status` and `steps_run`.

- [ ] **Step 5: Instrument process execution path**

In `Runtime::execute_admitted_process_action`, emit:

```rust
tracing::info!(
    event = "runtime.process.execute.start",
    session_id = self.inner.session_id.as_str(),
    tool_call_id = pending.id().as_str(),
    tool_name = pending.name().as_str(),
    argv = ?intent.argv(),
    cwd = intent.cwd().unwrap_or("."),
    stdout_limit_bytes = intent.stdout_limit_bytes(),
    stderr_limit_bytes = intent.stderr_limit_bytes(),
    "runtime process execution start"
);
```

After runner output and before recording the result:

```rust
tracing::info!(
    event = "runtime.process.execute.finish",
    session_id = self.inner.session_id.as_str(),
    tool_call_id = pending.id().as_str(),
    tool_name = pending.name().as_str(),
    status = process_status_label(output.status()),
    stdout_bytes = output.stdout_bytes(),
    stderr_bytes = output.stderr_bytes(),
    stdout_truncated = output.stdout_truncated(),
    stderr_truncated = output.stderr_truncated(),
    "runtime process execution finish"
);
```

On denied process action, emit `runtime.tool.execute.finish` with `status = "denied"` and diagnostic code `action_policy_denied`.

- [ ] **Step 6: Run runtime trace tests**

```bash
cargo test -p merry-runtime agent_loop_traces_loop_steps_tool_and_terminal_status -- --nocapture
cargo test -p merry-runtime process -- --nocapture
```

Expected: trace assertions pass without network or bwrap.

- [ ] **Step 7: Commit runtime instrumentation**

```bash
git add crates/merry-runtime/Cargo.toml crates/merry-runtime/src/agent_loop.rs crates/merry-runtime/src/runtime.rs
git commit -m "feat(runtime): trace agent loop and process actions"
```

## Task 6: Workspace Tool And Provider Trace Alignment

**Files:**
- Modify: `crates/merry-tool-workspace/Cargo.toml`
- Modify: `crates/merry-tool-workspace/src/lib.rs`
- Modify: `crates/merry-provider-openai/src/provider.rs`

- [ ] **Step 1: Add workspace tool tracing dependency**

Edit `crates/merry-tool-workspace/Cargo.toml`:

```toml
[dependencies]
tracing.workspace = true

[dev-dependencies]
tracing-subscriber.workspace = true
```

- [ ] **Step 2: Instrument workspace read/list/search/patch**

In each `ToolExecutor` implementation, log start before blocking work and finish after outcome construction:

```rust
tracing::info!(
    event = "runtime.workspace_tool.start",
    tool_name = WORKSPACE_READ_FILE_TOOL,
    path = args.path.as_str(),
    "workspace tool start"
);
```

For successful read/list/search:

```rust
tracing::info!(
    event = "runtime.workspace_tool.finish",
    tool_name = WORKSPACE_READ_FILE_TOOL,
    path = args.path.as_str(),
    status = "succeeded",
    "workspace tool finish"
);
```

For failed outcomes, include the diagnostic code already placed in the `ToolExecutionOutcome`:

```rust
tracing::info!(
    event = "runtime.workspace_tool.finish",
    tool_name = WORKSPACE_READ_FILE_TOOL,
    path = args.path.as_str(),
    status = "failed",
    diagnostic_code = ERROR_PATH_DENIED,
    "workspace tool finish"
);
```

For search, log `query_bytes = args.query.len()` instead of raw query when the query is longer than 128 bytes. For patch, log `relative_path`, `preimage_bytes`, and `replacement_bytes`, never full file content.

- [ ] **Step 3: Add workspace trace capture tests**

Add tests beside existing executor tests:

```rust
#[tokio::test(flavor = "current_thread")]
async fn workspace_read_file_traces_start_and_finish_without_file_contents() {
    let root = tempfile::tempdir().expect("tempdir should be created");
    std::fs::write(root.path().join("lib.rs"), "secret source text").expect("file should write");
    let executor = ReadFileExecutor {
        state: workspace_state_for_test(root.path()),
    };
    let call = pending_read_file_call("lib.rs");

    let (outcome, logs) = capture_traces(|| {
        futures_executor::block_on(executor.execute(call, ToolExecutionContext::default()))
    });

    assert!(outcome.expect("read should succeed").diagnostic().is_none());
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
    assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
    assert!(logs.contains("\"path\":\"lib.rs\""));
    assert!(!logs.contains("secret source text"));
}
```

Reuse the same capture helper shape as runtime tests. Keep helpers inside the test module.

- [ ] **Step 4: Align OpenAI provider metadata fields**

In `crates/merry-provider-openai/src/provider.rs`, change the `debug_span!` name and fields to match the runtime contract:

```rust
let stream_span = tracing::debug_span!(
    "runtime.provider.request",
    event = "runtime.provider.request",
    provider_name = self.config.provider_name().as_str(),
    model = request.model().as_str(),
    message_count = request.messages().len(),
    tool_count = request.tools().len(),
    continuation_count = request.continuations().len(),
    max_output_tokens = ?request.generation().max_output_tokens(),
    allow_parallel_tool_calls = request.generation().allow_parallel_tool_calls(),
    endpoint_path = tracing::field::Empty,
);
```

Keep existing request-body logging at `trace` as metadata only. Do not log raw HTTP body, API key, prompt text, tool result content, or provider response payload.

- [ ] **Step 5: Add provider trace safety test**

In `crates/merry-provider-openai/src/provider.rs`, add a helper that emits provider request metadata without sending HTTP:

```rust
fn trace_openai_request_metadata(
    config: &OpenAiProviderConfig,
    request: &ModelRequest,
    endpoint_path: &str,
) {
    tracing::debug!(
        event = "runtime.provider.request",
        provider_name = config.provider_name().as_str(),
        model = request.model().as_str(),
        message_count = request.messages().len(),
        tool_count = request.tools().len(),
        continuation_count = request.continuations().len(),
        max_output_tokens = ?request.generation().max_output_tokens(),
        allow_parallel_tool_calls = request.generation().allow_parallel_tool_calls(),
        endpoint_path,
        "openai provider request metadata"
    );
}
```

Call it from `stream_model` immediately after `build_responses_http_request` succeeds:

```rust
let http_request = build_responses_http_request(&self.config, &request)?;
trace_openai_request_metadata(&self.config, &request, http_request.endpoint.path());
event_stream_span.record("endpoint_path", http_request.endpoint.path());
```

Add a provider unit test for the helper:

```rust
#[test]
fn provider_trace_metadata_does_not_include_api_key_or_prompt_text() {
    let config = OpenAiProviderConfig::new("sk-test-secret")
        .expect("valid config")
        .with_base_url("https://api.example.test/v1")
        .expect("valid base url");
    let request = request_without_tools();

    let (_, logs) = capture_traces(|| {
        trace_openai_request_metadata(&config, &request, "/responses");
    });

    assert!(logs.contains("runtime.provider.request"));
    assert!(!logs.contains("sk-test-secret"));
    assert!(!logs.contains("Hello"));
}
```

- [ ] **Step 6: Run focused tests**

```bash
cargo test -p merry-tool-workspace workspace_read_file_traces_start_and_finish_without_file_contents -- --nocapture
cargo test -p merry-provider-openai provider_trace_metadata_does_not_include_api_key_or_prompt_text -- --nocapture
```

Expected: logs include safe metadata and exclude secrets/content.

- [ ] **Step 7: Commit tool/provider trace alignment**

```bash
git add crates/merry-tool-workspace/Cargo.toml crates/merry-tool-workspace/src/lib.rs crates/merry-provider-openai/src/provider.rs
git commit -m "feat: trace workspace tools and provider metadata"
```

## Task 7: End-To-End Log-Enabled Smoke Verification

**Files:**
- Modify: `crates/merry-cli/tests/debug.rs`
- Modify: `README.md` only if the implemented command behavior changes public usage text.

- [ ] **Step 1: Add deterministic CLI log smoke**

Add a CLI integration test that runs the deterministic debug command with config-backed logs:

```rust
#[test]
fn debug_command_writes_runtime_action_logs_to_default_xdg_state_path() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let config_dir = temp.path().join("config/merry");
    let state_dir = temp.path().join("state");
    std::fs::create_dir_all(&config_dir).expect("config dir should be created");
    std::fs::write(
        config_dir.join("config.toml"),
        "[observability.log]\nenabled = true\nlevel = \"debug\"\nformat = \"json\"\n",
    )
    .expect("config should write");

    let output = merry()
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_STATE_HOME", &state_dir)
        .arg("debug")
        .output()
        .expect("merry debug should run");

    assert!(output.status.success(), "debug should exit successfully");
    assert_debug_output(&output.stdout, "debug-session");

    let log_path = state_dir.join("merry/logs/merry.jsonl");
    let log = std::fs::read_to_string(&log_path).expect("default log file should exist");
    assert!(log.contains("runtime.step"));
    assert!(log.contains("debug-session"));
}
```

- [ ] **Step 2: Add log path failure integration test**

```rust
#[test]
fn debug_command_fails_clearly_when_default_log_parent_cannot_be_created() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let config_dir = temp.path().join("config/merry");
    let state_path = temp.path().join("state");
    std::fs::create_dir_all(&config_dir).expect("config dir should be created");
    std::fs::write(&state_path, "not a directory").expect("state blocker should write");
    std::fs::write(
        config_dir.join("config.toml"),
        "[observability.log]\nenabled = true\nlevel = \"info\"\nformat = \"json\"\n",
    )
    .expect("config should write");

    let output = merry()
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_STATE_HOME", &state_path)
        .arg("debug")
        .output()
        .expect("merry debug should run");

    assert!(!output.status.success(), "debug should fail");
    assert!(output.stdout.is_empty(), "failed logging setup should not write command stdout");
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("failed to create log directory") || stderr.contains("failed to open log file"));
}
```

- [ ] **Step 3: Run full default validation**

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: all default checks pass without bwrap, network, or live credentials.

- [ ] **Step 4: Run manual bwrap smoke with logs**

Create a local config outside the repo, using a temporary XDG root:

```bash
tmp="$(mktemp -d)"
mkdir -p "$tmp/config/merry"
cat > "$tmp/config/merry/config.toml" <<'CONFIG'
[observability.log]
enabled = true
level = "debug"
format = "json"

[providers.default]
provider = "openai-compatible"
model = "gpt-4.1-mini"

[providers.openai-compatible]
base_url = "https://api.openai.com/v1"
api_key_file = "secrets/openai.key"
CONFIG
XDG_CONFIG_HOME="$tmp/config" XDG_STATE_HOME="$tmp/state" cargo run -p merry-cli -- --with-sandbox debug coding-loop-smoke
tail -n 40 "$tmp/state/merry/logs/merry.jsonl"
```

Expected: stdout contains `coding-loop-smoke: ok`; the log file contains `runtime.loop.start`, `runtime.provider.request`, `runtime.tool.pending`, `runtime.process.execute.start`, `runtime.process.execute.finish`, and `runtime.loop.finish`.

- [ ] **Step 5: Commit final verification coverage**

```bash
git add crates/merry-cli/tests/debug.rs README.md
git commit -m "test(cli): cover config-backed log smokes"
```

## Self-Review Checklist

- The plan implements the approved observability-first spec: XDG config, TOML parsing, default log path, sandbox config/log mounts, config-backed subscriber setup, runtime/process/tool/provider traces, and deterministic tests.
- The plan does not add root `--log-level` or `--log-format` flags.
- The live provider path moves away from repo-local `.merry/secrets/openai.env`; sandboxed credentials can live under `~/.config/merry/secrets/`.
- Default tests remain offline and deterministic.
- Logs stay separate from command stdout.
- Secrets, raw provider payloads, full prompts, full file contents, and unbounded stdout/stderr are excluded from default logs.
