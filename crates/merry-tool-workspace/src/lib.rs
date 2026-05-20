//! Read-only workspace tools for Merry runtimes.
//!
//! This crate is intentionally outside `merry-runtime`: it adapts filesystem
//! reads into runtime-registered tools without making the runtime own real
//! workspace access policy.
//!
//! Path safety is scoped to trusted, stable workspace roots. The MVP rejects
//! absolute paths, parent-directory traversal, hidden paths unless explicitly
//! enabled, and ordinary symlink components before reading. On Unix, the final
//! file open also uses `O_NOFOLLOW` to avoid following a symlink swapped into
//! the leaf path between validation and open. This is not an OS sandbox and
//! does not claim complete hardening against malicious concurrent filesystem
//! mutation, including replacement of intermediate directories during a read.

use merry_core::{ErrorInfo, PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use merry_runtime::{
    RegisteredTool, ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
#[cfg(unix)]
use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
use thiserror::Error;

/// Registered tool name for read-only file reads.
pub const WORKSPACE_READ_FILE_TOOL: &str = "workspace_read_file";

const ERROR_INVALID_ARGUMENTS: &str = "workspace_invalid_arguments";
const ERROR_PATH_DENIED: &str = "workspace_path_denied";
const ERROR_FILE_NOT_FOUND: &str = "workspace_file_not_found";
const ERROR_NOT_FILE: &str = "workspace_path_not_file";
const ERROR_FILE_TOO_LARGE: &str = "workspace_file_too_large";
const ERROR_NOT_UTF8: &str = "workspace_file_not_utf8";
const ERROR_READ_FAILED: &str = "workspace_read_failed";

/// Limits applied by read-only workspace tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceToolLimits {
    /// Maximum bytes returned by `workspace_read_file`.
    pub max_read_bytes: usize,
}

impl Default for WorkspaceToolLimits {
    fn default() -> Self {
        Self {
            max_read_bytes: 1024 * 1024,
        }
    }
}

/// Configuration for read-only workspace tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceToolsConfig {
    roots: Vec<PathBuf>,
    allow_hidden: bool,
    limits: WorkspaceToolLimits,
}

impl WorkspaceToolsConfig {
    /// Creates a config with explicit workspace roots.
    #[must_use]
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            allow_hidden: false,
            limits: WorkspaceToolLimits::default(),
        }
    }

    /// Returns the configured, pre-canonical roots.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Returns whether hidden path components are allowed.
    #[must_use]
    pub fn allow_hidden(&self) -> bool {
        self.allow_hidden
    }

    /// Sets whether hidden path components are allowed.
    #[must_use]
    pub fn with_allow_hidden(mut self, allow_hidden: bool) -> Self {
        self.allow_hidden = allow_hidden;
        self
    }

    /// Returns the configured tool limits.
    #[must_use]
    pub fn limits(&self) -> &WorkspaceToolLimits {
        &self.limits
    }

    /// Sets read-only workspace tool limits.
    #[must_use]
    pub fn with_limits(mut self, limits: WorkspaceToolLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// Errors raised while validating read-only workspace tool configuration.
#[derive(Debug, Error)]
pub enum WorkspaceToolConfigError {
    /// At least one root must be configured explicitly.
    #[error("at least one workspace root must be configured")]
    NoRoots,
    /// A configured root does not exist.
    #[error("workspace root does not exist: {root}")]
    RootNotFound {
        /// Configured root path.
        root: PathBuf,
    },
    /// A configured root could not be canonicalized.
    #[error("could not canonicalize workspace root {root}: {source}")]
    RootCanonicalize {
        /// Configured root path.
        root: PathBuf,
        /// Source IO error.
        #[source]
        source: io::Error,
    },
    /// A configured root is not a directory.
    #[error("workspace root is not a directory: {root}")]
    RootNotDirectory {
        /// Configured root path.
        root: PathBuf,
    },
    /// A numeric limit is invalid.
    #[error("workspace tool limit {name} must be greater than zero")]
    InvalidLimit {
        /// Limit name.
        name: &'static str,
    },
}

/// Read-only workspace tools that can be registered with `merry-runtime`.
#[derive(Debug, Clone)]
pub struct ReadOnlyWorkspaceTools {
    state: Arc<WorkspaceToolState>,
}

impl ReadOnlyWorkspaceTools {
    /// Validates configuration and creates read-only workspace tools.
    pub fn new(config: WorkspaceToolsConfig) -> Result<Self, WorkspaceToolConfigError> {
        let state = WorkspaceToolState::new(config)?;
        Ok(Self {
            state: Arc::new(state),
        })
    }

    /// Returns the current first-slice registered tools.
    ///
    /// Only `workspace_read_file` is exposed in this first slice. Directory
    /// listing and text search are intentionally left for a follow-up slice.
    #[must_use]
    pub fn into_registered_tools(self) -> Vec<RegisteredTool> {
        vec![RegisteredTool::new(
            read_file_spec(),
            Arc::new(ReadFileExecutor { state: self.state }),
        )]
    }
}

#[derive(Debug)]
struct WorkspaceToolState {
    roots: Vec<PathBuf>,
    allow_hidden: bool,
    limits: WorkspaceToolLimits,
}

impl WorkspaceToolState {
    fn new(config: WorkspaceToolsConfig) -> Result<Self, WorkspaceToolConfigError> {
        if config.roots.is_empty() {
            return Err(WorkspaceToolConfigError::NoRoots);
        }

        if config.limits.max_read_bytes == 0 {
            return Err(WorkspaceToolConfigError::InvalidLimit {
                name: "max_read_bytes",
            });
        }

        let mut roots = Vec::with_capacity(config.roots.len());
        for root in config.roots {
            if !root.exists() {
                return Err(WorkspaceToolConfigError::RootNotFound { root });
            }

            let canonical = fs::canonicalize(&root).map_err(|source| {
                WorkspaceToolConfigError::RootCanonicalize {
                    root: root.clone(),
                    source,
                }
            })?;

            if !canonical.is_dir() {
                return Err(WorkspaceToolConfigError::RootNotDirectory { root });
            }

            roots.push(canonical);
        }

        Ok(Self {
            roots,
            allow_hidden: config.allow_hidden,
            limits: config.limits,
        })
    }
}

#[derive(Debug)]
struct ReadFileExecutor {
    state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for ReadFileExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let path = match parse_read_file_args(&call) {
                Ok(args) => args.path,
                Err(message) => {
                    return Ok(failed_outcome(
                        ERROR_INVALID_ARGUMENTS,
                        message,
                        None::<String>,
                    ));
                }
            };

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let handle = tokio::task::spawn_blocking(move || read_file_blocking(&state, path));

            tokio::select! {
                biased;
                () = token.cancelled() => Err(ToolExecutionError::Cancelled),
                joined = handle => match joined {
                    Ok(outcome) => {
                        if token.is_cancelled() {
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            Ok(outcome)
                        }
                    }
                    Err(error) => Err(ToolExecutionError::infrastructure(format!(
                        "workspace read task failed to join: {error}"
                    ))),
                },
            }
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    path: String,
}

#[derive(Debug, Serialize)]
struct ReadFileSuccess<'a> {
    ok: bool,
    tool: &'static str,
    path: &'a str,
    bytes: usize,
    truncated: bool,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct FailureEnvelope<'a> {
    ok: bool,
    tool: &'static str,
    error: FailureError<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct FailureError<'a> {
    code: &'a str,
    message: &'a str,
}

fn read_file_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_READ_FILE_TOOL).expect("static workspace tool name is valid"),
        "Read a UTF-8 file under a configured stable workspace root, rejecting traversal and ordinary symlink paths.",
        ToolInputSchema::new(schema_for!(ReadFileArgs))
            .expect("static workspace_read_file input schema is valid"),
    )
    .expect("static workspace_read_file spec is valid")
}

fn parse_read_file_args(call: &PendingToolCall) -> Result<ReadFileArgs, String> {
    serde_json::from_value(serde_json::Value::Object(
        call.arguments().as_object().clone(),
    ))
    .map_err(|error| format!("invalid workspace_read_file arguments: {error}"))
}

fn read_file_blocking(state: &WorkspaceToolState, path: String) -> ToolExecutionOutcome {
    let relative = match validate_relative_path(&path, state.allow_hidden) {
        Ok(relative) => relative,
        Err(error) => return failed_outcome(error.code, error.message, error.path),
    };

    for root in &state.roots {
        match read_existing_file(root, &relative, state.limits.max_read_bytes) {
            Ok(Some(success)) => return success,
            Ok(None) => {}
            Err(error) => return failed_outcome(error.code, error.message, Some(relative.display)),
        }
    }

    failed_outcome(
        ERROR_FILE_NOT_FOUND,
        "workspace file was not found",
        Some(relative.display),
    )
}

fn read_existing_file(
    root: &Path,
    relative: &ValidatedRelativePath,
    max_read_bytes: usize,
) -> Result<Option<ToolExecutionOutcome>, DomainError> {
    let mut current = root.to_path_buf();
    for component in &relative.components {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(DomainError::new(
                    ERROR_READ_FAILED,
                    "could not inspect workspace path",
                ));
            }
        };

        if metadata.file_type().is_symlink() {
            return Err(DomainError::new(
                ERROR_PATH_DENIED,
                "workspace path uses a symlink",
            ));
        }
    }

    let canonical = fs::canonicalize(&current).map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not canonicalize workspace file path",
        )
    })?;
    if !canonical.starts_with(root) {
        return Err(DomainError::new(
            ERROR_PATH_DENIED,
            "workspace path resolves outside a configured root",
        ));
    }

    let mut file = open_file_for_read(&current)?;
    let metadata = file.metadata().map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;

    if !metadata.is_file() {
        return Err(DomainError::new(
            ERROR_NOT_FILE,
            "workspace path is not a regular file",
        ));
    }

    if metadata.len() > max_read_bytes as u64 {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        ));
    }

    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_read_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;

    if bytes.len() > max_read_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        ));
    }

    let content = String::from_utf8(bytes)
        .map_err(|_| DomainError::new(ERROR_NOT_UTF8, "workspace file is not valid UTF-8"))?;

    let payload = ReadFileSuccess {
        ok: true,
        tool: WORKSPACE_READ_FILE_TOOL,
        path: &relative.display,
        bytes: content.len(),
        truncated: false,
        content: &content,
    };
    Ok(Some(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace read success envelope serializes"),
    )))
}

fn open_file_for_read(path: &Path) -> Result<fs::File, DomainError> {
    open_file_for_read_impl(path).map_err(|error| {
        if is_symlink_open_error(&error) {
            DomainError::new(ERROR_PATH_DENIED, "workspace path uses a symlink")
        } else {
            DomainError::new(ERROR_READ_FAILED, "could not open workspace file")
        }
    })
}

#[cfg(unix)]
fn open_file_for_read_impl(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}

#[cfg(not(unix))]
fn open_file_for_read_impl(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(unix)]
fn is_symlink_open_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_open_error(_: &io::Error) -> bool {
    false
}

#[derive(Debug)]
struct ValidatedRelativePath {
    components: Vec<String>,
    display: String,
}

fn validate_relative_path(
    raw_path: &str,
    allow_hidden: bool,
) -> Result<ValidatedRelativePath, PathValidationError> {
    if raw_path.is_empty() {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must not be empty",
            None,
        ));
    }

    if raw_path.chars().any(char::is_control) {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must not contain control characters",
            None,
        ));
    }

    let path = Path::new(raw_path);
    if path.is_absolute() {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must be relative",
            None,
        ));
    }

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let Some(value) = value.to_str() else {
                    return Err(PathValidationError::new(
                        ERROR_PATH_DENIED,
                        "workspace path component must be UTF-8",
                        None,
                    ));
                };
                if !allow_hidden && value.starts_with('.') {
                    return Err(PathValidationError::new(
                        ERROR_PATH_DENIED,
                        "workspace hidden paths are not allowed",
                        Some(raw_path.to_owned()),
                    ));
                }
                components.push(value.to_owned());
            }
            Component::CurDir => {
                return Err(PathValidationError::new(
                    ERROR_PATH_DENIED,
                    "workspace path must not contain '.' components",
                    Some(raw_path.to_owned()),
                ));
            }
            Component::ParentDir => {
                return Err(PathValidationError::new(
                    ERROR_PATH_DENIED,
                    "workspace path must not contain '..' components",
                    Some(raw_path.to_owned()),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathValidationError::new(
                    ERROR_PATH_DENIED,
                    "workspace path must be relative",
                    None,
                ));
            }
        }
    }

    if components.is_empty() {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must name a file",
            Some(raw_path.to_owned()),
        ));
    }

    let display = components.join("/");
    Ok(ValidatedRelativePath {
        components,
        display,
    })
}

#[derive(Debug)]
struct DomainError {
    code: &'static str,
    message: &'static str,
}

impl DomainError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

#[derive(Debug)]
struct PathValidationError {
    code: &'static str,
    message: &'static str,
    path: Option<String>,
}

impl PathValidationError {
    fn new(code: &'static str, message: &'static str, path: Option<String>) -> Self {
        Self {
            code,
            message,
            path,
        }
    }
}

fn failed_outcome(
    code: &'static str,
    message: impl Into<String>,
    path: Option<String>,
) -> ToolExecutionOutcome {
    let message = message.into();
    let envelope = FailureEnvelope {
        ok: false,
        tool: WORKSPACE_READ_FILE_TOOL,
        error: FailureError {
            code,
            message: &message,
        },
        path: path.as_deref(),
    };
    ToolExecutionOutcome::failed_json(
        serde_json::to_string(&envelope).expect("workspace read failure envelope serializes"),
        ErrorInfo::new(code, &message).expect("workspace read diagnostic is valid"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ToolCallArguments, ToolCallId, ToolCallResultStatus};
    use merry_runtime::{ArtifactContentKind, ToolExecutionError};
    use serde_json::{Map, Value, json};
    use std::{
        env,
        ffi::OsStr,
        fs::{self, File},
        io::Write,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    struct TempWorkspace {
        root: PathBuf,
    }

    impl TempWorkspace {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after unix epoch")
                .as_nanos();
            let root = env::temp_dir().join(format!(
                "merry-tool-workspace-{name}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("temp workspace should be created");
            Self { root }
        }

        fn path(&self) -> &Path {
            &self.root
        }

        fn write_text(&self, relative: &str, content: &str) {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent directory should be created");
            }
            fs::write(path, content).expect("text file should be written");
        }

        fn write_bytes(&self, relative: &str, content: &[u8]) {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent directory should be created");
            }
            let mut file = File::create(path).expect("binary file should be created");
            file.write_all(content)
                .expect("binary file should be written");
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn tools_for(root: &Path) -> ReadOnlyWorkspaceTools {
        ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![root.to_path_buf()]))
            .expect("workspace tools should construct")
    }

    fn read_outcome(tools: &ReadOnlyWorkspaceTools, path: &str) -> ToolExecutionOutcome {
        read_file_blocking(&tools.state, path.to_owned())
    }

    fn json_content(outcome: &ToolExecutionOutcome) -> Value {
        serde_json::from_str(
            outcome
                .content()
                .as_text()
                .expect("json content should be text"),
        )
        .expect("tool outcome should be JSON")
    }

    fn assert_failed_json(
        outcome: &ToolExecutionOutcome,
        code: &str,
        path: Option<&str>,
        host_root: &Path,
    ) {
        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(outcome.content().kind(), ArtifactContentKind::Json);
        assert_eq!(outcome.diagnostic().expect("diagnostic").code(), code);

        let payload = json_content(outcome);
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["tool"], WORKSPACE_READ_FILE_TOOL);
        assert_eq!(payload["error"]["code"], code);
        if let Some(path) = path {
            assert_eq!(payload["path"], path);
        } else {
            assert!(
                payload.get("path").is_none(),
                "failure payload should omit path"
            );
        }

        assert!(
            !outcome
                .content()
                .as_text()
                .expect("json content")
                .contains(host_root.to_str().expect("temp path utf8")),
            "tool output must not include absolute host roots"
        );
    }

    fn pending_call(arguments: Value) -> PendingToolCall {
        let arguments = ToolCallArguments::try_from(arguments).expect("arguments object");
        PendingToolCall::new(
            ToolCallId::new("call-1").expect("valid call id"),
            ToolName::new(WORKSPACE_READ_FILE_TOOL).expect("valid tool name"),
            arguments,
        )
    }

    #[test]
    fn config_rejects_missing_roots() {
        let err = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(Vec::new()))
            .expect_err("empty roots should be rejected");
        assert!(matches!(err, WorkspaceToolConfigError::NoRoots));
    }

    #[test]
    fn config_rejects_non_directory_root() {
        let temp = TempWorkspace::new("non-directory-root");
        temp.write_text("file.txt", "content\n");

        let err = ReadOnlyWorkspaceTools::new(WorkspaceToolsConfig::new(vec![
            temp.path().join("file.txt"),
        ]))
        .expect_err("file root should be rejected");
        assert!(matches!(
            err,
            WorkspaceToolConfigError::RootNotDirectory { .. }
        ));
    }

    #[test]
    fn config_rejects_zero_read_limit() {
        let temp = TempWorkspace::new("zero-limit");
        let config = WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
            .with_limits(WorkspaceToolLimits { max_read_bytes: 0 });

        let err = ReadOnlyWorkspaceTools::new(config).expect_err("zero limit should be rejected");
        assert!(matches!(
            err,
            WorkspaceToolConfigError::InvalidLimit {
                name: "max_read_bytes"
            }
        ));
    }

    #[test]
    fn into_registered_tools_exposes_only_read_file_first_slice() {
        let temp = TempWorkspace::new("registered-tools");
        let tools = tools_for(temp.path()).into_registered_tools();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].spec().name().as_str(), WORKSPACE_READ_FILE_TOOL);
    }

    #[test]
    fn read_file_success_returns_stable_json_without_host_root() {
        let temp = TempWorkspace::new("read-success");
        temp.write_text("dir/note.txt", "alpha\nbeta\n");
        let tools = tools_for(temp.path());

        let outcome = read_outcome(&tools, "dir/note.txt");
        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(
            payload,
            json!({
                "ok": true,
                "tool": WORKSPACE_READ_FILE_TOOL,
                "path": "dir/note.txt",
                "bytes": 11,
                "truncated": false,
                "content": "alpha\nbeta\n"
            })
        );
        assert!(
            !outcome
                .content()
                .as_text()
                .expect("json content")
                .contains(temp.path().to_str().expect("temp path utf8")),
            "tool output must not include absolute host roots"
        );
    }

    #[test]
    fn read_file_allows_empty_utf8_file() {
        let temp = TempWorkspace::new("empty-file");
        temp.write_text("empty.txt", "");
        let tools = tools_for(temp.path());

        let outcome = read_outcome(&tools, "empty.txt");
        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(payload["bytes"], 0);
        assert_eq!(payload["content"], "");
    }

    #[test]
    fn read_file_rejects_absolute_parent_hidden_and_control_paths() {
        let temp = TempWorkspace::new("path-denied");
        temp.write_text("visible.txt", "ok\n");
        let tools = tools_for(temp.path());

        for denied in [
            "/etc/passwd".to_owned(),
            "../outside.txt".to_owned(),
            ".secret".to_owned(),
            format!("bad{}name", char::from(7)),
        ] {
            let outcome = read_outcome(&tools, &denied);
            let expected_path = if denied.starts_with('/') || denied.chars().any(char::is_control) {
                None
            } else {
                Some(denied.as_str())
            };
            assert_failed_json(&outcome, ERROR_PATH_DENIED, expected_path, temp.path());
        }
    }

    #[test]
    fn read_file_reports_missing_non_utf8_and_limit_failures() {
        let temp = TempWorkspace::new("domain-failures");
        temp.write_bytes("binary.bin", &[0xff, 0xfe, 0xfd]);
        temp.write_text("large.txt", "abcdef");
        let tools = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
                .with_limits(WorkspaceToolLimits { max_read_bytes: 5 }),
        )
        .expect("workspace tools should construct");

        let missing = read_outcome(&tools, "missing.txt");
        assert_failed_json(
            &missing,
            ERROR_FILE_NOT_FOUND,
            Some("missing.txt"),
            temp.path(),
        );

        let not_utf8 = read_outcome(&tools, "binary.bin");
        assert_failed_json(&not_utf8, ERROR_NOT_UTF8, Some("binary.bin"), temp.path());

        let too_large = read_outcome(&tools, "large.txt");
        assert_failed_json(
            &too_large,
            ERROR_FILE_TOO_LARGE,
            Some("large.txt"),
            temp.path(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_file_rejects_symlink_without_following_it() {
        let temp = TempWorkspace::new("symlink");
        temp.write_text("target.txt", "secret\n");
        symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
            .expect("symlink should be created");
        let tools = tools_for(temp.path());

        let outcome = read_outcome(&tools, "link.txt");
        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            outcome.diagnostic().expect("diagnostic").code(),
            ERROR_PATH_DENIED
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_open_file_for_read_rejects_trailing_symlink() {
        let temp = TempWorkspace::new("open-nofollow");
        temp.write_text("target.txt", "secret\n");
        symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
            .expect("symlink should be created");

        let error = open_file_for_read(temp.path().join("link.txt").as_path())
            .expect_err("O_NOFOLLOW open should reject trailing symlink");

        assert_eq!(error.code, ERROR_PATH_DENIED);
    }

    #[test]
    fn read_file_argument_validation_returns_domain_failure() {
        let temp = TempWorkspace::new("bad-args");
        let tools = tools_for(temp.path());
        let executor = ReadFileExecutor {
            state: Arc::clone(&tools.state),
        };
        let mut args = Map::new();
        args.insert("path".to_owned(), Value::Number(1.into()));
        let call = pending_call(Value::Object(args));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio runtime should build");

        let outcome = runtime
            .block_on(executor.execute(call, ToolExecutionContext::default()))
            .expect("invalid args should be domain failure");

        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_failed_json(&outcome, ERROR_INVALID_ARGUMENTS, None, temp.path());
    }

    #[test]
    fn read_file_executor_returns_cancelled_when_token_is_cancelled() {
        let temp = TempWorkspace::new("cancelled");
        temp.write_text("note.txt", "ok\n");
        let tools = tools_for(temp.path());
        let executor = ReadFileExecutor {
            state: Arc::clone(&tools.state),
        };
        let call = pending_call(json!({ "path": "note.txt" }));
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio runtime should build");

        let err = runtime
            .block_on(executor.execute(call, ToolExecutionContext::new(token)))
            .expect_err("cancelled execution should return cancellation error");

        assert!(matches!(err, ToolExecutionError::Cancelled));
    }

    #[test]
    fn hidden_paths_can_be_enabled_explicitly() {
        let temp = TempWorkspace::new("allow-hidden");
        temp.write_text(".secret", "ok\n");
        let tools = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_allow_hidden(true),
        )
        .expect("workspace tools should construct");

        let outcome = read_outcome(&tools, ".secret");
        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    }

    #[test]
    fn non_utf8_component_is_rejected_when_constructible() {
        let path = PathBuf::from(OsStr::new("plain"));
        let text = path.to_str().expect("plain path is utf8");
        let validated = validate_relative_path(text, false).expect("plain path validates");
        assert_eq!(validated.display, "plain");
    }
}
