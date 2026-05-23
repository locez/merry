//! Provider-neutral process action protocol values.
//!
//! This module describes process intent and execution evidence only. It does
//! not execute commands, spawn subprocesses, or model a shell.

use std::{
    future::Future,
    path::{Component, Path},
    pin::Pin,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Maximum accepted byte length for one argv item in a process intent.
pub const MAX_PROCESS_ARG_BYTES: usize = 4096;
/// Maximum accepted number of argv items in a process intent.
pub const MAX_PROCESS_ARGV_ITEMS: usize = 256;
/// Maximum accepted byte length for a workspace-relative process cwd.
pub const MAX_PROCESS_CWD_BYTES: usize = 4096;
/// Maximum accepted byte length for inline stdin text.
pub const MAX_PROCESS_STDIN_TEXT_BYTES: usize = 64 * 1024;
/// Maximum accepted captured byte limit per process output stream.
pub const MAX_PROCESS_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;

/// Minimal process environment policy for SP1.
///
/// `Empty` means no inherited environment and no runtime-supplied overrides.
/// Future slices can add explicit allowlist or override variants without
/// changing the process intent shape.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessEnvPolicy {
    /// No inherited environment and no overrides.
    #[default]
    Empty,
    /// Test-only stand-in for a future non-empty environment policy.
    #[cfg(test)]
    NonEmptyForTest,
}

impl ProcessEnvPolicy {
    /// Creates the no-environment policy.
    #[must_use]
    pub const fn empty() -> Self {
        Self::Empty
    }
}

/// Explicit admission for the accepted local workspace process lane.
///
/// This value is intentionally small and declarative. It records that the
/// caller has selected Merry's current CLI bubblewrap profile and accepted the
/// local workspace process risk for that profile; it is not proof that any
/// process is actually confined. Runtime code treats it only as construction-
/// time admission for the narrow local workspace process predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptedLocalWorkspaceProcessAdmission {
    sandbox_profile: LocalWorkspaceProcessSandboxProfile,
}

impl AcceptedLocalWorkspaceProcessAdmission {
    /// Creates admission for the Merry CLI bubblewrap v1 sandbox profile.
    ///
    /// Calling this explicitly accepts the local workspace process risk for the
    /// declared profile.
    #[must_use]
    pub const fn accept_cli_bwrap_v1() -> Self {
        Self {
            sandbox_profile: LocalWorkspaceProcessSandboxProfile::CliBwrapV1,
        }
    }

    /// Returns the declared sandbox profile for this admission.
    #[must_use]
    pub const fn sandbox_profile(self) -> LocalWorkspaceProcessSandboxProfile {
        self.sandbox_profile
    }
}

/// Declared sandbox/profile associated with local workspace process admission.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalWorkspaceProcessSandboxProfile {
    /// Merry CLI bubblewrap profile version 1.
    CliBwrapV1,
}

/// Boxed process runner future used for object-safe runtime process boundaries.
///
/// The runner boundary is runtime-owned and provider-neutral. It accepts only a
/// validated [`ProcessActionIntent`] and a cancellation-aware context.
pub type ProcessRunnerFuture<'a> = Pin<Box<dyn Future<Output = ProcessRunnerResult> + Send + 'a>>;

/// Result returned by a runtime-owned process runner.
pub type ProcessRunnerResult = Result<ProcessRunnerOutput, ProcessRunnerError>;

/// Context passed to a process runner.
#[derive(Debug, Clone)]
pub struct ProcessRunnerContext {
    cancellation_token: CancellationToken,
}

impl ProcessRunnerContext {
    /// Creates a process runner context with the provided cancellation token.
    #[must_use]
    pub fn new(cancellation_token: CancellationToken) -> Self {
        Self { cancellation_token }
    }

    /// Returns the cancellation token for this process action.
    #[must_use]
    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }
}

/// Object-safe runtime process runner boundary.
///
/// Implementations for SP2 are fakes or adapters supplied by tests and higher
/// layers. This trait must not imply shell execution, raw shell parsing, or
/// provider-specific behavior.
pub trait ProcessRunner: Send + Sync {
    /// Runs a validated process intent and returns provider-neutral output.
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a>;
}

/// Provider-neutral, typed intent for a local process action.
///
/// The argv vector is intentionally open and does not enumerate allowed
/// commands. This value is proposal evidence only in SP1; it is not an
/// executor and must not be treated as authorization to spawn a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessActionIntent {
    summary: String,
    argv: Vec<String>,
    cwd: Option<String>,
    env_policy: ProcessEnvPolicy,
    stdin_text: Option<String>,
    stdout_limit_bytes: usize,
    stderr_limit_bytes: usize,
}

impl ProcessActionIntent {
    /// Creates a validated process action intent.
    pub fn new(
        argv: Vec<String>,
        cwd: Option<String>,
        env_policy: ProcessEnvPolicy,
        stdin_text: Option<String>,
        stdout_limit_bytes: usize,
        stderr_limit_bytes: usize,
    ) -> Result<Self, ProcessActionError> {
        validate_argv(&argv)?;
        let cwd = validate_cwd(cwd)?;
        validate_stdin_text(stdin_text.as_deref())?;
        validate_output_limit("stdout_limit_bytes", stdout_limit_bytes)?;
        validate_output_limit("stderr_limit_bytes", stderr_limit_bytes)?;
        let summary = summarize_intent(&argv, cwd.as_deref());

        Ok(Self {
            summary,
            argv,
            cwd,
            env_policy,
            stdin_text,
            stdout_limit_bytes,
            stderr_limit_bytes,
        })
    }

    /// Returns a compact deterministic summary of the process intent.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Returns the exact argv vector.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Returns the workspace-relative cwd, or `None` for the workspace root.
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Returns the typed environment policy.
    #[must_use]
    pub const fn env_policy(&self) -> ProcessEnvPolicy {
        self.env_policy
    }

    /// Returns optional inline stdin text.
    #[must_use]
    pub fn stdin_text(&self) -> Option<&str> {
        self.stdin_text.as_deref()
    }

    /// Returns the same intent identity without inline stdin payload.
    ///
    /// Internal action audit stores proposal identity for policy/debugging, but
    /// not process input payloads.
    #[must_use]
    pub(crate) fn without_stdin_text(&self) -> Self {
        Self {
            summary: self.summary.clone(),
            argv: self.argv.clone(),
            cwd: self.cwd.clone(),
            env_policy: self.env_policy,
            stdin_text: None,
            stdout_limit_bytes: self.stdout_limit_bytes,
            stderr_limit_bytes: self.stderr_limit_bytes,
        }
    }

    /// Returns the stdout capture limit in bytes.
    #[must_use]
    pub const fn stdout_limit_bytes(&self) -> usize {
        self.stdout_limit_bytes
    }

    /// Returns the stderr capture limit in bytes.
    #[must_use]
    pub const fn stderr_limit_bytes(&self) -> usize {
        self.stderr_limit_bytes
    }
}

/// Bounded output returned by a process runner.
///
/// The payload strings are intended for the result artifact only. Internal
/// execution audit evidence stores byte counts and truncation flags, not these
/// payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRunnerOutput {
    status: ProcessExitStatus,
    stdout_text: String,
    stdout_bytes: usize,
    stdout_truncated: bool,
    stderr_text: String,
    stderr_bytes: usize,
    stderr_truncated: bool,
}

impl ProcessRunnerOutput {
    /// Creates validated process runner output for a previously validated intent.
    pub fn new(
        intent: &ProcessActionIntent,
        status: ProcessExitStatus,
        stdout_text: impl Into<String>,
        stdout_truncated: bool,
        stderr_text: impl Into<String>,
        stderr_truncated: bool,
    ) -> Result<Self, ProcessActionError> {
        let stdout_text = stdout_text.into();
        let stderr_text = stderr_text.into();
        validate_captured_bytes(
            "stdout_bytes",
            stdout_text.len(),
            intent.stdout_limit_bytes(),
        )?;
        validate_captured_bytes(
            "stderr_bytes",
            stderr_text.len(),
            intent.stderr_limit_bytes(),
        )?;

        Ok(Self {
            status,
            stdout_bytes: stdout_text.len(),
            stdout_text,
            stdout_truncated,
            stderr_bytes: stderr_text.len(),
            stderr_text,
            stderr_truncated,
        })
    }

    /// Returns the provider-neutral completion status.
    #[must_use]
    pub const fn status(&self) -> ProcessExitStatus {
        self.status
    }

    /// Returns bounded stdout payload for the result artifact.
    #[must_use]
    pub fn stdout_text(&self) -> &str {
        &self.stdout_text
    }

    /// Returns captured stdout byte count.
    #[must_use]
    pub const fn stdout_bytes(&self) -> usize {
        self.stdout_bytes
    }

    /// Returns whether stdout capture was truncated.
    #[must_use]
    pub const fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }

    /// Returns bounded stderr payload for the result artifact.
    #[must_use]
    pub fn stderr_text(&self) -> &str {
        &self.stderr_text
    }

    /// Returns captured stderr byte count.
    #[must_use]
    pub const fn stderr_bytes(&self) -> usize {
        self.stderr_bytes
    }

    /// Returns whether stderr capture was truncated.
    #[must_use]
    pub const fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }

    /// Returns whether the process output represents successful completion.
    #[must_use]
    pub const fn ok(&self) -> bool {
        matches!(self.status, ProcessExitStatus::Exited(0))
    }

    /// Builds payload-free internal execution evidence for the given intent.
    pub fn execution_evidence(
        &self,
        intent: &ProcessActionIntent,
    ) -> Result<ProcessExecutionEvidence, ProcessActionError> {
        ProcessExecutionEvidence::new(
            intent,
            self.status,
            self.stdout_bytes,
            self.stdout_truncated,
            self.stderr_bytes,
            self.stderr_truncated,
        )
    }
}

/// Provider-neutral process completion status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessExitStatus {
    /// The process exited with an integer code.
    Exited(i32),
    /// The process was cancelled before normal completion.
    Cancelled,
    /// The process could not be started by the future process executor.
    FailedToStart,
    /// The process runner reported a domain failure before normal completion.
    DomainFailed,
}

impl ProcessExitStatus {
    /// Returns the exit code when the process reached normal exit.
    #[must_use]
    pub const fn exit_code(self) -> Option<i32> {
        match self {
            Self::Exited(code) => Some(code),
            Self::Cancelled | Self::FailedToStart | Self::DomainFailed => None,
        }
    }
}

/// Infrastructure errors returned by a process runner.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProcessRunnerError {
    /// Process runner was cancelled cooperatively before producing output.
    #[error("process runner cancelled")]
    Cancelled,

    /// The runner infrastructure failed before producing durable output.
    #[error("process runner infrastructure error: {message}")]
    Infrastructure {
        /// Actionable infrastructure failure detail.
        message: String,
    },
}

impl ProcessRunnerError {
    /// Creates an infrastructure process runner error.
    #[must_use]
    pub fn infrastructure(message: impl Into<String>) -> Self {
        Self::Infrastructure {
            message: message.into(),
        }
    }
}

/// Provider-neutral evidence recorded after a process action executes.
///
/// This stores bounded metadata only: the validated intent identity, completion
/// status, captured byte counts, and truncation flags. It contains no provider
/// wire data and no stdout/stderr payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessExecutionEvidence {
    intent_summary: String,
    argv: Vec<String>,
    cwd: Option<String>,
    status: ProcessExitStatus,
    stdout_bytes: usize,
    stdout_truncated: bool,
    stderr_bytes: usize,
    stderr_truncated: bool,
}

impl ProcessExecutionEvidence {
    /// Creates validated process execution evidence for a previously proposed intent.
    pub fn new(
        intent: &ProcessActionIntent,
        status: ProcessExitStatus,
        stdout_bytes: usize,
        stdout_truncated: bool,
        stderr_bytes: usize,
        stderr_truncated: bool,
    ) -> Result<Self, ProcessActionError> {
        validate_captured_bytes("stdout_bytes", stdout_bytes, intent.stdout_limit_bytes())?;
        validate_captured_bytes("stderr_bytes", stderr_bytes, intent.stderr_limit_bytes())?;

        Ok(Self {
            intent_summary: intent.summary().to_owned(),
            argv: intent.argv().to_vec(),
            cwd: intent.cwd().map(str::to_owned),
            status,
            stdout_bytes,
            stdout_truncated,
            stderr_bytes,
            stderr_truncated,
        })
    }

    /// Returns the compact process intent summary.
    #[must_use]
    pub fn intent_summary(&self) -> &str {
        &self.intent_summary
    }

    /// Returns the exact argv copied from the validated intent.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Returns the workspace-relative cwd copied from the validated intent.
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Returns the provider-neutral completion status.
    #[must_use]
    pub const fn status(&self) -> ProcessExitStatus {
        self.status
    }

    /// Returns the exit code when the process reached normal exit.
    #[must_use]
    pub const fn exit_code(&self) -> Option<i32> {
        self.status.exit_code()
    }

    /// Returns captured stdout byte count.
    #[must_use]
    pub const fn stdout_bytes(&self) -> usize {
        self.stdout_bytes
    }

    /// Returns whether stdout capture was truncated.
    #[must_use]
    pub const fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }

    /// Returns captured stderr byte count.
    #[must_use]
    pub const fn stderr_bytes(&self) -> usize {
        self.stderr_bytes
    }

    /// Returns whether stderr capture was truncated.
    #[must_use]
    pub const fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }

    pub(crate) fn matches_intent(&self, intent: &ProcessActionIntent) -> bool {
        self.intent_summary == intent.summary()
            && self.argv == intent.argv()
            && self.cwd.as_deref() == intent.cwd()
            && self.stdout_bytes <= intent.stdout_limit_bytes()
            && self.stderr_bytes <= intent.stderr_limit_bytes()
    }
}

/// Coarse runtime-owned classification for a proposed process argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessIntentClass {
    /// Read-only inspection and navigation commands with no workspace effect.
    Informational,
    /// Bounded local commands expected to read/write build artifacts.
    LocalWorkspaceEffect,
    /// No specific policy class is known.
    Unknown,
    /// The argv is blocked by hard process policy.
    Forbidden,
}

/// Classifies a process intent using validated argv only.
#[must_use]
pub fn classify_process_intent(intent: &ProcessActionIntent) -> ProcessIntentClass {
    classify_process_argv(intent.argv())
}

fn classify_process_argv(argv: &[String]) -> ProcessIntentClass {
    if is_informational_process_argv(argv) {
        return ProcessIntentClass::Informational;
    }
    if is_local_workspace_effect_process_argv(argv) {
        return ProcessIntentClass::LocalWorkspaceEffect;
    }
    if is_forbidden_process_argv(argv) {
        return ProcessIntentClass::Forbidden;
    }
    ProcessIntentClass::Unknown
}

fn is_informational_process_argv(argv: &[String]) -> bool {
    match argv {
        [executable, version]
            if executable_token_is(executable, "rustc") && version.as_str() == "--version" =>
        {
            true
        }
        [executable, rg_arg] if executable_token_is(executable, "rg") => {
            is_read_only_rg_single_argument(rg_arg)
        }
        _ => false,
    }
}

fn is_read_only_rg_single_argument(argument: &str) -> bool {
    argument == "--version" || argument == "--files" || is_simple_rg_literal_pattern(argument)
}

fn is_simple_rg_literal_pattern(pattern: &str) -> bool {
    !pattern.starts_with('-') && !pattern.chars().any(is_rg_regex_metacharacter)
}

fn is_rg_regex_metacharacter(character: char) -> bool {
    matches!(
        character,
        '\\' | '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
    )
}

fn is_local_workspace_effect_process_argv(argv: &[String]) -> bool {
    matches!(
        argv,
        [cargo, test, package_flag, package]
            if executable_token_is(cargo, "cargo")
                && test.as_str() == "test"
                && (package_flag.as_str() == "-p" || package_flag.as_str() == "--package")
                && package.as_str() == "merry-runtime"
    )
}

fn is_forbidden_process_argv(argv: &[String]) -> bool {
    let Some(executable) = argv.first().map(|argument| executable_name(argument)) else {
        return false;
    };

    FORBIDDEN_PROCESS_EXECUTABLES.contains(&executable.as_str())
}

fn executable_name(argument: &str) -> String {
    argument
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argument)
        .to_ascii_lowercase()
}

fn executable_token_is(argument: &str, expected: &str) -> bool {
    argument == expected
}

const FORBIDDEN_PROCESS_EXECUTABLES: &[&str] = &[
    "bash",
    "chmod",
    "chown",
    "cmd",
    "cp",
    "curl",
    "fish",
    "git",
    "mv",
    "nc",
    "netcat",
    "node",
    "perl",
    "powershell",
    "python",
    "python3",
    "pwsh",
    "rm",
    "rsync",
    "ruby",
    "scp",
    "sh",
    "ssh",
    "su",
    "sudo",
    "wget",
    "zsh",
];

/// Returns whether a process intent may enter the SP3-A low-risk process lane.
///
/// The first admitted lane is intentionally fail-closed. It is a small read-only
/// injected-runner allowset, not a general command risk model:
/// no inherited/supplied environment, no stdin text, and only deterministic
/// read-only argv shapes explicitly recognized by SP3-A. Future slices can
/// expand this predicate only with a real policy model and execution evidence
/// for the additional process inputs.
#[must_use]
pub fn is_low_risk_process_action_intent(intent: &ProcessActionIntent) -> bool {
    intent.env_policy() == ProcessEnvPolicy::Empty
        && intent.stdin_text().is_none()
        && classify_process_intent(intent) == ProcessIntentClass::Informational
}

/// Validation errors for provider-neutral process action values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProcessActionError {
    /// The argv vector was invalid.
    #[error("process action argv {reason}")]
    InvalidArgv {
        /// Validation failure detail.
        reason: &'static str,
    },

    /// One argv item was invalid.
    #[error("process action argv[{index}] {reason}")]
    InvalidArgument {
        /// Invalid argv index.
        index: usize,
        /// Validation failure detail.
        reason: &'static str,
    },

    /// The workspace-relative cwd was invalid.
    #[error("process action cwd {reason}")]
    InvalidCwd {
        /// Validation failure detail.
        reason: &'static str,
    },

    /// Inline stdin text was invalid.
    #[error("process action stdin_text {reason}")]
    InvalidStdinText {
        /// Validation failure detail.
        reason: &'static str,
    },

    /// An output capture limit was invalid.
    #[error("process action {field} {reason}")]
    InvalidOutputLimit {
        /// Invalid field name.
        field: &'static str,
        /// Validation failure detail.
        reason: &'static str,
    },

    /// Execute-time evidence was inconsistent with the validated intent.
    #[error("process execution evidence {field} {reason}")]
    InvalidExecutionEvidence {
        /// Invalid field name.
        field: &'static str,
        /// Validation failure detail.
        reason: &'static str,
    },
}

fn validate_argv(argv: &[String]) -> Result<(), ProcessActionError> {
    if argv.is_empty() {
        return Err(ProcessActionError::InvalidArgv {
            reason: "must not be empty",
        });
    }
    if argv.len() > MAX_PROCESS_ARGV_ITEMS {
        return Err(ProcessActionError::InvalidArgv {
            reason: "contains too many arguments",
        });
    }
    for (index, argument) in argv.iter().enumerate() {
        if argument.is_empty() {
            return Err(ProcessActionError::InvalidArgument {
                index,
                reason: "must not be empty",
            });
        }
        if argument.len() > MAX_PROCESS_ARG_BYTES {
            return Err(ProcessActionError::InvalidArgument {
                index,
                reason: "exceeds the byte limit",
            });
        }
        if argument.chars().any(char::is_control) {
            return Err(ProcessActionError::InvalidArgument {
                index,
                reason: "must not contain control characters",
            });
        }
    }

    Ok(())
}

fn validate_cwd(cwd: Option<String>) -> Result<Option<String>, ProcessActionError> {
    let Some(value) = cwd else {
        return Ok(None);
    };

    if value.trim().is_empty() {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must not be blank",
        });
    }
    if value.len() > MAX_PROCESS_CWD_BYTES {
        return Err(ProcessActionError::InvalidCwd {
            reason: "exceeds the byte limit",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must not contain control characters",
        });
    }
    if value.split('/').any(str::is_empty) {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must not contain empty path segments",
        });
    }
    if value != "."
        && value
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must not contain dot segments",
        });
    }

    let path = Path::new(&value);
    if path.is_absolute() {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must be relative",
        });
    }

    let mut saw_component = false;
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if value.to_str().is_none() {
                    return Err(ProcessActionError::InvalidCwd {
                        reason: "components must be UTF-8",
                    });
                }
                saw_component = true;
            }
            Component::CurDir if value == "." => {}
            Component::CurDir | Component::ParentDir => {
                return Err(ProcessActionError::InvalidCwd {
                    reason: "must not contain dot segments",
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ProcessActionError::InvalidCwd {
                    reason: "must be relative",
                });
            }
        }
    }

    if !saw_component && value != "." {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must name a workspace directory",
        });
    }

    Ok(Some(value))
}

fn validate_stdin_text(stdin_text: Option<&str>) -> Result<(), ProcessActionError> {
    if stdin_text.is_some_and(|text| text.len() > MAX_PROCESS_STDIN_TEXT_BYTES) {
        return Err(ProcessActionError::InvalidStdinText {
            reason: "exceeds the byte limit",
        });
    }
    Ok(())
}

fn validate_output_limit(field: &'static str, limit: usize) -> Result<(), ProcessActionError> {
    if limit == 0 {
        return Err(ProcessActionError::InvalidOutputLimit {
            field,
            reason: "must be greater than zero",
        });
    }
    if limit > MAX_PROCESS_OUTPUT_LIMIT_BYTES {
        return Err(ProcessActionError::InvalidOutputLimit {
            field,
            reason: "exceeds the byte limit",
        });
    }
    Ok(())
}

fn validate_captured_bytes(
    field: &'static str,
    bytes: usize,
    limit: usize,
) -> Result<(), ProcessActionError> {
    if bytes > limit {
        return Err(ProcessActionError::InvalidExecutionEvidence {
            field,
            reason: "must not exceed the intent output limit",
        });
    }
    Ok(())
}

fn summarize_intent(argv: &[String], cwd: Option<&str>) -> String {
    let executable = argv
        .first()
        .expect("process intent summary is built after argv validation");
    let cwd = cwd.unwrap_or(".");
    format!(
        "process argv[0]={executable}; argc={}; cwd={cwd}",
        argv.len()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_PROCESS_OUTPUT_LIMIT_BYTES, ProcessActionError, ProcessActionIntent, ProcessEnvPolicy,
        ProcessExecutionEvidence, ProcessExitStatus, ProcessIntentClass, classify_process_intent,
        is_low_risk_process_action_intent,
    };

    fn intent() -> ProcessActionIntent {
        ProcessActionIntent::new(
            vec!["cargo".to_owned(), "test".to_owned()],
            Some("crates/merry-runtime".to_owned()),
            ProcessEnvPolicy::empty(),
            Some("stdin text".to_owned()),
            1024,
            2048,
        )
        .expect("valid process intent")
    }

    #[test]
    fn process_action_intent_validates_argv_cwd_and_limits() {
        let valid = intent();
        assert_eq!(valid.argv(), ["cargo", "test"]);
        assert_eq!(valid.cwd(), Some("crates/merry-runtime"));
        assert_eq!(valid.env_policy(), ProcessEnvPolicy::Empty);
        assert_eq!(valid.stdin_text(), Some("stdin text"));
        assert_eq!(valid.stdout_limit_bytes(), 1024);
        assert_eq!(valid.stderr_limit_bytes(), 2048);
        assert!(valid.summary().contains("argv[0]=cargo"));

        let empty_argv = ProcessActionIntent::new(
            Vec::new(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect_err("empty argv is rejected");
        assert!(matches!(empty_argv, ProcessActionError::InvalidArgv { .. }));

        let empty_arg = ProcessActionIntent::new(
            vec!["cargo".to_owned(), String::new()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect_err("empty argv item is rejected");
        assert!(matches!(
            empty_arg,
            ProcessActionError::InvalidArgument { index: 1, .. }
        ));

        for cwd in [
            Some("/tmp".to_owned()),
            Some("../outside".to_owned()),
            Some("dir/../outside".to_owned()),
            Some("bad\ncwd".to_owned()),
        ] {
            let error = ProcessActionIntent::new(
                vec!["cargo".to_owned()],
                cwd,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .expect_err("bad cwd is rejected");
            assert!(matches!(error, ProcessActionError::InvalidCwd { .. }));
        }

        let zero_limit = ProcessActionIntent::new(
            vec!["cargo".to_owned()],
            Some(".".to_owned()),
            ProcessEnvPolicy::empty(),
            None,
            0,
            1024,
        )
        .expect_err("zero output limit is rejected");
        assert!(matches!(
            zero_limit,
            ProcessActionError::InvalidOutputLimit {
                field: "stdout_limit_bytes",
                ..
            }
        ));

        let oversized_limit = ProcessActionIntent::new(
            vec!["cargo".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            MAX_PROCESS_OUTPUT_LIMIT_BYTES + 1,
        )
        .expect_err("oversized output limit is rejected");
        assert!(matches!(
            oversized_limit,
            ProcessActionError::InvalidOutputLimit {
                field: "stderr_limit_bytes",
                ..
            }
        ));
    }

    #[test]
    fn process_execution_evidence_records_intent_identity_and_output_metadata() {
        let intent = intent();
        let evidence = ProcessExecutionEvidence::new(
            &intent,
            ProcessExitStatus::Exited(0),
            128,
            false,
            256,
            true,
        )
        .expect("valid process execution evidence");

        assert_eq!(evidence.intent_summary(), intent.summary());
        assert_eq!(evidence.argv(), intent.argv());
        assert_eq!(evidence.cwd(), intent.cwd());
        assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
        assert_eq!(evidence.exit_code(), Some(0));
        assert_eq!(evidence.stdout_bytes(), 128);
        assert!(!evidence.stdout_truncated());
        assert_eq!(evidence.stderr_bytes(), 256);
        assert!(evidence.stderr_truncated());
        assert!(evidence.matches_intent(&intent));

        let too_many_bytes = ProcessExecutionEvidence::new(
            &intent,
            ProcessExitStatus::Exited(1),
            1025,
            true,
            0,
            false,
        )
        .expect_err("captured bytes must stay within intent limits");
        assert!(matches!(
            too_many_bytes,
            ProcessActionError::InvalidExecutionEvidence {
                field: "stdout_bytes",
                ..
            }
        ));
    }

    #[test]
    fn classifies_known_process_argv_shapes() {
        for argv in [
            vec!["rustc", "--version"],
            vec!["rg", "--version"],
            vec!["rg", "--files"],
            vec!["rg", "ProcessRunner"],
        ] {
            let intent = ProcessActionIntent::new(
                argv.into_iter().map(str::to_owned).collect(),
                None,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .expect("informational argv is a valid process intent");
            assert_eq!(
                classify_process_intent(&intent),
                ProcessIntentClass::Informational
            );
        }

        for argv in [
            vec!["cargo", "test", "-p", "merry-runtime"],
            vec!["cargo", "test", "--package", "merry-runtime"],
        ] {
            let intent = ProcessActionIntent::new(
                argv.into_iter().map(str::to_owned).collect(),
                None,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .expect("local workspace argv is a valid process intent");
            assert_eq!(
                classify_process_intent(&intent),
                ProcessIntentClass::LocalWorkspaceEffect
            );
        }

        for argv in [
            vec!["sh", "-c", "echo unsafe"],
            vec!["bash", "-lc", "echo unsafe"],
            vec!["zsh", "-c", "echo unsafe"],
            vec!["fish", "-c", "echo unsafe"],
            vec!["cmd", "/C", "echo unsafe"],
            vec!["powershell", "-Command", "Write-Host unsafe"],
            vec!["pwsh", "-Command", "Write-Host unsafe"],
            vec!["python", "-c", "print('unsafe')"],
            vec!["python3", "script.py"],
            vec!["perl", "-e", "print 'unsafe'"],
            vec!["ruby", "-e", "puts 'unsafe'"],
            vec!["node", "-e", "console.log('unsafe')"],
            vec!["curl", "https://example.invalid"],
            vec!["wget", "https://example.invalid"],
            vec!["ssh", "example.invalid"],
            vec!["scp", "a", "b"],
            vec!["rsync", "a", "b"],
            vec!["nc", "example.invalid", "443"],
            vec!["netcat", "example.invalid", "443"],
            vec!["rm", "-rf", "target"],
            vec!["../bin/rm", "-rf", "target"],
            vec!["mv", "a", "b"],
            vec!["cp", "a", "b"],
            vec!["chmod", "600", "file"],
            vec!["chown", "user", "file"],
            vec!["git", "status"],
            vec!["/tmp/sh", "-c", "echo unsafe"],
            vec!["./bash", "-lc", "echo unsafe"],
        ] {
            let intent = ProcessActionIntent::new(
                argv.into_iter().map(str::to_owned).collect(),
                None,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .expect("forbidden argv is still a syntactically valid process intent");
            assert_eq!(
                classify_process_intent(&intent),
                ProcessIntentClass::Forbidden
            );
        }

        for argv in [
            vec!["cargo", "test"],
            vec!["cargo", "test", "-p", "other-crate"],
            vec!["/tmp/cargo", "test", "-p", "merry-runtime"],
            vec!["./cargo", "test", "-p", "merry-runtime"],
            vec!["../bin/cargo", "test", "-p", "merry-runtime"],
            vec!["/tmp/rustc", "--version"],
            vec!["./rg", "--version"],
            vec!["/tmp/rg", "--files"],
            vec!["./rg", "ProcessRunner"],
            vec!["rg", "-n", "ProcessRunner"],
            vec!["rg", "--glob", "*.rs"],
            vec!["rg", "-"],
            vec!["rg", "-pattern"],
            vec!["rg", "Process.*"],
            vec!["rg", "Process|Runner"],
            vec!["rg", "call()"],
            vec!["unknown-readonly-ish", "--version"],
            vec!["python3.12", "-c", "print('unknown')"],
            vec!["docker", "run", "image"],
        ] {
            let intent = ProcessActionIntent::new(
                argv.into_iter().map(str::to_owned).collect(),
                None,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .expect("unknown argv is still a syntactically valid process intent");
            assert_eq!(
                classify_process_intent(&intent),
                ProcessIntentClass::Unknown
            );
        }
    }

    #[test]
    fn sp3a_low_risk_process_admission_allows_narrow_read_only_argv() {
        for argv in [
            vec!["rustc", "--version"],
            vec!["rg", "--version"],
            vec!["rg", "--files"],
            vec!["rg", "ProcessRunner"],
        ] {
            let intent = ProcessActionIntent::new(
                argv.into_iter().map(str::to_owned).collect(),
                None,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .expect("informational argv is a valid process intent");
            assert!(is_low_risk_process_action_intent(&intent));
        }

        for argv in [
            vec!["cargo", "test", "-p", "merry-runtime"],
            vec!["cargo", "test", "--package", "merry-runtime"],
            vec!["/tmp/rustc", "--version"],
            vec!["./rg", "--version"],
            vec!["rg", "-n", "ProcessRunner"],
            vec!["rg", "--glob", "*.rs"],
            vec!["rg", "-"],
            vec!["rg", "Process.*"],
            vec!["rg", "Process|Runner"],
            vec!["unknown-readonly-ish", "--version"],
            vec!["sh", "-c", "echo unsafe"],
        ] {
            let intent = ProcessActionIntent::new(
                argv.into_iter().map(str::to_owned).collect(),
                None,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .expect("non-informational argv is still a valid process intent");
            assert!(!is_low_risk_process_action_intent(&intent));
        }
    }

    #[test]
    fn sp3a_low_risk_process_admission_rejects_stdin_or_env() {
        let stdin_intent = ProcessActionIntent::new(
            vec!["rg".to_owned(), "--files".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            Some("payload must not enter the auto-admitted lane".to_owned()),
            1024,
            1024,
        )
        .expect("stdin process intent is syntactically valid");
        assert_eq!(
            classify_process_intent(&stdin_intent),
            ProcessIntentClass::Informational
        );
        assert!(!is_low_risk_process_action_intent(&stdin_intent));

        let env_intent = ProcessActionIntent::new(
            vec!["rg".to_owned(), "--version".to_owned()],
            None,
            ProcessEnvPolicy::NonEmptyForTest,
            None,
            1024,
            1024,
        )
        .expect("non-empty env process intent is syntactically valid");
        assert_eq!(
            classify_process_intent(&env_intent),
            ProcessIntentClass::Informational
        );
        assert!(!is_low_risk_process_action_intent(&env_intent));
    }
}
