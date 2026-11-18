//! Session-scoped handles for processes that outlive one `run_process` call.
//!
//! The process backend owns the live child. This module is the typed view the
//! runtime, the model tools, and `/tasks` share.

use std::fmt;

/// Identifier returned to the model when a background process is retained.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackgroundProcessId(String);

impl BackgroundProcessId {
    /// Accepts one model-supplied id.
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-');
        if !valid {
            return Err(
                "background process id must be 1-64 ASCII letters, digits, or hyphens".to_owned(),
            );
        }
        Ok(Self(value))
    }

    /// Borrows the id text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackgroundProcessId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Lifecycle of one retained background process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundProcessState {
    /// The process group is still running.
    Running {
        /// Operating-system pid of the retained process.
        pid: u32,
    },
    /// The process exited on its own.
    Exited {
        /// Exit code, or `-1` when the platform did not report one.
        code: i32,
    },
    /// `stop_background_process` or session teardown killed the process group.
    Killed,
}

impl BackgroundProcessState {
    /// Returns the short status used in tool results and `/tasks`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Running { pid } => format!("running pid {pid}"),
            Self::Exited { code } => format!("exited {code}"),
            Self::Killed => "killed".to_owned(),
        }
    }
}

/// Compact row for a retained background process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundProcessSummary {
    id: BackgroundProcessId,
    command: String,
    cwd: Option<String>,
    state: BackgroundProcessState,
}

impl BackgroundProcessSummary {
    /// Creates one list row.
    #[must_use]
    pub fn new(
        id: BackgroundProcessId,
        command: impl Into<String>,
        cwd: Option<String>,
        state: BackgroundProcessState,
    ) -> Self {
        Self {
            id,
            command: command.into(),
            cwd,
            state,
        }
    }

    /// Returns the process id.
    #[must_use]
    pub fn id(&self) -> &BackgroundProcessId {
        &self.id
    }

    /// Returns the command text shown to the model.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Returns the workspace-relative cwd, when the call set one.
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub fn state(&self) -> &BackgroundProcessState {
        &self.state
    }
}

/// Captured output for one retained background process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundProcessOutput {
    summary: BackgroundProcessSummary,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl BackgroundProcessOutput {
    /// Creates a read result from the retained capture.
    #[must_use]
    pub fn new(
        summary: BackgroundProcessSummary,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    ) -> Self {
        Self {
            summary,
            stdout: stdout.into(),
            stderr: stderr.into(),
            stdout_truncated,
            stderr_truncated,
        }
    }

    /// Returns the process row.
    #[must_use]
    pub fn summary(&self) -> &BackgroundProcessSummary {
        &self.summary
    }

    /// Returns captured stdout since the process started, bounded by the original call.
    #[must_use]
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    /// Returns captured stderr since the process started, bounded by the original call.
    #[must_use]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// Returns whether stdout capture hit its limit.
    #[must_use]
    pub const fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }

    /// Returns whether stderr capture hit its limit.
    #[must_use]
    pub const fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }
}

/// Result of asking a retained process to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundProcessStop {
    summary: BackgroundProcessSummary,
    changed: bool,
}

impl BackgroundProcessStop {
    /// Creates a stop result.
    ///
    /// `changed` is true only when this call moved a running process to killed.
    #[must_use]
    pub fn new(summary: BackgroundProcessSummary, changed: bool) -> Self {
        Self { summary, changed }
    }

    /// Returns the process row after the stop attempt.
    #[must_use]
    pub fn summary(&self) -> &BackgroundProcessSummary {
        &self.summary
    }

    /// Returns whether this call killed a process that was still running.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }
}

/// A model-supplied id did not match a retained process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundProcessLookupError {
    id: BackgroundProcessId,
    known: Vec<BackgroundProcessId>,
}

impl BackgroundProcessLookupError {
    /// Creates a lookup miss that names the id and the ids still retained.
    #[must_use]
    pub fn new(id: BackgroundProcessId, known: Vec<BackgroundProcessId>) -> Self {
        Self { id, known }
    }

    /// Returns the text the model should see.
    #[must_use]
    pub fn message(&self) -> String {
        if self.known.is_empty() {
            format!(
                "No background process `{}`. No background processes are retained.",
                self.id
            )
        } else {
            let known = self
                .known
                .iter()
                .map(BackgroundProcessId::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            format!("No background process `{}`. Known: {known}.", self.id)
        }
    }
}

/// Session-owned control surface for retained background processes.
///
/// Implementations live with the process backend that holds the child.
pub trait BackgroundProcessControl: Send + Sync {
    /// Lists retained processes, refreshing exit state first.
    fn list(&self) -> Vec<BackgroundProcessSummary>;

    /// Returns one process and the output captured since it started.
    fn read(
        &self,
        id: &BackgroundProcessId,
    ) -> Result<BackgroundProcessOutput, BackgroundProcessLookupError>;

    /// Kills one retained process group.
    ///
    /// A process that has already exited or been killed returns that state
    /// with `changed == false`.
    fn stop(
        &self,
        id: &BackgroundProcessId,
    ) -> Result<BackgroundProcessStop, BackgroundProcessLookupError>;
}
