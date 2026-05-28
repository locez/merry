//! Workspace tools for Merry runtimes.
//!
//! This crate is intentionally outside `merry-runtime`: it adapts filesystem
//! reads, read-only discovery, and opt-in constrained workspace edits into
//! runtime-registered tools without making the runtime own real workspace
//! access policy.
//!
//! Path safety is scoped to trusted, stable workspace roots. The MVP rejects
//! absolute paths, parent-directory traversal, ordinary dot components except
//! exact `.` where a tool addresses the root, hidden paths unless explicitly
//! enabled, and ordinary symlink components before reading, listing, searching,
//! or patching. On Unix, file opens also use `O_NOFOLLOW` to avoid following a
//! symlink swapped into the leaf path between validation and open. This is not
//! an OS sandbox and does not claim complete hardening against malicious
//! concurrent filesystem mutation, including replacement of intermediate
//! directories during an operation.

use merry_core::{
    ErrorInfo, PendingToolCall, ToolCallResultStatus, ToolInputSchema, ToolName, ToolSpec,
};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, ActionExecutionEvidence, ActionProposal,
    ActionProposalError, ActionProposalEvidence, ProcessCommandToolError, ProcessRunner,
    RegisteredTool, RuntimeBuilder, ToolActionKind, ToolActionProposalFuture, ToolExecutionContext,
    ToolExecutionError, ToolExecutionOutcome, ToolExecutor, ToolExecutorFuture,
    WorkspacePatchChangeEvidence, WorkspacePatchExecutionEvidence, WorkspacePatchProposal,
    process_command_tool,
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(test)]
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

/// Registered tool name for read-only file reads.
pub const WORKSPACE_READ_FILE_TOOL: &str = "workspace_read_file";
/// Registered tool name for non-recursive read-only directory listing.
pub const WORKSPACE_LIST_DIR_TOOL: &str = "workspace_list_dir";
/// Registered tool name for bounded read-only literal text search.
pub const WORKSPACE_SEARCH_TEXT_TOOL: &str = "workspace_search_text";
/// Registered tool name for opt-in constrained workspace patches.
pub const WORKSPACE_PATCH_TOOL: &str = "workspace_patch";
/// Registered tool name for runtime-owned process execution in the coding-loop profile.
pub const CODING_LOOP_PROCESS_TOOL: &str = "run_process";

const ERROR_INVALID_ARGUMENTS: &str = "workspace_invalid_arguments";
const ERROR_PATH_DENIED: &str = "workspace_path_denied";
const ERROR_FILE_NOT_FOUND: &str = "workspace_file_not_found";
const ERROR_PATH_NOT_FOUND: &str = "workspace_path_not_found";
const ERROR_NOT_FILE: &str = "workspace_path_not_file";
const ERROR_NOT_DIRECTORY: &str = "workspace_path_not_directory";
const ERROR_NOT_SEARCHABLE: &str = "workspace_path_not_searchable";
const ERROR_FILE_TOO_LARGE: &str = "workspace_file_too_large";
const ERROR_NOT_UTF8: &str = "workspace_file_not_utf8";
const ERROR_READ_FAILED: &str = "workspace_read_failed";
const ERROR_WRITE_FAILED: &str = "workspace_write_failed";
const ERROR_PROPOSAL_MISMATCH: &str = "workspace_patch_approved_mismatch";
const WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE: &str = "workspace patch plan changed before execution";
const WORKSPACE_PATH_CONTRACT: &str = "workspace tool path values are relative to a configured workspace root; do not prefix them with a process cwd, repository root, or absolute host path";
const ERROR_PREIMAGE_ABSENT: &str = "workspace_patch_preimage_absent";
const ERROR_PREIMAGE_AMBIGUOUS: &str = "workspace_patch_preimage_ambiguous";
const PROJECT_CAPABILITY_CONTEXT_ID: &str = "project-capabilities";
const TRACE_PATH_MAX_CHARS: usize = 96;
#[cfg(test)]
static PATCH_TEST_AFTER_WRITE_HOOK: OnceLock<Mutex<Option<PatchTestAfterWriteHook>>> =
    OnceLock::new();
#[cfg(test)]
static TRACE_START_TEST_HOOK: OnceLock<Mutex<Option<TraceStartTestHook>>> = OnceLock::new();

/// Limits applied by workspace tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceToolLimits {
    /// Maximum bytes read from one file by `workspace_read_file` and `workspace_search_text`.
    pub max_read_bytes: usize,
    /// Maximum bytes written to one file by `workspace_patch`.
    pub max_write_bytes: usize,
    /// Maximum bytes accepted in one `workspace_patch` patch payload.
    pub max_patch_bytes: usize,
    /// Maximum entries returned by `workspace_list_dir`.
    pub max_list_entries: usize,
    /// Maximum matches returned by `workspace_search_text`.
    pub max_search_matches: usize,
    /// Maximum regular files inspected by one `workspace_search_text` call.
    pub max_search_files: usize,
    /// Maximum directory entries inspected by one `workspace_search_text` call.
    pub max_search_entries: usize,
    /// Maximum total bytes scanned by one `workspace_search_text` call.
    pub max_search_bytes: usize,
    /// Maximum bytes returned for one matched line by `workspace_search_text`.
    pub max_search_line_bytes: usize,
    /// Maximum bytes accepted in a `workspace_search_text` query.
    pub max_search_query_bytes: usize,
}

impl Default for WorkspaceToolLimits {
    fn default() -> Self {
        Self {
            max_read_bytes: 1024 * 1024,
            max_write_bytes: 1024 * 1024,
            max_patch_bytes: 128 * 1024,
            max_list_entries: 512,
            max_search_matches: 100,
            max_search_files: 1_000,
            max_search_entries: 10_000,
            max_search_bytes: 8 * 1024 * 1024,
            max_search_line_bytes: 8 * 1024,
            max_search_query_bytes: 1024,
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
struct PatchTestAfterWriteHook {
    root: PathBuf,
    hook: fn(&Path),
    consumed: AtomicBool,
}

#[cfg(test)]
#[derive(Debug)]
struct TraceStartTestHook {
    tool_call_id: String,
    hook: fn(),
    consumed: AtomicBool,
}

#[cfg(test)]
impl PatchTestAfterWriteHook {
    fn new(root: PathBuf, hook: fn(&Path)) -> Self {
        Self {
            root,
            hook,
            consumed: AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
impl TraceStartTestHook {
    fn new(tool_call_id: String, hook: fn()) -> Self {
        Self {
            tool_call_id,
            hook,
            consumed: AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
fn install_patch_test_after_write_hook(root: PathBuf, hook: fn(&Path)) {
    PATCH_TEST_AFTER_WRITE_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("patch test hook mutex should not be poisoned")
        .replace(PatchTestAfterWriteHook::new(root, hook));
}

#[cfg(test)]
fn install_trace_start_test_hook(tool_call_id: &str, hook: fn()) {
    TRACE_START_TEST_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("trace start hook mutex should not be poisoned")
        .replace(TraceStartTestHook::new(tool_call_id.to_owned(), hook));
}

#[cfg(test)]
fn maybe_run_patch_test_after_write_hook(root: &Path) {
    let Some(hook_slot) = PATCH_TEST_AFTER_WRITE_HOOK.get() else {
        return;
    };
    let hook_guard = hook_slot
        .lock()
        .expect("patch test hook mutex should not be poisoned");
    let Some(hook) = hook_guard.as_ref() else {
        return;
    };
    if root != hook.root {
        return;
    }
    if hook.consumed.swap(true, Ordering::SeqCst) {
        return;
    }
    (hook.hook)(&hook.root);
}

#[cfg(test)]
fn maybe_run_trace_start_test_hook(tool_call_id: &str) {
    let Some(hook_slot) = TRACE_START_TEST_HOOK.get() else {
        return;
    };
    let hook_guard = hook_slot
        .lock()
        .expect("trace start hook mutex should not be poisoned");
    let Some(hook) = hook_guard.as_ref() else {
        return;
    };
    if hook.tool_call_id != tool_call_id {
        return;
    }
    if hook.consumed.swap(true, Ordering::SeqCst) {
        return;
    }
    (hook.hook)();
}

/// Configuration for workspace tools.
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

    /// Sets workspace tool limits.
    #[must_use]
    pub fn with_limits(mut self, limits: WorkspaceToolLimits) -> Self {
        self.limits = limits;
        self
    }
}

/// Errors raised while validating workspace tool configuration.
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

/// Errors raised while building a reusable workspace coding-loop profile.
#[derive(Debug, Error)]
pub enum WorkspaceCodingLoopProfileError {
    /// Workspace tool configuration was invalid.
    #[error(transparent)]
    WorkspaceTools {
        /// Source workspace tool configuration error.
        #[from]
        source: WorkspaceToolConfigError,
    },
    /// The process command tool could not be constructed.
    #[error(transparent)]
    ProcessTool {
        /// Source process command tool error.
        #[from]
        source: ProcessCommandToolError,
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

    /// Returns the registered read-only workspace tools.
    #[must_use]
    pub fn into_registered_tools(self) -> Vec<RegisteredTool> {
        vec![
            RegisteredTool::read_only(
                read_file_spec(),
                Arc::new(ReadFileExecutor {
                    state: Arc::clone(&self.state),
                }),
            ),
            RegisteredTool::read_only(
                list_dir_spec(),
                Arc::new(ListDirExecutor {
                    state: Arc::clone(&self.state),
                }),
            ),
            RegisteredTool::read_only(
                search_text_spec(),
                Arc::new(SearchTextExecutor { state: self.state }),
            ),
        ]
    }

    /// Returns the registered read-only workspace tools plus the opt-in patch tool.
    ///
    /// The patch tool is classified as [`ToolActionKind::WorkspaceWrite`], so
    /// current runtime default policy denies it before invoking the executor.
    #[must_use]
    pub fn into_registered_tools_with_patch(self) -> Vec<RegisteredTool> {
        let patch_state = Arc::clone(&self.state);
        let mut tools = self.into_registered_tools();
        tools.push(
            RegisteredTool::new(
                workspace_patch_spec(),
                Arc::new(WorkspacePatchExecutor { state: patch_state }),
                ToolActionKind::WorkspaceWrite,
            )
            .with_action_proposal(),
        );
        tools
    }
}

/// Reusable tool/profile registration for Merry's workspace coding loop.
///
/// This profile keeps upper layers from assembling the same workspace
/// read/search, optional patch, process tool, and process permission lanes by
/// hand. It does not change runtime policy by itself: patch support remains
/// opt-in through [`WorkspaceCodingLoopProfile::with_patch_tool`], and local
/// workspace process effects require an injected runner plus explicit CLI
/// bwrap admission through
/// [`WorkspaceCodingLoopProfile::with_cli_bwrap_process_runner`].
#[derive(Clone)]
pub struct WorkspaceCodingLoopProfile {
    workspace_tools: ReadOnlyWorkspaceTools,
    include_patch_tool: bool,
    process_runner: Option<(
        AcceptedLocalWorkspaceProcessAdmission,
        Arc<dyn ProcessRunner>,
    )>,
}

impl WorkspaceCodingLoopProfile {
    /// Validates workspace tool configuration and creates the reusable profile.
    pub fn new(config: WorkspaceToolsConfig) -> Result<Self, WorkspaceToolConfigError> {
        Ok(Self {
            workspace_tools: ReadOnlyWorkspaceTools::new(config)?,
            include_patch_tool: false,
            process_runner: None,
        })
    }

    /// Includes the constrained workspace patch tool and low-risk patch lane.
    #[must_use]
    pub fn with_patch_tool(mut self) -> Self {
        self.include_patch_tool = true;
        self
    }

    /// Includes process execution lanes for the declared CLI bubblewrap profile.
    #[must_use]
    pub fn with_cli_bwrap_process_runner(
        mut self,
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        self.process_runner = Some((admission, runner));
        self
    }

    /// Registers this profile on an existing runtime builder.
    ///
    /// The returned builder is not built yet, so callers can still add model
    /// providers or other runtime options around the reusable coding-loop
    /// profile.
    pub fn register_on(
        self,
        mut builder: RuntimeBuilder,
    ) -> Result<RuntimeBuilder, WorkspaceCodingLoopProfileError> {
        if let Some(summary) = self.workspace_tools.project_capability_summary() {
            builder = builder.initial_context_summary(PROJECT_CAPABILITY_CONTEXT_ID, &summary);
        }

        if self.include_patch_tool {
            builder = builder.allow_low_risk_workspace_patches();
        }

        if let Some((admission, runner)) = self.process_runner {
            builder = builder
                .allow_low_risk_process_actions(Arc::clone(&runner))
                .allow_accepted_local_workspace_process_actions(admission, runner)
                .register_tool(process_command_tool(
                    ToolName::new(CODING_LOOP_PROCESS_TOOL).expect("static tool name is valid"),
                    "Run exact argv through Merry process policy for workspace inspection and verification.",
                )?);
        }

        let tools = if self.include_patch_tool {
            self.workspace_tools.into_registered_tools_with_patch()
        } else {
            self.workspace_tools.into_registered_tools()
        };
        for tool in tools {
            builder = builder.register_tool(tool);
        }

        Ok(builder)
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

        validate_limits(&config.limits)?;

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

    fn project_capability_summary(&self) -> Option<String> {
        self.roots
            .iter()
            .find_map(|root| project_capability_summary_for_root(root))
    }
}

impl ReadOnlyWorkspaceTools {
    fn project_capability_summary(&self) -> Option<String> {
        self.state.project_capability_summary()
    }
}

fn validate_limits(limits: &WorkspaceToolLimits) -> Result<(), WorkspaceToolConfigError> {
    for (name, value) in [
        ("max_read_bytes", limits.max_read_bytes),
        ("max_write_bytes", limits.max_write_bytes),
        ("max_patch_bytes", limits.max_patch_bytes),
        ("max_list_entries", limits.max_list_entries),
        ("max_search_matches", limits.max_search_matches),
        ("max_search_files", limits.max_search_files),
        ("max_search_entries", limits.max_search_entries),
        ("max_search_bytes", limits.max_search_bytes),
        ("max_search_line_bytes", limits.max_search_line_bytes),
        ("max_search_query_bytes", limits.max_search_query_bytes),
    ] {
        if value == 0 {
            return Err(WorkspaceToolConfigError::InvalidLimit { name });
        }
    }

    Ok(())
}

fn project_capability_summary_for_root(root: &Path) -> Option<String> {
    let mut lines = Vec::new();
    let mut checks = Vec::new();

    if root.join("Cargo.toml").is_file() {
        lines.push(
            "Detected Rust project metadata: Cargo.toml is present at the workspace root."
                .to_owned(),
        );
        checks.push("cargo fmt --all --check");
        checks.push("cargo clippy --all-targets --all-features -- -D warnings");
        checks.push("cargo test --all");
    }

    if root.join("justfile").is_file() || root.join("Justfile").is_file() {
        lines.push(
            "Detected justfile; prefer project-provided just tasks when AGENTS.md or user instructions name them."
                .to_owned(),
        );
    }

    if root.join("package.json").is_file() {
        lines.push(
            "Detected JavaScript/TypeScript project metadata: package.json is present.".to_owned(),
        );
    }

    if root.join("pyproject.toml").is_file() {
        lines.push("Detected Python project metadata: pyproject.toml is present.".to_owned());
    }

    if root.join("AGENTS.md").is_file() {
        lines.push(
            "Detected AGENTS.md at the workspace root; read and follow it as project-specific instructions before substantial work."
                .to_owned(),
        );
    }

    if !checks.is_empty() {
        lines.push(format!(
            "Default Rust verification candidates if not overridden by AGENTS.md or the user: {}.",
            checks.join("; ")
        ));
    }

    if lines.is_empty() {
        return None;
    }

    Some(lines.join("\n"))
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
                    return Ok(invalid_arguments_outcome(
                        WORKSPACE_READ_FILE_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_path = WorkspaceTracePath::new(path.as_str());
            trace_workspace_tool_start(
                WORKSPACE_READ_FILE_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Path(trace_path.as_ref()),
            );

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let handle = tokio::task::spawn_blocking(move || read_file_blocking(&state, path));

            tokio::select! {
                biased;
                () = token.cancelled() => {
                    trace_workspace_tool_finish(
                        WORKSPACE_READ_FILE_TOOL,
                        call.id().as_str(),
                        WorkspaceTraceTarget::Path(trace_path.as_ref()),
                        WorkspaceTraceFinish::Cancelled,
                    );
                    Err(ToolExecutionError::Cancelled)
                }
                joined = handle => match joined {
                    Ok(outcome) => {
                        if token.is_cancelled() {
                            trace_workspace_tool_finish(
                                WORKSPACE_READ_FILE_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                                WorkspaceTraceFinish::Cancelled,
                            );
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            trace_workspace_tool_finish(
                                WORKSPACE_READ_FILE_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                                WorkspaceTraceFinish::Outcome(&outcome),
                            );
                            Ok(outcome)
                        }
                    }
                    Err(error) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_READ_FILE_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Path(trace_path.as_ref()),
                            WorkspaceTraceFinish::Error("workspace_infrastructure_error"),
                        );
                        Err(ToolExecutionError::infrastructure(format!(
                            "workspace read task failed to join: {error}"
                        )))
                    }
                },
            }
        })
    }
}

#[derive(Debug)]
struct ListDirExecutor {
    state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for ListDirExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let path = match parse_list_dir_args(&call) {
                Ok(args) => args.path,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        WORKSPACE_LIST_DIR_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_path = WorkspaceTracePath::new(path.as_str());
            trace_workspace_tool_start(
                WORKSPACE_LIST_DIR_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Path(trace_path.as_ref()),
            );

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let worker_token = token.clone();
            let handle = tokio::task::spawn_blocking(move || {
                let is_cancelled = || worker_token.is_cancelled();
                list_dir_blocking_checked(&state, path, &is_cancelled)
            });

            tokio::select! {
                biased;
                () = token.cancelled() => {
                    trace_workspace_tool_finish(
                        WORKSPACE_LIST_DIR_TOOL,
                        call.id().as_str(),
                        WorkspaceTraceTarget::Path(trace_path.as_ref()),
                        WorkspaceTraceFinish::Cancelled,
                    );
                    Err(ToolExecutionError::Cancelled)
                }
                joined = handle => match joined {
                    Ok(Ok(outcome)) => {
                        if token.is_cancelled() {
                            trace_workspace_tool_finish(
                                WORKSPACE_LIST_DIR_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                                WorkspaceTraceFinish::Cancelled,
                            );
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            trace_workspace_tool_finish(
                                WORKSPACE_LIST_DIR_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                                WorkspaceTraceFinish::Outcome(&outcome),
                            );
                            Ok(outcome)
                        }
                    }
                    Ok(Err(error)) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_LIST_DIR_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Path(trace_path.as_ref()),
                            WorkspaceTraceFinish::from_error(&error),
                        );
                        Err(error)
                    }
                    Err(error) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_LIST_DIR_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Path(trace_path.as_ref()),
                            WorkspaceTraceFinish::Error("workspace_infrastructure_error"),
                        );
                        Err(ToolExecutionError::infrastructure(format!(
                            "workspace list task failed to join: {error}"
                        )))
                    }
                },
            }
        })
    }
}

#[derive(Debug)]
struct SearchTextExecutor {
    state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for SearchTextExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let args = match parse_search_text_args(&call) {
                Ok(args) => args,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        WORKSPACE_SEARCH_TEXT_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_path = args.path.as_deref().map(WorkspaceTracePath::new);
            let trace_query_bytes = args.query.len();
            trace_workspace_tool_start(
                WORKSPACE_SEARCH_TEXT_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Search {
                    path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                    query_bytes: trace_query_bytes,
                },
            );

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let worker_token = token.clone();
            let handle = tokio::task::spawn_blocking(move || {
                let is_cancelled = || worker_token.is_cancelled();
                search_text_blocking_checked(&state, args, &is_cancelled)
            });

            tokio::select! {
                biased;
                () = token.cancelled() => {
                    trace_workspace_tool_finish(
                        WORKSPACE_SEARCH_TEXT_TOOL,
                        call.id().as_str(),
                        WorkspaceTraceTarget::Search {
                            path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                            query_bytes: trace_query_bytes,
                        },
                        WorkspaceTraceFinish::Cancelled,
                    );
                    Err(ToolExecutionError::Cancelled)
                }
                joined = handle => match joined {
                    Ok(Ok(outcome)) => {
                        if token.is_cancelled() {
                            trace_workspace_tool_finish(
                                WORKSPACE_SEARCH_TEXT_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Search {
                                    path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                                    query_bytes: trace_query_bytes,
                                },
                                WorkspaceTraceFinish::Cancelled,
                            );
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            trace_workspace_tool_finish(
                                WORKSPACE_SEARCH_TEXT_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Search {
                                    path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                                    query_bytes: trace_query_bytes,
                                },
                                WorkspaceTraceFinish::Outcome(&outcome),
                            );
                            Ok(outcome)
                        }
                    }
                    Ok(Err(error)) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_SEARCH_TEXT_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Search {
                                path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                                query_bytes: trace_query_bytes,
                            },
                            WorkspaceTraceFinish::from_error(&error),
                        );
                        Err(error)
                    }
                    Err(error) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_SEARCH_TEXT_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Search {
                                path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                                query_bytes: trace_query_bytes,
                            },
                            WorkspaceTraceFinish::Error("workspace_infrastructure_error"),
                        );
                        Err(ToolExecutionError::infrastructure(format!(
                            "workspace search task failed to join: {error}"
                        )))
                    }
                },
            }
        })
    }
}

#[derive(Debug)]
struct WorkspacePatchExecutor {
    state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for WorkspacePatchExecutor {
    fn propose<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let args = match parse_workspace_patch_args(&call) {
                Ok(args) => args,
                Err(_) => return Ok(None),
            };

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let worker_token = token.clone();
            let handle = tokio::task::spawn_blocking(move || {
                let is_cancelled = || worker_token.is_cancelled();
                propose_workspace_patch_blocking_checked(&state, args, &call, &is_cancelled)
            });

            tokio::select! {
                biased;
                () = token.cancelled() => Err(ToolExecutionError::Cancelled),
                joined = handle => match joined {
                    Ok(Ok(proposal)) => {
                        if token.is_cancelled() {
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            Ok(proposal)
                        }
                    }
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(ToolExecutionError::infrastructure(format!(
                        "workspace patch proposal task failed to join: {error}"
                    ))),
                },
            }
        })
    }

    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let args = match parse_workspace_patch_args(&call) {
                Ok(args) => args,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        WORKSPACE_PATCH_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_patch_bytes = args.patch.len();
            trace_workspace_tool_start(
                WORKSPACE_PATCH_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Patch {
                    patch_bytes: trace_patch_bytes,
                },
            );

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let worker_token = token.clone();
            let approved_proposal = context.approved_workspace_patch().cloned();
            let handle = tokio::task::spawn_blocking(move || {
                let is_cancelled = || worker_token.is_cancelled();
                workspace_patch_blocking_checked(
                    &state,
                    args,
                    approved_proposal.as_ref(),
                    &is_cancelled,
                )
            });

            tokio::select! {
                biased;
                joined = handle => match joined {
                    Ok(Ok(outcome)) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_PATCH_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Patch {
                                patch_bytes: trace_patch_bytes,
                            },
                            WorkspaceTraceFinish::Outcome(&outcome),
                        );
                        Ok(outcome)
                    }
                    Ok(Err(error)) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_PATCH_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Patch {
                                patch_bytes: trace_patch_bytes,
                            },
                            WorkspaceTraceFinish::from_error(&error),
                        );
                        Err(error)
                    }
                    Err(error) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_PATCH_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Patch {
                                patch_bytes: trace_patch_bytes,
                            },
                            WorkspaceTraceFinish::Error("workspace_infrastructure_error"),
                        );
                        Err(ToolExecutionError::infrastructure(format!(
                            "workspace patch task failed to join: {error}"
                        )))
                    }
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListDirArgs {
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchTextArgs {
    query: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_matches: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorkspacePatchArgs {
    patch: String,
}

enum WorkspaceTraceTarget<'a> {
    ToolOnly,
    Path(&'a str),
    Search {
        path: Option<&'a str>,
        query_bytes: usize,
    },
    Patch {
        patch_bytes: usize,
    },
}

#[derive(Clone, Debug)]
struct WorkspaceTracePath {
    summary: String,
}

impl WorkspaceTracePath {
    fn new(path: &str) -> Self {
        Self {
            summary: bounded_trace_text(path, TRACE_PATH_MAX_CHARS),
        }
    }

    fn as_str(&self) -> &str {
        &self.summary
    }
}

impl AsRef<str> for WorkspaceTracePath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Clone, Copy)]
enum WorkspaceTraceFinish<'a> {
    Outcome(&'a ToolExecutionOutcome),
    Cancelled,
    Error(&'static str),
}

impl<'a> WorkspaceTraceFinish<'a> {
    fn from_error(error: &ToolExecutionError) -> Self {
        match error {
            ToolExecutionError::Cancelled => Self::Cancelled,
            ToolExecutionError::Infrastructure { .. } => {
                Self::Error("workspace_infrastructure_error")
            }
        }
    }

    fn status(self) -> &'static str {
        match self {
            Self::Outcome(outcome) => tool_outcome_status(outcome),
            Self::Cancelled => "cancelled",
            Self::Error(_) => "error",
        }
    }

    fn diagnostic_code(self) -> Option<&'a str> {
        match self {
            Self::Outcome(outcome) => outcome.diagnostic().map(ErrorInfo::code),
            Self::Cancelled => Some("workspace_tool_cancelled"),
            Self::Error(code) => Some(code),
        }
    }

    fn output_bytes(self) -> Option<usize> {
        match self {
            Self::Outcome(outcome) => Some(outcome.content().as_bytes().len()),
            Self::Cancelled | Self::Error(_) => None,
        }
    }
}

fn trace_workspace_tool_start(
    tool_name: &'static str,
    tool_call_id: &str,
    target: WorkspaceTraceTarget<'_>,
) {
    match target {
        WorkspaceTraceTarget::ToolOnly => {
            tracing::info!(
                event = "runtime.workspace_tool.start",
                tool_name,
                tool_call_id,
                "workspace tool start"
            );
        }
        WorkspaceTraceTarget::Path(path) => {
            tracing::info!(
                event = "runtime.workspace_tool.start",
                tool_name,
                tool_call_id,
                path,
                "workspace tool start"
            );
        }
        WorkspaceTraceTarget::Search { path, query_bytes } => {
            tracing::info!(
                event = "runtime.workspace_tool.start",
                tool_name,
                tool_call_id,
                path,
                query_bytes,
                "workspace tool start"
            );
        }
        WorkspaceTraceTarget::Patch { patch_bytes } => {
            tracing::info!(
                event = "runtime.workspace_tool.start",
                tool_name,
                tool_call_id,
                patch_bytes,
                "workspace tool start"
            );
        }
    }
    #[cfg(test)]
    maybe_run_trace_start_test_hook(tool_call_id);
}

fn trace_workspace_tool_finish(
    tool_name: &'static str,
    tool_call_id: &str,
    target: WorkspaceTraceTarget<'_>,
    finish: WorkspaceTraceFinish<'_>,
) {
    let status = finish.status();
    let diagnostic_code = finish.diagnostic_code();
    let output_bytes = finish.output_bytes();
    match target {
        WorkspaceTraceTarget::ToolOnly => {
            tracing::info!(
                event = "runtime.workspace_tool.finish",
                tool_name,
                tool_call_id,
                status,
                diagnostic_code,
                output_bytes,
                "workspace tool finish"
            );
        }
        WorkspaceTraceTarget::Path(path) => {
            tracing::info!(
                event = "runtime.workspace_tool.finish",
                tool_name,
                tool_call_id,
                path,
                status,
                diagnostic_code,
                output_bytes,
                "workspace tool finish"
            );
        }
        WorkspaceTraceTarget::Search { path, query_bytes } => {
            tracing::info!(
                event = "runtime.workspace_tool.finish",
                tool_name,
                tool_call_id,
                path,
                query_bytes,
                status,
                diagnostic_code,
                output_bytes,
                "workspace tool finish"
            );
        }
        WorkspaceTraceTarget::Patch { patch_bytes } => {
            tracing::info!(
                event = "runtime.workspace_tool.finish",
                tool_name,
                tool_call_id,
                patch_bytes,
                status,
                diagnostic_code,
                output_bytes,
                "workspace tool finish"
            );
        }
    }
}

fn tool_outcome_status(outcome: &ToolExecutionOutcome) -> &'static str {
    match outcome.status() {
        ToolCallResultStatus::Succeeded => "succeeded",
        ToolCallResultStatus::Failed => "failed",
    }
}

fn invalid_arguments_outcome(
    tool_name: &'static str,
    tool_call_id: &str,
    message: String,
) -> ToolExecutionOutcome {
    let outcome = failed_outcome(tool_name, ERROR_INVALID_ARGUMENTS, message, None::<String>);
    trace_workspace_tool_start(tool_name, tool_call_id, WorkspaceTraceTarget::ToolOnly);
    trace_workspace_tool_finish(
        tool_name,
        tool_call_id,
        WorkspaceTraceTarget::ToolOnly,
        WorkspaceTraceFinish::Outcome(&outcome),
    );
    outcome
}

fn bounded_trace_text(value: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut truncated = false;
    for character in value.chars().take(max_chars) {
        output.push(character);
    }
    if value.chars().count() > max_chars {
        truncated = true;
    }
    if truncated {
        output.push_str("...");
    }
    output
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
struct ListDirSuccess<'a> {
    ok: bool,
    tool: &'static str,
    path: &'a str,
    entries: Vec<ListDirEntry>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct ListDirEntry {
    name: String,
    path: String,
    kind: EntryKind,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Serialize)]
struct SearchTextSuccess<'a> {
    ok: bool,
    tool: &'static str,
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    matches: Vec<SearchMatch>,
    searched_files: usize,
    skipped: SearchSkipCounts,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct WorkspacePatchSuccess {
    ok: bool,
    tool: &'static str,
    changes: Vec<WorkspacePatchSuccessChange>,
}

#[derive(Debug, Serialize)]
struct WorkspacePatchSuccessChange {
    path: String,
    hunks: usize,
    bytes_before: usize,
    bytes_after: usize,
}

#[derive(Debug, Serialize)]
struct SearchMatch {
    path: String,
    line_number: usize,
    line: String,
    truncated: bool,
}

#[derive(Debug, Default, Serialize)]
struct SearchSkipCounts {
    hidden: usize,
    symlink: usize,
    non_utf8: usize,
    too_large: usize,
    read_failed: usize,
}

#[derive(Debug, Serialize)]
struct FailureEnvelope<'a> {
    ok: bool,
    tool: &'static str,
    error: FailureError<'a>,
    recovery: FailureRecovery,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct FailureError<'a> {
    code: &'a str,
    message: &'a str,
}

#[derive(Debug, Serialize)]
struct FailureRecovery {
    path_contract: &'static str,
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

fn list_dir_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_LIST_DIR_TOOL).expect("static workspace tool name is valid"),
        "List one directory under configured stable workspace roots as a non-recursive, stable, memory-bounded, cancellable listing without symlink traversal.",
        ToolInputSchema::new(schema_for!(ListDirArgs))
            .expect("static workspace_list_dir input schema is valid"),
    )
    .expect("static workspace_list_dir spec is valid")
}

fn search_text_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_SEARCH_TEXT_TOOL).expect("static workspace tool name is valid"),
        "Search UTF-8 files under configured stable workspace roots with literal, case-sensitive matching and bounded traversal, entry inspection, and scanned bytes.",
        ToolInputSchema::new(schema_for!(SearchTextArgs))
            .expect("static workspace_search_text input schema is valid"),
    )
    .expect("static workspace_search_text spec is valid")
}

fn workspace_patch_spec() -> ToolSpec {
    ToolSpec::new(
        ToolName::new(WORKSPACE_PATCH_TOOL).expect("static workspace tool name is valid"),
        "Apply one Merry workspace patch set to UTF-8 files under configured stable workspace roots. Use workspace-relative paths in *** Update File: ... headers. Prefer the smallest unique hunk context needed for a localized edit; do not submit whole-file content for small edits.",
        ToolInputSchema::new(schema_for!(WorkspacePatchArgs))
            .expect("static workspace_patch input schema is valid"),
    )
    .expect("static workspace_patch spec is valid")
}

fn parse_read_file_args(call: &PendingToolCall) -> Result<ReadFileArgs, String> {
    parse_tool_args(call, WORKSPACE_READ_FILE_TOOL)
}

fn parse_list_dir_args(call: &PendingToolCall) -> Result<ListDirArgs, String> {
    parse_tool_args(call, WORKSPACE_LIST_DIR_TOOL)
}

fn parse_search_text_args(call: &PendingToolCall) -> Result<SearchTextArgs, String> {
    parse_tool_args(call, WORKSPACE_SEARCH_TEXT_TOOL)
}

fn parse_workspace_patch_args(call: &PendingToolCall) -> Result<WorkspacePatchArgs, String> {
    parse_tool_args(call, WORKSPACE_PATCH_TOOL)
}

fn parse_tool_args<T>(call: &PendingToolCall, tool_name: &'static str) -> Result<T, String>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::Object(
        call.arguments().as_object().clone(),
    ))
    .map_err(|error| format!("invalid {tool_name} arguments: {error}"))
}

fn read_file_blocking(state: &WorkspaceToolState, path: String) -> ToolExecutionOutcome {
    let relative = match validate_relative_path(&path, state.allow_hidden) {
        Ok(relative) => relative,
        Err(error) => {
            return failed_outcome(
                WORKSPACE_READ_FILE_TOOL,
                error.code,
                error.message,
                error.path,
            );
        }
    };

    for root in &state.roots {
        match resolve_existing_path(root, &relative) {
            Ok(Some(resolved)) => {
                return match read_resolved_file(&relative, &resolved.path, &state.limits) {
                    Ok(success) => success,
                    Err(error) => failed_outcome(
                        WORKSPACE_READ_FILE_TOOL,
                        error.code,
                        error.message,
                        Some(relative.display),
                    ),
                };
            }
            Ok(None) => {}
            Err(error) => {
                return failed_outcome(
                    WORKSPACE_READ_FILE_TOOL,
                    error.code,
                    error.message,
                    Some(relative.display),
                );
            }
        }
    }

    failed_outcome(
        WORKSPACE_READ_FILE_TOOL,
        ERROR_FILE_NOT_FOUND,
        "workspace file was not found",
        Some(relative.display),
    )
}

fn read_resolved_file(
    relative: &ValidatedRelativePath,
    path: &Path,
    limits: &WorkspaceToolLimits,
) -> Result<ToolExecutionOutcome, DomainError> {
    let mut file = open_file_for_read(path)?;
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

    if metadata.len() > limits.max_read_bytes as u64 {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        ));
    }

    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limits.max_read_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;

    if bytes.len() > limits.max_read_bytes {
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
    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace read success envelope serializes"),
    ))
}

#[cfg(test)]
fn list_dir_blocking(state: &WorkspaceToolState, path: String) -> ToolExecutionOutcome {
    list_dir_blocking_checked(state, path, &|| false)
        .expect("uncancelled workspace list should not return cancellation")
}

fn list_dir_blocking_checked(
    state: &WorkspaceToolState,
    path: String,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    let relative = match validate_relative_path_or_root(&path, state.allow_hidden) {
        Ok(relative) => relative,
        Err(error) => {
            return Ok(failed_outcome(
                WORKSPACE_LIST_DIR_TOOL,
                error.code,
                error.message,
                error.path,
            ));
        }
    };

    for root in &state.roots {
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }

        match resolve_existing_path(root, &relative) {
            Ok(Some(resolved)) => {
                return match list_resolved_dir(&relative, &resolved.path, state, is_cancelled) {
                    Ok(success) => Ok(success),
                    Err(BlockingToolError::Domain(error)) => Ok(failed_outcome(
                        WORKSPACE_LIST_DIR_TOOL,
                        error.code,
                        error.message,
                        Some(relative.display),
                    )),
                    Err(BlockingToolError::Cancelled) => Err(ToolExecutionError::Cancelled),
                };
            }
            Ok(None) => {}
            Err(error) => {
                return Ok(failed_outcome(
                    WORKSPACE_LIST_DIR_TOOL,
                    error.code,
                    error.message,
                    Some(relative.display),
                ));
            }
        }
    }

    Ok(failed_outcome(
        WORKSPACE_LIST_DIR_TOOL,
        ERROR_PATH_NOT_FOUND,
        "workspace directory was not found",
        Some(relative.display),
    ))
}

fn list_resolved_dir(
    relative: &ValidatedRelativePath,
    path: &Path,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace directory metadata",
        )
    })?;
    if !metadata.is_dir() {
        return Err(
            DomainError::new(ERROR_NOT_DIRECTORY, "workspace path is not a directory").into(),
        );
    }

    let mut entries = Vec::new();
    let read_dir = fs::read_dir(path)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace directory"))?;
    for entry in read_dir {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }

        let entry = entry.map_err(|_| {
            DomainError::new(
                ERROR_READ_FAILED,
                "could not read workspace directory entry",
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !state.allow_hidden && is_hidden_name(&name) {
            continue;
        }

        let file_type = entry.file_type().map_err(|_| {
            DomainError::new(
                ERROR_READ_FAILED,
                "could not inspect workspace directory entry",
            )
        })?;
        let entry_path = join_display_path(&relative.display, &name);
        push_bounded_list_entry(
            &mut entries,
            ListDirEntry {
                name,
                path: entry_path,
                kind: entry_kind(file_type),
            },
            state.limits.max_list_entries,
        );
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let truncated = entries.len() > state.limits.max_list_entries;
    entries.truncate(state.limits.max_list_entries);

    let payload = ListDirSuccess {
        ok: true,
        tool: WORKSPACE_LIST_DIR_TOOL,
        path: &relative.display,
        entries,
        truncated,
    };
    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace list success envelope serializes"),
    ))
}

fn push_bounded_list_entry(
    entries: &mut Vec<ListDirEntry>,
    entry: ListDirEntry,
    max_entries: usize,
) {
    let retention_limit = max_entries.saturating_add(1);
    if entries.len() < retention_limit {
        entries.push(entry);
        return;
    }

    let Some(current_last) = entries
        .iter()
        .max_by(|left, right| left.name.cmp(&right.name))
    else {
        entries.push(entry);
        return;
    };

    if entry.name >= current_last.name {
        return;
    }

    let current_last_name = current_last.name.clone();
    if let Some(index) = entries
        .iter()
        .position(|candidate| candidate.name == current_last_name)
    {
        entries[index] = entry;
    }
}

fn entry_kind(file_type: fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

#[cfg(test)]
fn search_text_blocking(state: &WorkspaceToolState, args: SearchTextArgs) -> ToolExecutionOutcome {
    search_text_blocking_checked(state, args, &|| false)
        .expect("uncancelled workspace search should not return cancellation")
}

fn search_text_blocking_checked(
    state: &WorkspaceToolState,
    args: SearchTextArgs,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    if args.query.is_empty() {
        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace search query must not be empty",
            None::<String>,
        ));
    }
    if args.query.len() > state.limits.max_search_query_bytes {
        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace search query exceeds the configured byte limit",
            None::<String>,
        ));
    }
    if args.query.chars().any(char::is_control) {
        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace search query must be a single line without control characters",
            None::<String>,
        ));
    }

    let max_matches = args
        .max_matches
        .unwrap_or(state.limits.max_search_matches)
        .min(state.limits.max_search_matches);
    if max_matches == 0 {
        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace search max_matches must be greater than zero",
            None::<String>,
        ));
    }

    let mut search = SearchRun::new(&args.query, max_matches, &state.limits, state.allow_hidden);

    if let Some(path) = args.path {
        let relative = match validate_relative_path_or_root(&path, state.allow_hidden) {
            Ok(relative) => relative,
            Err(error) => {
                return Ok(failed_outcome(
                    WORKSPACE_SEARCH_TEXT_TOOL,
                    error.code,
                    error.message,
                    error.path,
                ));
            }
        };

        let display = relative.display.clone();
        for root in &state.roots {
            if is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            match resolve_existing_path(root, &relative) {
                Ok(Some(resolved)) => {
                    return match search_resolved_path(
                        &mut search,
                        &resolved.path,
                        &display,
                        is_cancelled,
                    ) {
                        Ok(()) => Ok(search_success(&args.query, Some(display), search)),
                        Err(BlockingToolError::Domain(error)) => Ok(failed_outcome(
                            WORKSPACE_SEARCH_TEXT_TOOL,
                            error.code,
                            error.message,
                            Some(relative.display),
                        )),
                        Err(BlockingToolError::Cancelled) => Err(ToolExecutionError::Cancelled),
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    return Ok(failed_outcome(
                        WORKSPACE_SEARCH_TEXT_TOOL,
                        error.code,
                        error.message,
                        Some(relative.display),
                    ));
                }
            }
        }

        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_PATH_NOT_FOUND,
            "workspace search path was not found",
            Some(relative.display),
        ));
    }

    for root in &state.roots {
        if search.is_done() {
            break;
        }
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }
        search_directory(&mut search, root, ".", is_cancelled)
            .map_err(blocking_tool_error_into_execution)?;
    }

    Ok(search_success(&args.query, None, search))
}

#[derive(Debug)]
struct WorkspacePatch {
    files: Vec<WorkspacePatchFile>,
}

#[derive(Debug)]
struct WorkspacePatchFile {
    path: String,
    operation: WorkspacePatchOperation,
}

#[derive(Debug)]
enum WorkspacePatchOperation {
    Update { hunks: Vec<WorkspacePatchHunk> },
}

#[derive(Debug)]
struct WorkspacePatchHunk {
    lines: Vec<WorkspacePatchLine>,
}

impl WorkspacePatchHunk {
    fn has_edit(&self) -> bool {
        self.lines.iter().any(|line| {
            matches!(
                line,
                WorkspacePatchLine::Remove(_) | WorkspacePatchLine::Add(_)
            )
        })
    }

    fn old_text(&self, trailing_newline: bool) -> String {
        collect_patch_hunk_text(
            self.lines.iter().filter_map(|line| match line {
                WorkspacePatchLine::Context(text) | WorkspacePatchLine::Remove(text) => Some(text),
                WorkspacePatchLine::Add(_) => None,
            }),
            trailing_newline,
        )
    }

    fn new_text(&self, trailing_newline: bool) -> String {
        collect_patch_hunk_text(
            self.lines.iter().filter_map(|line| match line {
                WorkspacePatchLine::Context(text) | WorkspacePatchLine::Add(text) => Some(text),
                WorkspacePatchLine::Remove(_) => None,
            }),
            trailing_newline,
        )
    }
}

#[derive(Debug)]
enum WorkspacePatchLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug)]
struct WorkspacePatchParseError {
    message: &'static str,
    path: Option<String>,
}

impl WorkspacePatchParseError {
    fn new(message: &'static str, path: Option<String>) -> Self {
        Self { message, path }
    }
}

fn parse_workspace_patch(raw_patch: &str) -> Result<WorkspacePatch, WorkspacePatchParseError> {
    const BEGIN: &str = "*** Begin Workspace Patch";
    const END: &str = "*** End Workspace Patch";
    const UPDATE_PREFIX: &str = "*** Update File: ";

    let raw_patch = raw_patch.strip_prefix('\u{feff}').unwrap_or(raw_patch);
    let lines = raw_patch.lines().collect::<Vec<_>>();
    let mut index = 0;
    skip_blank_patch_lines(&lines, &mut index);

    if patch_line(lines.get(index).copied()) != Some(BEGIN) {
        return Err(WorkspacePatchParseError::new(
            "workspace patch must start with *** Begin Workspace Patch",
            None,
        ));
    }
    index += 1;

    let mut files = Vec::new();
    let mut seen_paths = BTreeSet::new();
    loop {
        skip_blank_patch_lines(&lines, &mut index);
        let Some(line) = patch_line(lines.get(index).copied()) else {
            return Err(WorkspacePatchParseError::new(
                "workspace patch must end with *** End Workspace Patch",
                None,
            ));
        };
        if line == END {
            index += 1;
            skip_blank_patch_lines(&lines, &mut index);
            if index != lines.len() {
                return Err(WorkspacePatchParseError::new(
                    "workspace patch must not contain text after *** End Workspace Patch",
                    None,
                ));
            }
            break;
        }

        let Some(path) = line.strip_prefix(UPDATE_PREFIX).map(str::trim) else {
            return Err(WorkspacePatchParseError::new(
                "workspace patch expected *** Update File: <path>",
                None,
            ));
        };
        if path.is_empty() {
            return Err(WorkspacePatchParseError::new(
                "workspace patch update path must not be empty",
                None,
            ));
        }
        let path = path.to_owned();
        if !seen_paths.insert(path.clone()) {
            return Err(WorkspacePatchParseError::new(
                "workspace patch must not update the same file more than once",
                Some(path),
            ));
        }
        index += 1;

        let hunks = parse_workspace_patch_update_hunks(&lines, &mut index, &path)?;
        files.push(WorkspacePatchFile {
            path,
            operation: WorkspacePatchOperation::Update { hunks },
        });
    }

    if files.is_empty() {
        return Err(WorkspacePatchParseError::new(
            "workspace patch must contain at least one file update",
            None,
        ));
    }

    Ok(WorkspacePatch { files })
}

fn parse_workspace_patch_update_hunks(
    lines: &[&str],
    index: &mut usize,
    path: &str,
) -> Result<Vec<WorkspacePatchHunk>, WorkspacePatchParseError> {
    const END: &str = "*** End Workspace Patch";
    const UPDATE_PREFIX: &str = "*** Update File: ";

    let mut hunks = Vec::new();
    let mut current = Vec::new();
    while let Some(line) = patch_line(lines.get(*index).copied()) {
        if line == END || line.starts_with(UPDATE_PREFIX) {
            break;
        }
        if line.trim().is_empty() && current.is_empty() {
            *index += 1;
            continue;
        }
        if line.starts_with("@@") {
            push_workspace_patch_hunk(&mut hunks, &mut current, path)?;
            *index += 1;
            continue;
        }
        let Some((prefix, text)) = line.split_at_checked(1) else {
            return Err(WorkspacePatchParseError::new(
                "workspace patch hunk line must start with space, +, or -",
                Some(path.to_owned()),
            ));
        };
        match prefix {
            " " => current.push(WorkspacePatchLine::Context(text.to_owned())),
            "-" => current.push(WorkspacePatchLine::Remove(text.to_owned())),
            "+" => current.push(WorkspacePatchLine::Add(text.to_owned())),
            _ => {
                return Err(WorkspacePatchParseError::new(
                    "workspace patch hunk line must start with space, +, or -",
                    Some(path.to_owned()),
                ));
            }
        }
        *index += 1;
    }
    push_workspace_patch_hunk(&mut hunks, &mut current, path)?;

    if hunks.is_empty() {
        return Err(WorkspacePatchParseError::new(
            "workspace patch update must contain at least one hunk",
            Some(path.to_owned()),
        ));
    }
    Ok(hunks)
}

fn push_workspace_patch_hunk(
    hunks: &mut Vec<WorkspacePatchHunk>,
    current: &mut Vec<WorkspacePatchLine>,
    path: &str,
) -> Result<(), WorkspacePatchParseError> {
    if current.is_empty() {
        return Ok(());
    }
    let hunk = WorkspacePatchHunk {
        lines: std::mem::take(current),
    };
    if !hunk.has_edit() {
        return Err(WorkspacePatchParseError::new(
            "workspace patch hunk must add or remove at least one line",
            Some(path.to_owned()),
        ));
    }
    hunks.push(hunk);
    Ok(())
}

fn patch_line(line: Option<&str>) -> Option<&str> {
    line.map(|line| line.strip_suffix('\r').unwrap_or(line))
}

fn skip_blank_patch_lines(lines: &[&str], index: &mut usize) {
    while matches!(patch_line(lines.get(*index).copied()), Some(line) if line.trim().is_empty()) {
        *index += 1;
    }
}

#[cfg(test)]
fn workspace_patch_blocking(
    state: &WorkspaceToolState,
    args: WorkspacePatchArgs,
) -> ToolExecutionOutcome {
    workspace_patch_blocking_checked(state, args, None, &|| false)
        .expect("uncancelled workspace patch should not return cancellation")
}

fn propose_workspace_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: WorkspacePatchArgs,
    call: &PendingToolCall,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<ActionProposal>, ToolExecutionError> {
    match plan_workspace_patch_blocking_checked(state, args, is_cancelled) {
        Ok(WorkspacePatchPlanOutcome::Planned(plan)) => {
            let changes = plan
                .changes
                .iter()
                .map(WorkspacePatchFilePlan::change_evidence)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            let patch = WorkspacePatchProposal::from_changes(changes)
                .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            let subject = plan.subject();
            let proposal = ActionProposal::new(
                call,
                ToolActionKind::WorkspaceWrite,
                "workspace patch",
                subject.clone(),
                plan.summary(),
                ActionProposalEvidence::WorkspacePatch(patch),
            )
            .map_err(|error| ToolExecutionError::infrastructure(error.to_string()))?;
            Ok(Some(proposal))
        }
        Ok(WorkspacePatchPlanOutcome::Failure(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

fn workspace_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: WorkspacePatchArgs,
    approved_proposal: Option<&WorkspacePatchProposal>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    match plan_workspace_patch_blocking_checked(state, args, is_cancelled)? {
        WorkspacePatchPlanOutcome::Planned(plan) => {
            if let Some(approved) = approved_proposal {
                if match_approved_patch_proposal(approved, &plan).is_err() {
                    return Ok(proposal_mismatch_outcome(plan.subject()));
                }
            }
            execute_workspace_patch_plan(plan, is_cancelled)
        }
        WorkspacePatchPlanOutcome::Failure(outcome) => Ok(outcome),
    }
}

fn plan_workspace_patch_blocking_checked(
    state: &WorkspaceToolState,
    args: WorkspacePatchArgs,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchPlanOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    if args.patch.trim().is_empty() {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch must not be empty",
            None::<String>,
        )));
    }

    if args.patch.contains('\0') {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch must not contain NUL bytes",
            None::<String>,
        )));
    }

    if args.patch.len() > state.limits.max_patch_bytes {
        return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace patch payload exceeds the configured byte limit; retry with the smallest unique hunk needed for the edit",
            None::<String>,
        )));
    }

    let patch = match parse_workspace_patch(&args.patch) {
        Ok(patch) => patch,
        Err(error) => {
            return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                WORKSPACE_PATCH_TOOL,
                ERROR_INVALID_ARGUMENTS,
                error.message,
                error.path,
            )));
        }
    };

    let mut changes = Vec::with_capacity(patch.files.len());
    for file_patch in patch.files {
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }
        match plan_workspace_patch_file(state, file_patch, is_cancelled) {
            Ok(change) => changes.push(change),
            Err(WorkspacePatchFilePlanError::Domain { error, path }) => {
                return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                    WORKSPACE_PATCH_TOOL,
                    error.code,
                    error.message,
                    Some(path),
                )));
            }
            Err(WorkspacePatchFilePlanError::Path(error)) => {
                return Ok(WorkspacePatchPlanOutcome::Failure(failed_outcome(
                    WORKSPACE_PATCH_TOOL,
                    error.code,
                    error.message,
                    error.path,
                )));
            }
            Err(WorkspacePatchFilePlanError::Cancelled) => {
                return Err(ToolExecutionError::Cancelled);
            }
        }
    }

    Ok(WorkspacePatchPlanOutcome::Planned(WorkspacePatchPlan {
        changes,
    }))
}

#[derive(Debug)]
enum WorkspacePatchPlanOutcome {
    Planned(WorkspacePatchPlan),
    Failure(ToolExecutionOutcome),
}

#[derive(Debug)]
struct WorkspacePatchPlan {
    changes: Vec<WorkspacePatchFilePlan>,
}

impl WorkspacePatchPlan {
    fn subject(&self) -> String {
        match self.changes.as_slice() {
            [change] => change.relative.display.clone(),
            changes => format!("{} files", changes.len()),
        }
    }

    fn summary(&self) -> String {
        match self.changes.as_slice() {
            [change] => format!(
                "Apply {} hunk(s) in {} ({} bytes -> {} bytes).",
                change.hunks, change.relative.display, change.bytes_before, change.bytes_after
            ),
            changes => {
                let bytes_before = changes.iter().fold(0usize, |sum, change| {
                    sum.saturating_add(change.bytes_before)
                });
                let bytes_after = changes
                    .iter()
                    .fold(0usize, |sum, change| sum.saturating_add(change.bytes_after));
                format!(
                    "Apply workspace patch to {} files ({} bytes -> {} bytes).",
                    changes.len(),
                    bytes_before,
                    bytes_after
                )
            }
        }
    }
}

#[derive(Debug)]
struct WorkspacePatchFilePlan {
    relative: ValidatedRelativePath,
    path: PathBuf,
    content_before: String,
    replacement: String,
    preimage_bytes: usize,
    replacement_bytes: usize,
    bytes_before: usize,
    bytes_after: usize,
    hunks: usize,
    max_read_bytes: usize,
}

impl WorkspacePatchFilePlan {
    fn file_fingerprint_before(&self) -> String {
        stable_content_fingerprint(self.content_before.as_bytes())
    }

    fn file_fingerprint_after(&self) -> String {
        stable_content_fingerprint(self.replacement.as_bytes())
    }

    fn change_evidence(&self) -> Result<WorkspacePatchChangeEvidence, ActionProposalError> {
        WorkspacePatchChangeEvidence::new(
            self.relative.display.clone(),
            self.preimage_bytes,
            self.replacement_bytes,
            self.bytes_before,
            self.bytes_after,
            self.file_fingerprint_before(),
            self.file_fingerprint_after(),
        )
    }
}

fn match_approved_patch_proposal(
    approved: &WorkspacePatchProposal,
    plan: &WorkspacePatchPlan,
) -> Result<(), ()> {
    let planned_changes = plan
        .changes
        .iter()
        .map(WorkspacePatchFilePlan::change_evidence)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ())?;
    (approved.changes() == planned_changes.as_slice())
        .then_some(())
        .ok_or(())
}

fn proposal_mismatch_outcome(path: String) -> ToolExecutionOutcome {
    failed_outcome(
        WORKSPACE_PATCH_TOOL,
        ERROR_PROPOSAL_MISMATCH,
        WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE,
        Some(path),
    )
}

fn plan_workspace_patch_file(
    state: &WorkspaceToolState,
    file_patch: WorkspacePatchFile,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchFilePlan, WorkspacePatchFilePlanError> {
    match file_patch.operation {
        WorkspacePatchOperation::Update { hunks } => {
            let relative = validate_relative_path(&file_patch.path, state.allow_hidden)
                .map_err(WorkspacePatchFilePlanError::Path)?;

            for root in &state.roots {
                if is_cancelled() {
                    return Err(WorkspacePatchFilePlanError::Cancelled);
                }

                match resolve_existing_path(root, &relative) {
                    Ok(Some(resolved)) => {
                        return plan_resolved_workspace_patch_file(
                            relative,
                            resolved.path,
                            hunks,
                            state,
                            is_cancelled,
                        )
                        .map_err(|error| match error {
                            BlockingToolError::Domain(error) => {
                                WorkspacePatchFilePlanError::Domain {
                                    error,
                                    path: file_patch.path,
                                }
                            }
                            BlockingToolError::Cancelled => WorkspacePatchFilePlanError::Cancelled,
                        });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return Err(WorkspacePatchFilePlanError::Domain {
                            error,
                            path: relative.display,
                        });
                    }
                }
            }

            Err(WorkspacePatchFilePlanError::Domain {
                error: DomainError::new(ERROR_FILE_NOT_FOUND, "workspace file was not found"),
                path: relative.display,
            })
        }
    }
}

#[derive(Debug)]
enum WorkspacePatchFilePlanError {
    Path(PathValidationError),
    Domain { error: DomainError, path: String },
    Cancelled,
}

fn plan_resolved_workspace_patch_file(
    relative: ValidatedRelativePath,
    path: PathBuf,
    hunks: Vec<WorkspacePatchHunk>,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<WorkspacePatchFilePlan, BlockingToolError> {
    let content = read_patch_preimage(&path, state, is_cancelled)?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let (replacement, preimage_bytes, replacement_bytes) =
        build_patch_replacement(&content, &hunks)?;
    if replacement.len() > state.limits.max_write_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace patch result exceeds the configured write limit",
        )
        .into());
    }

    Ok(WorkspacePatchFilePlan {
        relative,
        path,
        bytes_before: content.len(),
        bytes_after: replacement.len(),
        preimage_bytes,
        replacement_bytes,
        hunks: hunks.len(),
        content_before: content,
        replacement,
        max_read_bytes: state.limits.max_read_bytes,
    })
}

fn read_patch_preimage(
    path: &Path,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    read_patch_preimage_for_path(path, state.limits.max_read_bytes, is_cancelled)
}

fn read_patch_preimage_for_path(
    path: &Path,
    max_read_bytes: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let pre_open_metadata = fs::symlink_metadata(path).map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;
    if pre_open_metadata.file_type().is_symlink() {
        return Err(DomainError::new(ERROR_PATH_DENIED, "workspace path uses a symlink").into());
    }
    if !pre_open_metadata.is_file() {
        return Err(
            DomainError::new(ERROR_NOT_FILE, "workspace path is not a regular file").into(),
        );
    }

    let mut file = open_file_for_read(path)?;
    let metadata = file.metadata().map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;

    if !metadata.is_file() {
        return Err(
            DomainError::new(ERROR_NOT_FILE, "workspace path is not a regular file").into(),
        );
    }

    if metadata.len() > max_read_bytes as u64 {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
        .into());
    }

    let file_size = usize::try_from(metadata.len()).map_err(|_| {
        DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
    })?;

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let mut bytes = Vec::with_capacity(file_size);
    Read::by_ref(&mut file)
        .take(metadata.len())
        .read_to_end(&mut bytes)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;

    if bytes.len() > max_read_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
        .into());
    }

    if bytes.contains(&0) {
        return Err(DomainError::new(ERROR_NOT_UTF8, "workspace file appears to be binary").into());
    }

    let content = String::from_utf8(bytes)
        .map_err(|_| DomainError::new(ERROR_NOT_UTF8, "workspace file is not valid UTF-8"))?;

    Ok(content)
}

fn execute_workspace_patch_plan(
    plan: WorkspacePatchPlan,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    let mut written_changes = Vec::with_capacity(plan.changes.len());
    let mut evidence_changes = Vec::with_capacity(plan.changes.len());

    for change in plan.changes {
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }
        let relative_display = change.relative.display.clone();
        let content_after = match execute_workspace_patch_file_plan(&change, is_cancelled) {
            Ok(content_after) => content_after,
            Err(PatchFileWriteError::Outcome(outcome)) => return Ok(*outcome),
            Err(PatchFileWriteError::Cancelled) => return Err(ToolExecutionError::Cancelled),
        };
        evidence_changes.push(
            WorkspacePatchChangeEvidence::new(
                relative_display.clone(),
                change.preimage_bytes,
                change.replacement_bytes,
                change.bytes_before,
                content_after.len(),
                stable_content_fingerprint(change.content_before.as_bytes()),
                stable_content_fingerprint(content_after.as_bytes()),
            )
            .map_err(|error| {
                ToolExecutionError::infrastructure(format!(
                    "workspace patch execution evidence was invalid: {error}"
                ))
            })?,
        );
        written_changes.push(WorkspacePatchSuccessChange {
            path: relative_display,
            hunks: change.hunks,
            bytes_before: change.bytes_before,
            bytes_after: content_after.len(),
        });
    }

    let evidence =
        WorkspacePatchExecutionEvidence::from_changes(evidence_changes).map_err(|error| {
            ToolExecutionError::infrastructure(format!(
                "workspace patch execution evidence was invalid: {error}"
            ))
        })?;
    let payload = WorkspacePatchSuccess {
        ok: true,
        tool: WORKSPACE_PATCH_TOOL,
        changes: written_changes,
    };
    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace patch success envelope serializes"),
    )
    .with_execution_evidence(ActionExecutionEvidence::WorkspacePatch(evidence)))
}

enum PatchFileWriteError {
    Outcome(Box<ToolExecutionOutcome>),
    Cancelled,
}

fn execute_workspace_patch_file_plan(
    plan: &WorkspacePatchFilePlan,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, PatchFileWriteError> {
    let relative_display = plan.relative.display.clone();
    let mut file = match open_file_for_patch(&plan.path) {
        Ok(file) => file,
        Err(error) => {
            return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                WORKSPACE_PATCH_TOOL,
                error.code,
                error.message,
                Some(relative_display),
            ))));
        }
    };
    match read_open_patch_file_before_write(&mut file, plan.max_read_bytes, is_cancelled) {
        Ok(bytes) if bytes == plan.content_before.as_bytes() => {}
        Ok(_) => {
            return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                WORKSPACE_PATCH_TOOL,
                ERROR_WRITE_FAILED,
                "workspace file changed before patch write",
                Some(relative_display),
            ))));
        }
        Err(BlockingToolError::Domain(error)) => {
            return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                WORKSPACE_PATCH_TOOL,
                error.code,
                error.message,
                Some(relative_display),
            ))));
        }
        Err(BlockingToolError::Cancelled) => return Err(PatchFileWriteError::Cancelled),
    }
    if file.seek(SeekFrom::Start(0)).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not seek workspace file",
            Some(relative_display),
        ))));
    }
    if file.write_all(plan.replacement.as_bytes()).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not write workspace file",
            Some(relative_display),
        ))));
    }
    if file.set_len(plan.replacement.len() as u64).is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not truncate workspace file",
            Some(relative_display),
        ))));
    }
    if file.sync_all().is_err() {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "could not sync workspace file",
            Some(relative_display),
        ))));
    }
    drop(file);

    #[cfg(test)]
    maybe_run_patch_test_after_write_hook(&plan.path);

    let content_after =
        match read_patch_preimage_for_path(&plan.path, plan.replacement.len(), &|| false) {
            Ok(content) => content,
            Err(BlockingToolError::Domain(error)) => {
                return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
                    WORKSPACE_PATCH_TOOL,
                    error.code,
                    error.message,
                    Some(relative_display),
                ))));
            }
            Err(BlockingToolError::Cancelled) => {
                unreachable!("post-write readback is not cancellable")
            }
        };

    if content_after != plan.replacement {
        return Err(PatchFileWriteError::Outcome(Box::new(failed_outcome(
            WORKSPACE_PATCH_TOOL,
            ERROR_WRITE_FAILED,
            "workspace patch verification failed after write",
            Some(relative_display),
        ))));
    }

    Ok(content_after)
}

fn read_open_patch_file_before_write(
    file: &mut fs::File,
    max_read_bytes: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let metadata = file.metadata().map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;
    if !metadata.is_file() {
        return Err(
            DomainError::new(ERROR_NOT_FILE, "workspace path is not a regular file").into(),
        );
    }
    if metadata.len() > max_read_bytes as u64 {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
        .into());
    }

    if file.seek(SeekFrom::Start(0)).is_err() {
        return Err(DomainError::new(ERROR_READ_FAILED, "could not seek workspace file").into());
    }

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).map_err(|_| {
        DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
    })?);
    Read::by_ref(file)
        .take(metadata.len())
        .read_to_end(&mut bytes)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;
    if bytes.len() > max_read_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
        .into());
    }
    Ok(bytes)
}

fn build_patch_replacement(
    content: &str,
    hunks: &[WorkspacePatchHunk],
) -> Result<(String, usize, usize), BlockingToolError> {
    let mut replacement = content.to_owned();
    let trailing_newline = content.ends_with('\n');
    let mut preimage_bytes = 0usize;
    let mut replacement_bytes = 0usize;

    for hunk in hunks {
        let old_text = hunk.old_text(trailing_newline);
        let new_text = hunk.new_text(trailing_newline);
        if old_text.is_empty() {
            return Err(DomainError::new(
                ERROR_INVALID_ARGUMENTS,
                "workspace patch update hunks must include context or removed text",
            )
            .into());
        }
        replacement = build_replacement(&replacement, &old_text, &new_text)?;
        preimage_bytes = preimage_bytes.saturating_add(old_text.len());
        replacement_bytes = replacement_bytes.saturating_add(new_text.len());
    }

    Ok((replacement, preimage_bytes, replacement_bytes))
}

fn collect_patch_hunk_text<'a>(
    lines: impl Iterator<Item = &'a String>,
    trailing_newline: bool,
) -> String {
    let mut output = String::new();
    let mut count = 0usize;
    for line in lines {
        output.push_str(line);
        output.push('\n');
        count += 1;
    }
    if count > 0 && !trailing_newline {
        output.pop();
    }
    output
}

fn build_replacement(
    content: &str,
    old_text: &str,
    new_text: &str,
) -> Result<String, BlockingToolError> {
    let Some(start) = content.find(old_text) else {
        return Err(DomainError::new(
            ERROR_PREIMAGE_ABSENT,
            "workspace patch preimage was not found",
        )
        .into());
    };

    let after_start = start + old_text.len();
    if content[after_start..].contains(old_text) {
        return Err(DomainError::new(
            ERROR_PREIMAGE_AMBIGUOUS,
            "workspace patch preimage matched more than once",
        )
        .into());
    }

    let mut replacement = String::with_capacity(
        content
            .len()
            .saturating_sub(old_text.len())
            .saturating_add(new_text.len()),
    );
    replacement.push_str(&content[..start]);
    replacement.push_str(new_text);
    replacement.push_str(&content[after_start..]);
    Ok(replacement)
}

fn stable_content_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("fnv1a64:{hash:016x}")
}

#[derive(Debug)]
struct SearchRun<'a> {
    query: &'a str,
    max_matches: usize,
    limits: &'a WorkspaceToolLimits,
    allow_hidden: bool,
    matches: Vec<SearchMatch>,
    searched_files: usize,
    scanned_bytes: usize,
    inspected_entries: usize,
    skipped: SearchSkipCounts,
    truncated: bool,
}

impl<'a> SearchRun<'a> {
    fn new(
        query: &'a str,
        max_matches: usize,
        limits: &'a WorkspaceToolLimits,
        allow_hidden: bool,
    ) -> Self {
        Self {
            query,
            max_matches,
            limits,
            allow_hidden,
            matches: Vec::new(),
            searched_files: 0,
            scanned_bytes: 0,
            inspected_entries: 0,
            skipped: SearchSkipCounts::default(),
            truncated: false,
        }
    }

    fn is_done(&self) -> bool {
        self.truncated || self.matches.len() >= self.max_matches
    }

    fn has_search_budget(&self) -> bool {
        self.scanned_bytes < self.limits.max_search_bytes
    }

    fn remaining_search_bytes(&self) -> usize {
        self.limits
            .max_search_bytes
            .saturating_sub(self.scanned_bytes)
    }

    fn record_scanned_bytes(&mut self, bytes: usize) {
        self.scanned_bytes = self.scanned_bytes.saturating_add(bytes);
    }

    fn try_inspect_entry(&mut self) -> bool {
        if self.inspected_entries >= self.limits.max_search_entries {
            self.truncated = true;
            return false;
        }

        self.inspected_entries += 1;
        true
    }
}

fn search_success(
    query: &str,
    path: Option<String>,
    search: SearchRun<'_>,
) -> ToolExecutionOutcome {
    let payload = SearchTextSuccess {
        ok: true,
        tool: WORKSPACE_SEARCH_TEXT_TOOL,
        query,
        path: path.as_deref(),
        matches: search.matches,
        searched_files: search.searched_files,
        skipped: search.skipped,
        truncated: search.truncated,
    };
    ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace search success envelope serializes"),
    )
}

fn search_resolved_path(
    search: &mut SearchRun<'_>,
    path: &Path,
    display: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Err(DomainError::new(
                ERROR_READ_FAILED,
                "could not inspect workspace search path",
            )
            .into());
        }
    };

    if metadata.is_file() {
        search_file(search, path, display, is_cancelled)?;
        Ok(())
    } else if metadata.is_dir() {
        search_directory(search, path, display, is_cancelled)?;
        Ok(())
    } else {
        Err(DomainError::new(
            ERROR_NOT_SEARCHABLE,
            "workspace search path is not a regular file or directory",
        )
        .into())
    }
}

fn search_directory(
    search: &mut SearchRun<'_>,
    path: &Path,
    display_prefix: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let read_dir = match fs::read_dir(path) {
        Ok(read_dir) => read_dir,
        Err(_) => {
            search.skipped.read_failed += 1;
            return Ok(());
        }
    };

    let mut children = Vec::new();
    for entry in read_dir {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        if search.is_done() {
            break;
        }
        if !search.try_inspect_entry() {
            break;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                search.skipped.read_failed += 1;
                continue;
            }
        };

        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            search.skipped.non_utf8 += 1;
            continue;
        };
        if !search.allow_hidden && is_hidden_name(&name) {
            search.skipped.hidden += 1;
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                search.skipped.read_failed += 1;
                continue;
            }
        };

        if file_type.is_symlink() {
            search.skipped.symlink += 1;
            continue;
        }

        let display = join_display_path(display_prefix, &name);
        if file_type.is_dir() || file_type.is_file() {
            children.push(SearchChild {
                path: entry.path(),
                display,
                is_dir: file_type.is_dir(),
            });
        }
    }

    children.sort_by(|left, right| left.display.cmp(&right.display));
    for child in children {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        if search.is_done() {
            break;
        }
        if !search.has_search_budget() {
            search.truncated = true;
            break;
        }

        if child.is_dir {
            search_directory(search, &child.path, &child.display, is_cancelled)?;
        } else {
            search_file(search, &child.path, &child.display, is_cancelled)?;
        }
    }

    Ok(())
}

#[derive(Debug)]
struct SearchChild {
    path: PathBuf,
    display: String,
    is_dir: bool,
}

fn search_file(
    search: &mut SearchRun<'_>,
    path: &Path,
    display: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    if search.searched_files >= search.limits.max_search_files {
        search.truncated = true;
        return Ok(());
    }
    if !search.has_search_budget() {
        search.truncated = true;
        return Ok(());
    }

    let mut file = match open_file_for_read(path) {
        Ok(file) => file,
        Err(error) => {
            if error.code == ERROR_PATH_DENIED {
                search.skipped.symlink += 1;
            } else {
                search.skipped.read_failed += 1;
            }
            return Ok(());
        }
    };

    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => {
            search.skipped.read_failed += 1;
            return Ok(());
        }
    };
    if !metadata.is_file() {
        return Ok(());
    }

    if metadata.len() > search.limits.max_read_bytes as u64 {
        search.searched_files += 1;
        search.skipped.too_large += 1;
        return Ok(());
    }
    let file_size = match usize::try_from(metadata.len()) {
        Ok(file_size) => file_size,
        Err(_) => {
            search.searched_files += 1;
            search.skipped.too_large += 1;
            return Ok(());
        }
    };
    if file_size > search.remaining_search_bytes() {
        search.truncated = true;
        return Ok(());
    }

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let mut bytes = Vec::new();
    if Read::by_ref(&mut file)
        .take(metadata.len())
        .read_to_end(&mut bytes)
        .is_err()
    {
        search.skipped.read_failed += 1;
        return Ok(());
    }

    search.searched_files += 1;
    search.record_scanned_bytes(bytes.len());

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(_) => {
            search.skipped.non_utf8 += 1;
            return Ok(());
        }
    };

    for (line_index, line) in content.lines().enumerate() {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }

        if line.contains(search.query) {
            let (line, truncated) = truncate_utf8_line(line, search.limits.max_search_line_bytes);
            search.matches.push(SearchMatch {
                path: display.to_owned(),
                line_number: line_index + 1,
                line,
                truncated,
            });
            if search.matches.len() >= search.max_matches {
                search.truncated = true;
                return Ok(());
            }
        }
    }

    Ok(())
}

fn join_display_path(prefix: &str, name: &str) -> String {
    if prefix == "." {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

fn truncate_utf8_line(line: &str, max_bytes: usize) -> (String, bool) {
    if line.len() <= max_bytes {
        return (line.to_owned(), false);
    }

    let end = (0..=max_bytes)
        .rev()
        .find(|index| line.is_char_boundary(*index))
        .expect("zero is always a UTF-8 character boundary");
    (line[..end].to_owned(), true)
}

#[derive(Debug)]
struct ResolvedWorkspacePath {
    path: PathBuf,
}

fn resolve_existing_path(
    root: &Path,
    relative: &ValidatedRelativePath,
) -> Result<Option<ResolvedWorkspacePath>, DomainError> {
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
        DomainError::new(ERROR_READ_FAILED, "could not canonicalize workspace path")
    })?;
    if !canonical.starts_with(root) {
        return Err(DomainError::new(
            ERROR_PATH_DENIED,
            "workspace path resolves outside a configured root",
        ));
    }

    Ok(Some(ResolvedWorkspacePath { path: current }))
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

fn open_file_for_patch(path: &Path) -> Result<fs::File, DomainError> {
    open_file_for_patch_impl(path).map_err(|error| {
        if is_symlink_open_error(&error) {
            DomainError::new(ERROR_PATH_DENIED, "workspace path uses a symlink")
        } else {
            DomainError::new(
                ERROR_WRITE_FAILED,
                "could not open workspace file for patching",
            )
        }
    })
}

#[cfg(unix)]
fn open_file_for_read_impl(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}

#[cfg(not(unix))]
fn open_file_for_read_impl(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(unix)]
fn open_file_for_patch_impl(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(false)
        .truncate(false)
        .custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}

#[cfg(not(unix))]
fn open_file_for_patch_impl(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .truncate(false)
        .open(path)
}

#[cfg(unix)]
fn is_symlink_open_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_open_error(_: &io::Error) -> bool {
    false
}

#[derive(Debug, Clone)]
struct ValidatedRelativePath {
    components: Vec<String>,
    display: String,
}

fn validate_relative_path(
    raw_path: &str,
    allow_hidden: bool,
) -> Result<ValidatedRelativePath, PathValidationError> {
    validate_relative_path_impl(raw_path, allow_hidden, false)
}

fn validate_relative_path_or_root(
    raw_path: &str,
    allow_hidden: bool,
) -> Result<ValidatedRelativePath, PathValidationError> {
    validate_relative_path_impl(raw_path, allow_hidden, true)
}

fn validate_relative_path_impl(
    raw_path: &str,
    allow_hidden: bool,
    allow_root: bool,
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

    if allow_root && raw_path == "." {
        return Ok(ValidatedRelativePath {
            components: Vec::new(),
            display: ".".to_owned(),
        });
    }

    let path = Path::new(raw_path);
    if path.is_absolute() {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must be relative",
            None,
        ));
    }

    if has_forbidden_raw_dot_segment(raw_path) {
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            "workspace path must not contain '.' or '..' components",
            Some(raw_path.to_owned()),
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
        let message = if allow_root {
            "workspace path must be exact '.' or name a relative path"
        } else {
            "workspace path must name a file"
        };
        return Err(PathValidationError::new(
            ERROR_PATH_DENIED,
            message,
            Some(raw_path.to_owned()),
        ));
    }

    let display = components.join("/");
    Ok(ValidatedRelativePath {
        components,
        display,
    })
}

fn has_forbidden_raw_dot_segment(raw_path: &str) -> bool {
    raw_path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
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
enum BlockingToolError {
    Domain(DomainError),
    Cancelled,
}

impl From<DomainError> for BlockingToolError {
    fn from(error: DomainError) -> Self {
        Self::Domain(error)
    }
}

fn blocking_tool_error_into_execution(error: BlockingToolError) -> ToolExecutionError {
    match error {
        BlockingToolError::Domain(error) => ToolExecutionError::infrastructure(error.message),
        BlockingToolError::Cancelled => ToolExecutionError::Cancelled,
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
    tool: &'static str,
    code: &'static str,
    message: impl Into<String>,
    path: Option<String>,
) -> ToolExecutionOutcome {
    let message = message.into();
    let envelope = FailureEnvelope {
        ok: false,
        tool,
        error: FailureError {
            code,
            message: &message,
        },
        recovery: FailureRecovery {
            path_contract: WORKSPACE_PATH_CONTRACT,
        },
        path: path.as_deref(),
    };
    ToolExecutionOutcome::failed_json(
        serde_json::to_string(&envelope).expect("workspace failure envelope serializes"),
        ErrorInfo::new(code, &message).expect("workspace diagnostic is valid"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use merry_core::{ToolCallArguments, ToolCallId, ToolCallResultStatus};
    use merry_runtime::{ActionExecutionEvidence, ArtifactContentKind, ToolExecutionError};
    use serde_json::{Map, Value, json};
    use std::{
        cell::Cell,
        env,
        ffi::OsStr,
        fs::{self, File},
        future::Future,
        io::Write,
        sync::{Arc as StdArc, Mutex as StdMutex, OnceLock as StdOnceLock},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn trace_output_buffer() -> &'static StdArc<StdMutex<Vec<u8>>> {
        #[derive(Clone)]
        struct Buffer(StdArc<StdMutex<Vec<u8>>>);

        impl std::io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("buffer mutex should not be poisoned")
                    .extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        static TRACE_OUTPUT: StdOnceLock<StdArc<StdMutex<Vec<u8>>>> = StdOnceLock::new();
        TRACE_OUTPUT.get_or_init(|| {
            use tracing_subscriber::{fmt, prelude::*};

            let bytes = StdArc::new(StdMutex::new(Vec::new()));
            let writer_bytes = StdArc::clone(&bytes);
            let subscriber = tracing_subscriber::registry().with(
                fmt::layer()
                    .json()
                    .with_writer(move || Buffer(StdArc::clone(&writer_bytes))),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("test tracing subscriber should install once");
            bytes
        })
    }

    async fn capture_traces_for<F, R>(trace_marker: &str, future: F) -> (R, String)
    where
        F: Future<Output = R>,
    {
        let bytes = StdArc::clone(trace_output_buffer());
        let start = bytes
            .lock()
            .expect("buffer mutex should not be poisoned")
            .len();
        let result = future.await;
        let text = {
            let guard = bytes.lock().expect("buffer mutex should not be poisoned");
            String::from_utf8(guard[start..].to_vec()).expect("trace output should be UTF-8")
        };
        let text = text
            .lines()
            .filter(|line| line.contains(trace_marker))
            .collect::<Vec<_>>()
            .join("\n");
        (result, text)
    }

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

    fn list_outcome(tools: &ReadOnlyWorkspaceTools, path: &str) -> ToolExecutionOutcome {
        list_dir_blocking(&tools.state, path.to_owned())
    }

    fn search_outcome(
        tools: &ReadOnlyWorkspaceTools,
        query: &str,
        path: Option<&str>,
        max_matches: Option<usize>,
    ) -> ToolExecutionOutcome {
        search_text_blocking(
            &tools.state,
            SearchTextArgs {
                query: query.to_owned(),
                path: path.map(str::to_owned),
                max_matches,
            },
        )
    }

    fn patch_outcome(
        tools: &ReadOnlyWorkspaceTools,
        path: &str,
        old_text: &str,
        new_text: &str,
    ) -> ToolExecutionOutcome {
        workspace_patch_blocking(
            &tools.state,
            WorkspacePatchArgs {
                patch: update_patch(path, old_text, new_text),
            },
        )
    }

    fn patch_text_outcome(tools: &ReadOnlyWorkspaceTools, patch: &str) -> ToolExecutionOutcome {
        workspace_patch_blocking(
            &tools.state,
            WorkspacePatchArgs {
                patch: patch.to_owned(),
            },
        )
    }

    fn patch_proposal(
        tools: &ReadOnlyWorkspaceTools,
        path: &str,
        old_text: &str,
        new_text: &str,
    ) -> Option<ActionProposal> {
        let patch = update_patch(path, old_text, new_text);
        let call = pending_call_for(
            WORKSPACE_PATCH_TOOL,
            json!({
                "patch": patch
            }),
        );
        propose_workspace_patch_blocking_checked(
            &tools.state,
            WorkspacePatchArgs { patch },
            &call,
            &|| false,
        )
        .expect("uncancelled workspace patch proposal should not return cancellation")
    }

    fn update_patch(path: &str, old_text: &str, new_text: &str) -> String {
        format!(
            "*** Begin Workspace Patch\n*** Update File: {path}\n-{old_text}\n+{new_text}\n*** End Workspace Patch"
        )
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
        assert_failed_json_for_tool(outcome, WORKSPACE_READ_FILE_TOOL, code, path, host_root);
    }

    fn assert_failed_json_for_tool(
        outcome: &ToolExecutionOutcome,
        tool: &str,
        code: &str,
        path: Option<&str>,
        host_root: &Path,
    ) {
        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(outcome.content().kind(), ArtifactContentKind::Json);
        assert_eq!(outcome.diagnostic().expect("diagnostic").code(), code);

        let payload = json_content(outcome);
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["tool"], tool);
        assert_eq!(payload["error"]["code"], code);
        assert_eq!(
            payload["recovery"]["path_contract"],
            WORKSPACE_PATH_CONTRACT
        );
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

    fn assert_no_provider_visible_patch_metadata(outcome: &ToolExecutionOutcome) {
        let text = outcome
            .content()
            .as_text()
            .expect("json content should be text");
        for forbidden in [
            "approved workspace patch",
            "fingerprint",
            "proposal",
            "fnv1a64",
            "file_fingerprint_before",
            "file_fingerprint_after",
        ] {
            assert!(
                !text.contains(forbidden),
                "provider-visible patch output leaked {forbidden}: {text}"
            );
        }
        if let Some(diagnostic) = outcome.diagnostic() {
            assert_eq!(
                diagnostic.message(),
                WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE,
                "patch mismatch diagnostic should stay neutral"
            );
            let diagnostic_text = format!("{} {}", diagnostic.code(), diagnostic.message());
            for forbidden in [
                "approved workspace patch",
                "fingerprint",
                "proposal",
                "fnv1a64",
                "file_fingerprint_before",
                "file_fingerprint_after",
            ] {
                assert!(
                    !diagnostic_text.contains(forbidden),
                    "provider-visible patch diagnostic leaked {forbidden}: {diagnostic_text}"
                );
            }
        }
    }

    fn pending_call(arguments: Value) -> PendingToolCall {
        pending_call_for(WORKSPACE_READ_FILE_TOOL, arguments)
    }

    fn pending_call_with_id(tool: &str, call_id: &str, arguments: Value) -> PendingToolCall {
        let arguments = ToolCallArguments::try_from(arguments).expect("arguments object");
        PendingToolCall::new(
            ToolCallId::new(call_id).expect("valid call id"),
            ToolName::new(tool).expect("valid tool name"),
            arguments,
        )
    }

    fn pending_call_for(tool: &str, arguments: Value) -> PendingToolCall {
        pending_call_with_id(tool, "call-1", arguments)
    }

    fn assert_invalid_arguments_trace(
        outcome: ToolExecutionOutcome,
        logs: &str,
        tool_name: &str,
        tool_call_id: &str,
    ) {
        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            outcome.diagnostic().expect("diagnostic").code(),
            ERROR_INVALID_ARGUMENTS
        );
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
        assert!(logs.contains("\"status\":\"failed\""));
        assert!(logs.contains("\"diagnostic_code\":\"workspace_invalid_arguments\""));
        assert!(logs.contains(&format!("\"tool_name\":\"{tool_name}\"")));
        assert!(logs.contains(&format!("\"tool_call_id\":\"{tool_call_id}\"")));
        assert!(!logs.contains("sensitive invalid payload"));
    }

    fn read_text(path: &Path) -> String {
        fs::read_to_string(path).expect("workspace text file should be readable")
    }

    static TRACE_START_CANCEL_TOKEN: StdOnceLock<
        StdMutex<Option<tokio_util::sync::CancellationToken>>,
    > = StdOnceLock::new();

    fn install_trace_start_cancellation_token(token: tokio_util::sync::CancellationToken) {
        TRACE_START_CANCEL_TOKEN
            .get_or_init(|| StdMutex::new(None))
            .lock()
            .expect("trace start cancel token mutex should not be poisoned")
            .replace(token);
    }

    fn cancel_trace_start_token() {
        let Some(slot) = TRACE_START_CANCEL_TOKEN.get() else {
            return;
        };
        let token = slot
            .lock()
            .expect("trace start cancel token mutex should not be poisoned")
            .take();
        if let Some(token) = token {
            token.cancel();
        }
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
        let config = WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_read_bytes: 0,
                ..WorkspaceToolLimits::default()
            },
        );

        let err = ReadOnlyWorkspaceTools::new(config).expect_err("zero limit should be rejected");
        assert!(matches!(
            err,
            WorkspaceToolConfigError::InvalidLimit {
                name: "max_read_bytes"
            }
        ));
    }

    #[test]
    fn config_rejects_each_zero_limit() {
        let temp = TempWorkspace::new("zero-all-limits");

        for invalid_name in [
            "max_read_bytes",
            "max_write_bytes",
            "max_patch_bytes",
            "max_list_entries",
            "max_search_matches",
            "max_search_files",
            "max_search_entries",
            "max_search_bytes",
            "max_search_line_bytes",
            "max_search_query_bytes",
        ] {
            let mut limits = WorkspaceToolLimits::default();
            match invalid_name {
                "max_read_bytes" => limits.max_read_bytes = 0,
                "max_write_bytes" => limits.max_write_bytes = 0,
                "max_patch_bytes" => limits.max_patch_bytes = 0,
                "max_list_entries" => limits.max_list_entries = 0,
                "max_search_matches" => limits.max_search_matches = 0,
                "max_search_files" => limits.max_search_files = 0,
                "max_search_entries" => limits.max_search_entries = 0,
                "max_search_bytes" => limits.max_search_bytes = 0,
                "max_search_line_bytes" => limits.max_search_line_bytes = 0,
                "max_search_query_bytes" => limits.max_search_query_bytes = 0,
                other => panic!("unexpected limit name {other}"),
            }

            let config =
                WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(limits);
            let err =
                ReadOnlyWorkspaceTools::new(config).expect_err("zero limit should be rejected");
            assert!(matches!(
                err,
                WorkspaceToolConfigError::InvalidLimit { name } if name == invalid_name
            ));
        }
    }

    #[test]
    fn into_registered_tools_exposes_read_list_and_search() {
        let temp = TempWorkspace::new("registered-tools");
        let tools = tools_for(temp.path()).into_registered_tools();
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool.spec().name().as_str())
            .collect();

        assert_eq!(
            names,
            [
                WORKSPACE_READ_FILE_TOOL,
                WORKSPACE_LIST_DIR_TOOL,
                WORKSPACE_SEARCH_TEXT_TOOL
            ]
        );
    }

    #[test]
    fn patch_tool_registration_is_opt_in_and_workspace_write() {
        let temp = TempWorkspace::new("registered-patch-tools");

        let read_only_tools = tools_for(temp.path()).into_registered_tools();
        assert!(
            read_only_tools
                .iter()
                .all(|tool| tool.spec().name().as_str() != WORKSPACE_PATCH_TOOL)
        );

        let tools = tools_for(temp.path()).into_registered_tools_with_patch();
        let patch = tools
            .iter()
            .find(|tool| tool.spec().name().as_str() == WORKSPACE_PATCH_TOOL)
            .expect("patch tool should be registered only by opt-in method");
        assert_eq!(patch.action_kind(), ToolActionKind::WorkspaceWrite);
        assert!(patch.proposals_enabled());
        assert_eq!(tools.len(), 4);
        assert!(
            tools
                .iter()
                .filter(|tool| tool.spec().name().as_str() != WORKSPACE_PATCH_TOOL)
                .all(|tool| {
                    tool.action_kind() == ToolActionKind::ReadOnly && !tool.proposals_enabled()
                })
        );
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
        for forbidden in [
            "fingerprint",
            "fnv1a64",
            "preimage_bytes",
            "replacement_bytes",
        ] {
            assert!(
                !outcome
                    .content()
                    .as_text()
                    .expect("json content")
                    .contains(forbidden),
                "provider-visible patch output leaked {forbidden}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_read_file_traces_start_and_finish_without_file_contents() {
        let temp = TempWorkspace::new("trace-read-file");
        temp.write_text("lib.rs", "secret source text\n");
        let tools = tools_for(temp.path());
        let executor = ReadFileExecutor {
            state: Arc::clone(&tools.state),
        };
        let call = pending_call_with_id(
            WORKSPACE_READ_FILE_TOOL,
            "call-trace-read-file",
            json!({ "path": "lib.rs" }),
        );

        let (outcome, logs) = capture_traces_for(
            "call-trace-read-file",
            executor.execute(call, ToolExecutionContext::default()),
        )
        .await;
        let outcome = outcome.expect("read should succeed");

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert!(outcome.diagnostic().is_none());
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
        assert!(logs.contains("\"status\":\"succeeded\""));
        assert!(logs.contains("\"tool_name\":\"workspace_read_file\""));
        assert!(logs.contains("\"tool_call_id\":\"call-trace-read-file\""));
        assert!(logs.contains("\"path\":\"lib.rs\""));
        assert!(!logs.contains("secret source text"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_read_file_failure_trace_includes_diagnostic_code() {
        let temp = TempWorkspace::new("trace-read-failure");
        let tools = tools_for(temp.path());
        let executor = ReadFileExecutor {
            state: Arc::clone(&tools.state),
        };
        let call = pending_call_with_id(
            WORKSPACE_READ_FILE_TOOL,
            "call-trace-read-failure",
            json!({ "path": "../secret.txt" }),
        );

        let (outcome, logs) = capture_traces_for(
            "call-trace-read-failure",
            executor.execute(call, ToolExecutionContext::default()),
        )
        .await;
        let outcome = outcome.expect("path denial should resolve as a domain result");

        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        assert_eq!(
            outcome.diagnostic().expect("diagnostic").code(),
            ERROR_PATH_DENIED
        );
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
        assert!(logs.contains("\"status\":\"failed\""));
        assert!(logs.contains("\"diagnostic_code\":\"workspace_path_denied\""));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_path_traces_are_bounded_summaries() {
        let temp = TempWorkspace::new("trace-bounded-path");
        let tools = tools_for(temp.path());
        let executor = ReadFileExecutor {
            state: Arc::clone(&tools.state),
        };
        let long_path = format!("{}tail.txt", "nested/".repeat(32));
        let expected_summary = bounded_trace_text(&long_path, TRACE_PATH_MAX_CHARS);
        let call = pending_call_with_id(
            WORKSPACE_READ_FILE_TOOL,
            "call-trace-bounded-path",
            json!({ "path": long_path }),
        );

        let (_outcome, logs) = capture_traces_for(
            "call-trace-bounded-path",
            executor.execute(call, ToolExecutionContext::default()),
        )
        .await;

        assert!(logs.contains(expected_summary.as_str()));
        assert!(!logs.contains("nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/nested/tail.txt"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_started_tool_trace_finishes_cancelled_when_token_cancels() {
        let temp = TempWorkspace::new("trace-cancelled");
        temp.write_text("note.txt", "ok\n");
        let tools = tools_for(temp.path());
        let executor = ReadFileExecutor {
            state: Arc::clone(&tools.state),
        };
        let call = pending_call_with_id(
            WORKSPACE_READ_FILE_TOOL,
            "call-trace-cancelled",
            json!({ "path": "note.txt" }),
        );
        let token = tokio_util::sync::CancellationToken::new();
        install_trace_start_cancellation_token(token.clone());
        install_trace_start_test_hook("call-trace-cancelled", cancel_trace_start_token);

        let (result, logs) = capture_traces_for(
            "call-trace-cancelled",
            executor.execute(call, ToolExecutionContext::new(token)),
        )
        .await;
        let error = result.expect_err("cancelled execution should return cancellation error");

        assert!(matches!(error, ToolExecutionError::Cancelled));
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
        assert!(logs.contains("\"status\":\"cancelled\""));
        assert!(logs.contains("\"diagnostic_code\":\"workspace_tool_cancelled\""));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_search_trace_uses_query_bytes_without_long_query_text() {
        let temp = TempWorkspace::new("trace-search");
        temp.write_text("notes.txt", "needle\n");
        let tools = tools_for(temp.path());
        let executor = SearchTextExecutor {
            state: Arc::clone(&tools.state),
        };
        let long_query = "needle-".repeat(24);
        let call = pending_call_with_id(
            WORKSPACE_SEARCH_TEXT_TOOL,
            "call-trace-search",
            json!({ "query": long_query }),
        );

        let (outcome, logs) = capture_traces_for(
            "call-trace-search",
            executor.execute(call, ToolExecutionContext::default()),
        )
        .await;
        let outcome = outcome.expect("search should resolve as a domain result");

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
        assert!(logs.contains("\"tool_name\":\"workspace_search_text\""));
        assert!(logs.contains("\"query_bytes\":168"));
        assert!(!logs.contains(long_query.as_str()));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_patch_trace_uses_byte_counts_without_patch_text() {
        let temp = TempWorkspace::new("trace-patch");
        temp.write_text("src/lib.rs", "old secret text\n");
        let tools = tools_for(temp.path());
        let executor = WorkspacePatchExecutor {
            state: Arc::clone(&tools.state),
        };
        let patch = update_patch("src/lib.rs", "old secret text", "new secret text");
        let call = pending_call_with_id(
            WORKSPACE_PATCH_TOOL,
            "call-trace-patch",
            json!({ "patch": patch }),
        );

        let (outcome, logs) = capture_traces_for(
            "call-trace-patch",
            executor.execute(call, ToolExecutionContext::default()),
        )
        .await;
        let outcome = outcome.expect("patch should succeed");

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
        assert!(logs.contains("\"tool_name\":\"workspace_patch\""));
        assert!(logs.contains("\"patch_bytes\":"));
        assert!(!logs.contains("old secret text"));
        assert!(!logs.contains("new secret text"));
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
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_read_bytes: 5,
                    ..WorkspaceToolLimits::default()
                },
            ),
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
    fn list_dir_success_returns_sorted_non_hidden_entries_and_symlink_kind() {
        let temp = TempWorkspace::new("list-success");
        temp.write_text("root/b.txt", "b\n");
        temp.write_text("root/a.txt", "a\n");
        fs::create_dir_all(temp.path().join("root/dir")).expect("directory should be created");
        temp.write_text("root/.secret", "hidden\n");
        #[cfg(unix)]
        symlink(
            temp.path().join("root/a.txt"),
            temp.path().join("root/link.txt"),
        )
        .expect("symlink should be created");
        let tools = tools_for(temp.path());

        let outcome = list_outcome(&tools, "root");

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["tool"], WORKSPACE_LIST_DIR_TOOL);
        assert_eq!(payload["path"], "root");
        assert_eq!(payload["truncated"], false);

        #[cfg(unix)]
        assert_eq!(
            payload["entries"],
            json!([
                { "name": "a.txt", "path": "root/a.txt", "kind": "file" },
                { "name": "b.txt", "path": "root/b.txt", "kind": "file" },
                { "name": "dir", "path": "root/dir", "kind": "directory" },
                { "name": "link.txt", "path": "root/link.txt", "kind": "symlink" }
            ])
        );

        #[cfg(not(unix))]
        assert_eq!(
            payload["entries"],
            json!([
                { "name": "a.txt", "path": "root/a.txt", "kind": "file" },
                { "name": "b.txt", "path": "root/b.txt", "kind": "file" },
                { "name": "dir", "path": "root/dir", "kind": "directory" }
            ])
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

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_list_dir_traces_start_and_finish_without_entry_contents() {
        let temp = TempWorkspace::new("trace-list-dir");
        temp.write_text("root/secret-name.txt", "secret file content\n");
        let tools = tools_for(temp.path());
        let executor = ListDirExecutor {
            state: Arc::clone(&tools.state),
        };
        let call = pending_call_with_id(
            WORKSPACE_LIST_DIR_TOOL,
            "call-trace-list-dir",
            json!({ "path": "root" }),
        );

        let (outcome, logs) = capture_traces_for(
            "call-trace-list-dir",
            executor.execute(call, ToolExecutionContext::default()),
        )
        .await;
        let outcome = outcome.expect("list should succeed");

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.start\""));
        assert!(logs.contains("\"event\":\"runtime.workspace_tool.finish\""));
        assert!(logs.contains("\"status\":\"succeeded\""));
        assert!(logs.contains("\"tool_name\":\"workspace_list_dir\""));
        assert!(logs.contains("\"tool_call_id\":\"call-trace-list-dir\""));
        assert!(logs.contains("\"path\":\"root\""));
        assert!(!logs.contains("secret file content"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_tool_invalid_arguments_trace_failed_without_payload() {
        let temp = TempWorkspace::new("trace-invalid-args");
        let tools = tools_for(temp.path());
        let invalid_arguments = json!({ "unexpected": "sensitive invalid payload" });

        let read_executor = ReadFileExecutor {
            state: Arc::clone(&tools.state),
        };
        let read_call = pending_call_with_id(
            WORKSPACE_READ_FILE_TOOL,
            "call-trace-read-invalid-args",
            invalid_arguments.clone(),
        );
        let (read_outcome, read_logs) = capture_traces_for(
            "call-trace-read-invalid-args",
            read_executor.execute(read_call, ToolExecutionContext::default()),
        )
        .await;
        assert_invalid_arguments_trace(
            read_outcome.expect("read invalid args should resolve as a failed outcome"),
            &read_logs,
            WORKSPACE_READ_FILE_TOOL,
            "call-trace-read-invalid-args",
        );

        let list_executor = ListDirExecutor {
            state: Arc::clone(&tools.state),
        };
        let list_call = pending_call_with_id(
            WORKSPACE_LIST_DIR_TOOL,
            "call-trace-list-invalid-args",
            invalid_arguments.clone(),
        );
        let (list_outcome, list_logs) = capture_traces_for(
            "call-trace-list-invalid-args",
            list_executor.execute(list_call, ToolExecutionContext::default()),
        )
        .await;
        assert_invalid_arguments_trace(
            list_outcome.expect("list invalid args should resolve as a failed outcome"),
            &list_logs,
            WORKSPACE_LIST_DIR_TOOL,
            "call-trace-list-invalid-args",
        );

        let search_executor = SearchTextExecutor {
            state: Arc::clone(&tools.state),
        };
        let search_call = pending_call_with_id(
            WORKSPACE_SEARCH_TEXT_TOOL,
            "call-trace-search-invalid-args",
            invalid_arguments.clone(),
        );
        let (search_outcome, search_logs) = capture_traces_for(
            "call-trace-search-invalid-args",
            search_executor.execute(search_call, ToolExecutionContext::default()),
        )
        .await;
        assert_invalid_arguments_trace(
            search_outcome.expect("search invalid args should resolve as a failed outcome"),
            &search_logs,
            WORKSPACE_SEARCH_TEXT_TOOL,
            "call-trace-search-invalid-args",
        );

        let patch_executor = WorkspacePatchExecutor {
            state: Arc::clone(&tools.state),
        };
        let patch_call = pending_call_with_id(
            WORKSPACE_PATCH_TOOL,
            "call-trace-patch-invalid-args",
            invalid_arguments,
        );
        let (patch_outcome, patch_logs) = capture_traces_for(
            "call-trace-patch-invalid-args",
            patch_executor.execute(patch_call, ToolExecutionContext::default()),
        )
        .await;
        assert_invalid_arguments_trace(
            patch_outcome.expect("patch invalid args should resolve as a failed outcome"),
            &patch_logs,
            WORKSPACE_PATCH_TOOL,
            "call-trace-patch-invalid-args",
        );
    }

    #[test]
    fn list_dir_allows_exact_root_dot_and_truncates_as_success() {
        let temp = TempWorkspace::new("list-root-limit");
        temp.write_text("c.txt", "c\n");
        temp.write_text("a.txt", "a\n");
        temp.write_text("b.txt", "b\n");
        temp.write_text("aa.txt", "aa\n");
        let tools = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_list_entries: 2,
                    ..WorkspaceToolLimits::default()
                },
            ),
        )
        .expect("workspace tools should construct");

        let outcome = list_outcome(&tools, ".");

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(payload["path"], ".");
        assert_eq!(payload["truncated"], true);
        assert_eq!(
            payload["entries"],
            json!([
                { "name": "a.txt", "path": "a.txt", "kind": "file" },
                { "name": "aa.txt", "path": "aa.txt", "kind": "file" }
            ])
        );
    }

    #[test]
    fn list_dir_rejects_bad_paths_and_non_directory_domain_failure() {
        let temp = TempWorkspace::new("list-domain-failures");
        temp.write_text("file.txt", "content\n");
        let tools = tools_for(temp.path());

        let dot_component = list_outcome(&tools, "dir/./file");
        assert_failed_json_for_tool(
            &dot_component,
            WORKSPACE_LIST_DIR_TOOL,
            ERROR_PATH_DENIED,
            Some("dir/./file"),
            temp.path(),
        );

        let file = list_outcome(&tools, "file.txt");
        assert_failed_json_for_tool(
            &file,
            WORKSPACE_LIST_DIR_TOOL,
            ERROR_NOT_DIRECTORY,
            Some("file.txt"),
            temp.path(),
        );

        let missing = list_outcome(&tools, "missing");
        assert_failed_json_for_tool(
            &missing,
            WORKSPACE_LIST_DIR_TOOL,
            ERROR_PATH_NOT_FOUND,
            Some("missing"),
            temp.path(),
        );

        let absolute_parent = list_outcome(&tools, "/../outside");
        assert_failed_json_for_tool(
            &absolute_parent,
            WORKSPACE_LIST_DIR_TOOL,
            ERROR_PATH_DENIED,
            None,
            temp.path(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_dir_rejects_requested_symlink_without_following_it() {
        let temp = TempWorkspace::new("list-symlink");
        fs::create_dir_all(temp.path().join("target")).expect("target directory should be created");
        symlink(temp.path().join("target"), temp.path().join("link"))
            .expect("symlink should be created");
        let tools = tools_for(temp.path());

        let outcome = list_outcome(&tools, "link");

        assert_failed_json_for_tool(
            &outcome,
            WORKSPACE_LIST_DIR_TOOL,
            ERROR_PATH_DENIED,
            Some("link"),
            temp.path(),
        );
    }

    #[test]
    fn search_text_finds_literal_case_sensitive_matches_in_stable_order() {
        let temp = TempWorkspace::new("search-success");
        temp.write_text("b.txt", "needle in b\nNeedle uppercase\n");
        temp.write_text("a.txt", "first\nneedle in a\n");
        temp.write_text("dir/c.txt", "needle in c\n");
        let tools = tools_for(temp.path());

        let outcome = search_outcome(&tools, "needle", None, None);

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["tool"], WORKSPACE_SEARCH_TEXT_TOOL);
        assert_eq!(payload["query"], "needle");
        assert!(payload.get("path").is_none());
        assert_eq!(payload["searched_files"], 3);
        assert_eq!(payload["truncated"], false);
        assert_eq!(
            payload["matches"],
            json!([
                { "path": "a.txt", "line_number": 2, "line": "needle in a", "truncated": false },
                { "path": "b.txt", "line_number": 1, "line": "needle in b", "truncated": false },
                { "path": "dir/c.txt", "line_number": 1, "line": "needle in c", "truncated": false }
            ])
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
    fn search_text_returns_success_for_no_match() {
        let temp = TempWorkspace::new("search-no-match");
        temp.write_text("note.txt", "alpha\n");
        let tools = tools_for(temp.path());

        let outcome = search_outcome(&tools, "needle", Some("note.txt"), None);

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(payload["path"], "note.txt");
        assert_eq!(payload["matches"], json!([]));
        assert_eq!(payload["truncated"], false);
    }

    #[test]
    fn search_text_respects_match_file_query_and_line_limits() {
        let temp = TempWorkspace::new("search-limits");
        temp.write_text("a.txt", "needle abcdef\nneedle again\n");
        temp.write_text("b.txt", "needle in b\n");
        let tools = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_search_matches: 5,
                    max_search_files: 1,
                    max_search_line_bytes: 10,
                    max_search_query_bytes: 6,
                    ..WorkspaceToolLimits::default()
                },
            ),
        )
        .expect("workspace tools should construct");

        let limited = search_outcome(&tools, "needle", None, Some(1));
        assert_eq!(limited.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&limited);
        assert_eq!(payload["searched_files"], 1);
        assert_eq!(payload["truncated"], true);
        assert_eq!(
            payload["matches"],
            json!([
                { "path": "a.txt", "line_number": 1, "line": "needle abc", "truncated": true }
            ])
        );

        let file_limited = search_outcome(&tools, "absent", None, None);
        assert_eq!(file_limited.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&file_limited);
        assert_eq!(payload["searched_files"], 1);
        assert_eq!(payload["truncated"], true);
        assert_eq!(payload["matches"], json!([]));

        let too_long = search_outcome(&tools, "needles", None, None);
        assert_failed_json_for_tool(
            &too_long,
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            None,
            temp.path(),
        );
    }

    #[test]
    fn search_text_total_byte_limit_truncates_without_scanning_next_file() {
        let temp = TempWorkspace::new("search-total-bytes");
        temp.write_text("a.txt", "abc\n");
        temp.write_text("b.txt", "needle\n");
        let tools = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_search_bytes: 4,
                    ..WorkspaceToolLimits::default()
                },
            ),
        )
        .expect("workspace tools should construct");

        let outcome = search_outcome(&tools, "needle", None, None);

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(payload["searched_files"], 1);
        assert_eq!(payload["truncated"], true);
        assert_eq!(payload["matches"], json!([]));
    }

    #[test]
    fn search_text_entry_limit_truncates_recursive_enumeration() {
        let temp = TempWorkspace::new("search-entry-limit");
        temp.write_text("a.txt", "absent\n");
        temp.write_text("b.txt", "needle\n");
        let tools = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_search_entries: 1,
                    ..WorkspaceToolLimits::default()
                },
            ),
        )
        .expect("workspace tools should construct");

        let outcome = search_outcome(&tools, "needle", None, None);

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(payload["searched_files"], 0);
        assert_eq!(payload["truncated"], true);
        assert_eq!(payload["matches"], json!([]));
    }

    #[test]
    fn search_text_counts_skipped_hidden_non_utf8_symlink_and_too_large() {
        let temp = TempWorkspace::new("search-skips");
        temp.write_text("visible.txt", "needle\n");
        temp.write_text(".hidden.txt", "needle hidden\n");
        temp.write_bytes("binary.bin", &[0xff, 0xfe, 0xfd]);
        temp.write_text("large.txt", "needle large\n");
        #[cfg(unix)]
        symlink(
            temp.path().join("visible.txt"),
            temp.path().join("linked.txt"),
        )
        .expect("symlink should be created");
        let tools = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_read_bytes: 6,
                    ..WorkspaceToolLimits::default()
                },
            ),
        )
        .expect("workspace tools should construct");

        let outcome = search_outcome(&tools, "needle", None, None);

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(payload["matches"], json!([]));
        assert_eq!(payload["skipped"]["hidden"], 1);
        assert_eq!(payload["skipped"]["non_utf8"], 1);
        assert_eq!(payload["skipped"]["too_large"], 2);
        #[cfg(unix)]
        assert_eq!(payload["skipped"]["symlink"], 1);
    }

    #[test]
    fn search_text_rejects_bad_path_and_missing_path_domain_failure() {
        let temp = TempWorkspace::new("search-domain-failures");
        let tools = tools_for(temp.path());

        let denied = search_outcome(&tools, "needle", Some("../outside"), None);
        assert_failed_json_for_tool(
            &denied,
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_PATH_DENIED,
            Some("../outside"),
            temp.path(),
        );

        let missing = search_outcome(&tools, "needle", Some("missing.txt"), None);
        assert_failed_json_for_tool(
            &missing,
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_PATH_NOT_FOUND,
            Some("missing.txt"),
            temp.path(),
        );

        let empty_query = search_outcome(&tools, "", None, None);
        assert_failed_json_for_tool(
            &empty_query,
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            None,
            temp.path(),
        );

        let multiline_query = search_outcome(&tools, "need\nle", None, None);
        assert_failed_json_for_tool(
            &multiline_query,
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            None,
            temp.path(),
        );

        let control_query =
            search_outcome(&tools, &format!("bad{}query", char::from(7)), None, None);
        assert_failed_json_for_tool(
            &control_query,
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            None,
            temp.path(),
        );

        let absolute_parent = search_outcome(&tools, "needle", Some("/../outside"), None);
        assert_failed_json_for_tool(
            &absolute_parent,
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_PATH_DENIED,
            None,
            temp.path(),
        );
    }

    #[test]
    fn workspace_patch_executor_replaces_one_hunk_in_existing_utf8_file() {
        let temp = TempWorkspace::new("patch-success");
        temp.write_text("dir/note.txt", "alpha\nold value\nomega\n");
        let tools = tools_for(temp.path());
        let executor = WorkspacePatchExecutor {
            state: Arc::clone(&tools.state),
        };
        let patch = update_patch("dir/note.txt", "old value", "new value");
        let call = pending_call_for(WORKSPACE_PATCH_TOOL, json!({ "patch": patch }));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio runtime should build");

        let outcome = runtime
            .block_on(executor.execute(call, ToolExecutionContext::default()))
            .expect("patch executor should succeed");

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        let payload = json_content(&outcome);
        assert_eq!(
            payload,
            json!({
                "ok": true,
                "tool": WORKSPACE_PATCH_TOOL,
                "changes": [{
                    "path": "dir/note.txt",
                    "hunks": 1,
                    "bytes_before": 22,
                    "bytes_after": 22
                }]
            })
        );
        assert_eq!(
            read_text(&temp.path().join("dir/note.txt")),
            "alpha\nnew value\nomega\n"
        );
        let evidence = match outcome
            .execution_evidence()
            .expect("successful patch should include internal execution evidence")
        {
            ActionExecutionEvidence::WorkspacePatch(evidence) => evidence,
            ActionExecutionEvidence::ProcessAction(_) => {
                panic!("workspace patch execution must not produce process action evidence")
            }
        };
        assert_eq!(evidence.relative_path(), "dir/note.txt");
        assert_eq!(evidence.preimage_bytes(), "old value\n".len());
        assert_eq!(evidence.replacement_bytes(), "new value\n".len());
        assert_eq!(evidence.file_bytes_before(), 22);
        assert_eq!(evidence.file_bytes_after(), 22);
        assert_eq!(
            evidence.file_fingerprint_before(),
            &stable_content_fingerprint("alpha\nold value\nomega\n".as_bytes())
        );
        assert_eq!(
            evidence.file_fingerprint_after(),
            &stable_content_fingerprint("alpha\nnew value\nomega\n".as_bytes())
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
    fn workspace_patch_executor_applies_multi_file_patch_and_records_each_change() {
        let temp = TempWorkspace::new("patch-multi-file-success");
        temp.write_text("src/lib.rs", "alpha\nold lib\nomega\n");
        temp.write_text("tests/smoke.rs", "alpha\nold test\nomega\n");
        let tools = tools_for(temp.path());
        let patch = "\
*** Begin Workspace Patch
*** Update File: src/lib.rs
-old lib
+new lib
*** Update File: tests/smoke.rs
-old test
+new test
*** End Workspace Patch";

        let outcome = patch_text_outcome(&tools, patch);

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(
            read_text(&temp.path().join("src/lib.rs")),
            "alpha\nnew lib\nomega\n"
        );
        assert_eq!(
            read_text(&temp.path().join("tests/smoke.rs")),
            "alpha\nnew test\nomega\n"
        );
        let payload = json_content(&outcome);
        assert_eq!(payload["tool"], WORKSPACE_PATCH_TOOL);
        assert_eq!(
            payload["changes"]
                .as_array()
                .expect("changes should be an array")
                .len(),
            2
        );
        let evidence = match outcome
            .execution_evidence()
            .expect("successful patch should include internal execution evidence")
        {
            ActionExecutionEvidence::WorkspacePatch(evidence) => evidence,
            ActionExecutionEvidence::ProcessAction(_) => {
                panic!("workspace patch execution must not produce process action evidence")
            }
        };
        assert_eq!(evidence.changes().len(), 2);
        assert_eq!(evidence.changes()[0].relative_path(), "src/lib.rs");
        assert_eq!(evidence.changes()[1].relative_path(), "tests/smoke.rs");
    }

    #[test]
    fn workspace_patch_proposal_reads_preimage_metadata_without_mutation() {
        let temp = TempWorkspace::new("patch-proposal");
        temp.write_text("dir/note.txt", "alpha\nold value\nomega\n");
        let tools = tools_for(temp.path());

        let proposal = patch_proposal(&tools, "dir/note.txt", "old value", "newer value")
            .expect("valid patch should produce proposal");

        assert_eq!(proposal.tool_call_id().as_str(), "call-1");
        assert_eq!(proposal.tool_name().as_str(), WORKSPACE_PATCH_TOOL);
        assert_eq!(proposal.action_kind(), ToolActionKind::WorkspaceWrite);
        assert_eq!(proposal.label(), "workspace patch");
        assert_eq!(proposal.subject(), "dir/note.txt");
        assert!(
            proposal
                .summary()
                .contains("Apply 1 hunk(s) in dir/note.txt")
        );
        let patch = match proposal.evidence() {
            ActionProposalEvidence::WorkspacePatch(patch) => patch,
            ActionProposalEvidence::ProcessAction(_) => {
                panic!("workspace patch proposal must not produce process action evidence")
            }
        };
        assert_eq!(patch.relative_path(), "dir/note.txt");
        assert_eq!(patch.preimage_bytes(), "old value\n".len());
        assert_eq!(patch.replacement_bytes(), "newer value\n".len());
        assert_eq!(patch.file_bytes_before(), 22);
        assert_eq!(patch.file_bytes_after(), 24);
        assert_eq!(
            patch.file_fingerprint_before(),
            &stable_content_fingerprint("alpha\nold value\nomega\n".as_bytes())
        );
        assert_eq!(
            patch.file_fingerprint_after(),
            &stable_content_fingerprint("alpha\nnewer value\nomega\n".as_bytes())
        );
        assert_eq!(
            read_text(&temp.path().join("dir/note.txt")),
            "alpha\nold value\nomega\n"
        );
    }

    #[test]
    fn workspace_patch_execute_after_stale_proposal_mismatch_fails_without_mutation() {
        let temp = TempWorkspace::new("patch-proposal-stale-fail-closed");
        temp.write_text("note.txt", "alpha\nold\nomega\n");
        let tools = tools_for(temp.path());

        let proposal = patch_proposal(&tools, "note.txt", "old", "replacement")
            .expect("initial valid patch should produce proposal");
        let proposed_patch = match proposal.evidence() {
            ActionProposalEvidence::WorkspacePatch(proposed_patch) => proposed_patch,
            ActionProposalEvidence::ProcessAction(_) => {
                panic!("workspace patch proposal must not produce process action evidence")
            }
        };
        assert_eq!(proposed_patch.file_bytes_before(), 16);
        assert_eq!(proposed_patch.file_bytes_after(), 24);
        assert_eq!(
            proposed_patch.file_fingerprint_before(),
            &stable_content_fingerprint("alpha\nold\nomega\n".as_bytes())
        );

        temp.write_text("note.txt", "intro\nalpha\nold\nomega\n");
        let outcome = workspace_patch_blocking_checked(
            &tools.state,
            WorkspacePatchArgs {
                patch: update_patch("note.txt", "old", "replacement"),
            },
            Some(proposed_patch),
            &|| false,
        )
        .expect("uncancelled workspace patch should not return cancellation");

        assert_failed_json_for_tool(
            &outcome,
            WORKSPACE_PATCH_TOOL,
            ERROR_PROPOSAL_MISMATCH,
            Some("note.txt"),
            temp.path(),
        );
        assert_eq!(
            read_text(&temp.path().join("note.txt")),
            "intro\nalpha\nold\nomega\n"
        );
        assert!(outcome.execution_evidence().is_none());
    }

    #[test]
    fn workspace_patch_same_size_stale_proposal_fingerprint_mismatch_fails_without_leak_or_mutation()
     {
        let temp = TempWorkspace::new("patch-proposal-same-size-stale-fail-closed");
        temp.write_text("note.txt", "alpha\nold\nomega\n");
        let tools = tools_for(temp.path());

        let proposal = patch_proposal(&tools, "note.txt", "old", "new")
            .expect("initial valid patch should produce proposal");
        let proposed_patch = match proposal.evidence() {
            ActionProposalEvidence::WorkspacePatch(proposed_patch) => proposed_patch,
            ActionProposalEvidence::ProcessAction(_) => {
                panic!("workspace patch proposal must not produce process action evidence")
            }
        };
        assert_eq!(proposed_patch.file_bytes_before(), 16);
        assert_eq!(proposed_patch.file_bytes_after(), 16);

        temp.write_text("note.txt", "bravo\nold\nomega\n");
        let outcome = workspace_patch_blocking_checked(
            &tools.state,
            WorkspacePatchArgs {
                patch: update_patch("note.txt", "old", "new"),
            },
            Some(proposed_patch),
            &|| false,
        )
        .expect("same-size proposal mismatch should resolve as failed JSON");

        assert_failed_json_for_tool(
            &outcome,
            WORKSPACE_PATCH_TOOL,
            ERROR_PROPOSAL_MISMATCH,
            Some("note.txt"),
            temp.path(),
        );
        assert_no_provider_visible_patch_metadata(&outcome);
        assert_eq!(
            json_content(&outcome)["error"]["message"],
            WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE
        );
        assert_eq!(
            read_text(&temp.path().join("note.txt")),
            "bravo\nold\nomega\n"
        );
        assert!(outcome.execution_evidence().is_none());
    }

    #[test]
    fn workspace_patch_proposal_returns_none_for_invalid_or_stale_patch_without_mutation() {
        let temp = TempWorkspace::new("patch-proposal-none");
        temp.write_text("note.txt", "alpha\nold\nomega\n");
        let tools = tools_for(temp.path());

        assert!(
            patch_proposal(&tools, "note.txt", "missing", "new").is_none(),
            "stale preimage should not produce proposal evidence"
        );
        assert!(
            patch_proposal(&tools, "../note.txt", "old", "new").is_none(),
            "invalid path should not produce proposal evidence"
        );
        assert_eq!(
            read_text(&temp.path().join("note.txt")),
            "alpha\nold\nomega\n"
        );
    }

    #[test]
    fn workspace_patch_stale_and_ambiguous_preimages_fail_without_mutation() {
        let temp = TempWorkspace::new("patch-preimage-failures");
        temp.write_text("stale.txt", "alpha\nbeta\n");
        temp.write_text("ambiguous.txt", "repeat\nmiddle\nrepeat\n");
        let tools = tools_for(temp.path());

        let stale = patch_outcome(&tools, "stale.txt", "gamma", "delta");
        assert_failed_json_for_tool(
            &stale,
            WORKSPACE_PATCH_TOOL,
            ERROR_PREIMAGE_ABSENT,
            Some("stale.txt"),
            temp.path(),
        );
        assert!(stale.execution_evidence().is_none());
        assert_eq!(read_text(&temp.path().join("stale.txt")), "alpha\nbeta\n");

        let ambiguous = patch_outcome(&tools, "ambiguous.txt", "repeat", "single");
        assert_failed_json_for_tool(
            &ambiguous,
            WORKSPACE_PATCH_TOOL,
            ERROR_PREIMAGE_AMBIGUOUS,
            Some("ambiguous.txt"),
            temp.path(),
        );
        assert!(ambiguous.execution_evidence().is_none());
        assert_eq!(
            read_text(&temp.path().join("ambiguous.txt")),
            "repeat\nmiddle\nrepeat\n"
        );

        temp.write_text(
            "ambiguous-status.txt",
            "const ENTRIES: &[Entry] = &[\n    Entry { key: \"status\", value: \"todo\" },\n    Entry { key: \"status\", value: \"todo\" },\n];\n",
        );
        let ambiguous_status = patch_outcome(
            &tools,
            "ambiguous-status.txt",
            "    Entry { key: \"status\", value: \"todo\" },",
            "    Entry { key: \"status\", value: \"done\" },",
        );
        assert_failed_json_for_tool(
            &ambiguous_status,
            WORKSPACE_PATCH_TOOL,
            ERROR_PREIMAGE_AMBIGUOUS,
            Some("ambiguous-status.txt"),
            temp.path(),
        );
        assert!(ambiguous_status.execution_evidence().is_none());
        assert_eq!(
            read_text(&temp.path().join("ambiguous-status.txt")),
            "const ENTRIES: &[Entry] = &[\n    Entry { key: \"status\", value: \"todo\" },\n    Entry { key: \"status\", value: \"todo\" },\n];\n"
        );
    }

    #[test]
    fn workspace_patch_missing_or_ambiguous_after_proposal_still_does_not_write() {
        let temp = TempWorkspace::new("patch-post-proposal-failures");
        temp.write_text("missing-after-proposal.txt", "alpha\nold\nomega\n");
        temp.write_text("ambiguous-after-proposal.txt", "alpha\nold\nomega\n");
        let tools = tools_for(temp.path());

        assert!(
            patch_proposal(&tools, "missing-after-proposal.txt", "old", "new").is_some(),
            "initial patch should be proposable"
        );
        fs::remove_file(temp.path().join("missing-after-proposal.txt"))
            .expect("workspace file should be removable");
        let missing = patch_outcome(&tools, "missing-after-proposal.txt", "old", "new");
        assert_failed_json_for_tool(
            &missing,
            WORKSPACE_PATCH_TOOL,
            ERROR_FILE_NOT_FOUND,
            Some("missing-after-proposal.txt"),
            temp.path(),
        );
        assert!(missing.execution_evidence().is_none());
        assert!(
            !temp.path().join("missing-after-proposal.txt").exists(),
            "missing preimage path should not be recreated"
        );

        assert!(
            patch_proposal(&tools, "ambiguous-after-proposal.txt", "old", "new").is_some(),
            "initial patch should be proposable"
        );
        temp.write_text("ambiguous-after-proposal.txt", "old\nmiddle\nold\n");
        let ambiguous = patch_outcome(&tools, "ambiguous-after-proposal.txt", "old", "new");
        assert_failed_json_for_tool(
            &ambiguous,
            WORKSPACE_PATCH_TOOL,
            ERROR_PREIMAGE_AMBIGUOUS,
            Some("ambiguous-after-proposal.txt"),
            temp.path(),
        );
        assert!(ambiguous.execution_evidence().is_none());
        assert_eq!(
            read_text(&temp.path().join("ambiguous-after-proposal.txt")),
            "old\nmiddle\nold\n"
        );
    }

    #[test]
    fn workspace_patch_rejects_bad_hidden_missing_and_directory_paths_without_mutation() {
        let temp = TempWorkspace::new("patch-path-denied");
        temp.write_text("visible.txt", "old\n");
        temp.write_text(".secret", "old\n");
        fs::create_dir_all(temp.path().join("dir")).expect("directory should be created");
        let tools = tools_for(temp.path());

        for denied in [
            "/etc/passwd".to_owned(),
            "../outside.txt".to_owned(),
            ".secret".to_owned(),
            "dir/./file.txt".to_owned(),
        ] {
            let outcome = patch_outcome(&tools, &denied, "old", "new");
            let expected_path = if denied.starts_with('/') {
                None
            } else {
                Some(denied.as_str())
            };
            assert_failed_json_for_tool(
                &outcome,
                WORKSPACE_PATCH_TOOL,
                ERROR_PATH_DENIED,
                expected_path,
                temp.path(),
            );
        }
        assert_eq!(read_text(&temp.path().join(".secret")), "old\n");

        let missing = patch_outcome(&tools, "missing.txt", "old", "new");
        assert_failed_json_for_tool(
            &missing,
            WORKSPACE_PATCH_TOOL,
            ERROR_FILE_NOT_FOUND,
            Some("missing.txt"),
            temp.path(),
        );
        assert!(!temp.path().join("missing.txt").exists());

        let directory = patch_outcome(&tools, "dir", "old", "new");
        assert_failed_json_for_tool(
            &directory,
            WORKSPACE_PATCH_TOOL,
            ERROR_NOT_FILE,
            Some("dir"),
            temp.path(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_patch_rejects_symlink_path_without_following_it() {
        let temp = TempWorkspace::new("patch-symlink");
        temp.write_text("target.txt", "old\n");
        symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
            .expect("symlink should be created");
        let tools = tools_for(temp.path());

        let outcome = patch_outcome(&tools, "link.txt", "old", "new");

        assert_failed_json_for_tool(
            &outcome,
            WORKSPACE_PATCH_TOOL,
            ERROR_PATH_DENIED,
            Some("link.txt"),
            temp.path(),
        );
        assert_eq!(read_text(&temp.path().join("target.txt")), "old\n");
    }

    #[test]
    fn workspace_patch_rejects_binary_and_limit_failures_without_mutation() {
        let temp = TempWorkspace::new("patch-binary-limits");
        temp.write_bytes("binary.txt", b"old\0value\n");
        temp.write_text("large-read.txt", "abcdef\n");
        temp.write_text("large-write.txt", "b\n");
        temp.write_text("large-payload.txt", "abc\n");

        let binary_tools = tools_for(temp.path());
        let binary = patch_outcome(&binary_tools, "binary.txt", "old", "new");
        assert_failed_json_for_tool(
            &binary,
            WORKSPACE_PATCH_TOOL,
            ERROR_NOT_UTF8,
            Some("binary.txt"),
            temp.path(),
        );
        assert_eq!(
            fs::read(temp.path().join("binary.txt")).expect("binary file should be readable"),
            b"old\0value\n"
        );

        let read_limited = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_read_bytes: 3,
                    ..WorkspaceToolLimits::default()
                },
            ),
        )
        .expect("workspace tools should construct");
        let too_large_read = patch_outcome(&read_limited, "large-read.txt", "abc", "x");
        assert_failed_json_for_tool(
            &too_large_read,
            WORKSPACE_PATCH_TOOL,
            ERROR_FILE_TOO_LARGE,
            Some("large-read.txt"),
            temp.path(),
        );
        assert_eq!(read_text(&temp.path().join("large-read.txt")), "abcdef\n");

        let payload_limited = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_patch_bytes: 3,
                    ..WorkspaceToolLimits::default()
                },
            ),
        )
        .expect("workspace tools should construct");
        let too_large_payload = patch_outcome(&payload_limited, "large-payload.txt", "ab", "cd");
        assert_failed_json_for_tool(
            &too_large_payload,
            WORKSPACE_PATCH_TOOL,
            ERROR_INVALID_ARGUMENTS,
            None,
            temp.path(),
        );
        assert_eq!(read_text(&temp.path().join("large-payload.txt")), "abc\n");

        let write_limited = ReadOnlyWorkspaceTools::new(
            WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
                WorkspaceToolLimits {
                    max_write_bytes: 4,
                    ..WorkspaceToolLimits::default()
                },
            ),
        )
        .expect("workspace tools should construct");
        let too_large_write = patch_outcome(&write_limited, "large-write.txt", "b", "bcdef");
        assert_failed_json_for_tool(
            &too_large_write,
            WORKSPACE_PATCH_TOOL,
            ERROR_FILE_TOO_LARGE,
            Some("large-write.txt"),
            temp.path(),
        );
        assert_eq!(read_text(&temp.path().join("large-write.txt")), "b\n");
    }

    #[test]
    fn workspace_patch_cancellation_before_write_keeps_file_unchanged() {
        let temp = TempWorkspace::new("patch-cancel-before-write");
        temp.write_text("note.txt", "alpha\nold\nomega\n");
        let tools = tools_for(temp.path());
        let args = WorkspacePatchArgs {
            patch: update_patch("note.txt", "old", "new"),
        };
        let checks = Cell::new(0);
        let is_cancelled = || {
            let next = checks.get() + 1;
            checks.set(next);
            next >= 6
        };

        let err = workspace_patch_blocking_checked(&tools.state, args, None, &is_cancelled)
            .expect_err("cancellation before write should abort patch execution");

        assert!(matches!(err, ToolExecutionError::Cancelled));
        assert_eq!(
            read_text(&temp.path().join("note.txt")),
            "alpha\nold\nomega\n"
        );
    }

    fn mark_patch_cancelled_after_write(path: &Path) {
        fs::write(
            path.with_file_name("cancel-after-write.marker"),
            "cancelled",
        )
        .expect("post-write cancellation marker should be written");
    }

    #[test]
    fn workspace_patch_cancellation_after_write_returns_durable_outcome() {
        let temp = TempWorkspace::new("patch-cancel-after-write");
        temp.write_text("note.txt", "alpha\nold\nomega\n");
        let tools = tools_for(temp.path());
        let note_path =
            fs::canonicalize(temp.path().join("note.txt")).expect("note path should canonicalize");
        install_patch_test_after_write_hook(note_path, mark_patch_cancelled_after_write);
        let args = WorkspacePatchArgs {
            patch: update_patch("note.txt", "old", "new"),
        };
        let cancel_marker = temp.path().join("cancel-after-write.marker");
        let is_cancelled = || cancel_marker.exists();

        let outcome = workspace_patch_blocking_checked(&tools.state, args, None, &is_cancelled)
            .expect("cancellation after write must return durable patch outcome");

        assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
        assert_eq!(
            read_text(&temp.path().join("note.txt")),
            "alpha\nnew\nomega\n"
        );
        let evidence = match outcome
            .execution_evidence()
            .expect("successful patch should include internal execution evidence")
        {
            ActionExecutionEvidence::WorkspacePatch(evidence) => evidence,
            ActionExecutionEvidence::ProcessAction(_) => {
                panic!("workspace patch execution must not produce process action evidence")
            }
        };
        assert_eq!(evidence.relative_path(), "note.txt");
        assert_eq!(evidence.file_bytes_after(), "alpha\nnew\nomega\n".len());
    }

    #[cfg(unix)]
    #[test]
    fn search_text_rejects_requested_symlink_without_following_it() {
        let temp = TempWorkspace::new("search-symlink");
        temp.write_text("target.txt", "needle\n");
        symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
            .expect("symlink should be created");
        let tools = tools_for(temp.path());

        let outcome = search_outcome(&tools, "needle", Some("link.txt"), None);

        assert_failed_json_for_tool(
            &outcome,
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_PATH_DENIED,
            Some("link.txt"),
            temp.path(),
        );
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
    fn list_and_search_executors_return_cancelled_when_token_is_cancelled() {
        let temp = TempWorkspace::new("list-search-cancelled");
        temp.write_text("note.txt", "needle\n");
        let tools = tools_for(temp.path());
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio runtime should build");

        let list_executor = ListDirExecutor {
            state: Arc::clone(&tools.state),
        };
        let list_call = pending_call_for(WORKSPACE_LIST_DIR_TOOL, json!({ "path": "." }));
        let list_err = runtime
            .block_on(list_executor.execute(list_call, ToolExecutionContext::new(token.clone())))
            .expect_err("cancelled list execution should return cancellation error");
        assert!(matches!(list_err, ToolExecutionError::Cancelled));

        let search_executor = SearchTextExecutor {
            state: Arc::clone(&tools.state),
        };
        let search_call =
            pending_call_for(WORKSPACE_SEARCH_TEXT_TOOL, json!({ "query": "needle" }));
        let search_err = runtime
            .block_on(search_executor.execute(search_call, ToolExecutionContext::new(token)))
            .expect_err("cancelled search execution should return cancellation error");
        assert!(matches!(search_err, ToolExecutionError::Cancelled));
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
