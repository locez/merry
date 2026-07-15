//! Provider-neutral process action protocol values.
//!
//! This module describes process intent and execution evidence only. It does
//! not execute commands or spawn subprocesses. It includes narrow risk
//! classifiers for process argv and shell-wrapper command text, but those
//! classifiers are not a shell interpreter.

use crate::{PermissionRequest, RequestedCapability};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    fmt,
    future::Future,
    path::{Component, Path},
    pin::Pin,
    sync::Arc,
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

/// Minimal process environment override policy for SP1.
///
/// This describes environment changes requested by the tool call. It does not
/// decide whether the selected process runner inherits its own current
/// environment; that is part of the runner/sandbox boundary chosen by the
/// runtime builder.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEnvPolicy {
    /// No tool-requested environment overrides.
    #[default]
    Empty,
    /// Test-only stand-in for a future non-empty environment policy.
    #[cfg(test)]
    NonEmptyForTest,
}

impl ProcessEnvPolicy {
    /// Creates the no environment override policy.
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
    permission_profile_id: ProcessPermissionProfileId,
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
            permission_profile_id: ProcessPermissionProfileId::LOCAL_WORKSPACE_BWRAP_V1,
        }
    }

    #[cfg(test)]
    pub(crate) const fn for_test_permission_profile_id(
        permission_profile_id: ProcessPermissionProfileId,
    ) -> Self {
        Self {
            sandbox_profile: LocalWorkspaceProcessSandboxProfile::CliBwrapV1,
            permission_profile_id,
        }
    }

    /// Returns the declared sandbox profile for this admission.
    #[must_use]
    pub const fn sandbox_profile(self) -> LocalWorkspaceProcessSandboxProfile {
        self.sandbox_profile
    }

    /// Returns the permission profile admitted by this construction-time grant.
    #[must_use]
    pub const fn permission_profile_id(self) -> ProcessPermissionProfileId {
        self.permission_profile_id
    }

    pub(crate) fn matches_intent(self, intent: &ProcessActionIntent) -> bool {
        required_process_permission_profile_id(intent) == Some(self.permission_profile_id)
    }
}

/// Declared sandbox/profile associated with local workspace process admission.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalWorkspaceProcessSandboxProfile {
    /// Merry CLI bubblewrap profile version 1.
    CliBwrapV1,
}

/// Stable identifier for a runtime-owned process permission profile.
///
/// Permission profiles describe filesystem, network, and side-effect
/// capability. They are separate from concrete command classification and from
/// model-visible tool profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessPermissionProfileId(&'static str);

impl ProcessPermissionProfileId {
    /// Read-only process lane for bounded inspection commands.
    pub const READ_ONLY_V1: Self = Self("process.read_only.v1");
    /// Local workspace process lane accepted for the CLI bubblewrap v1 sandbox.
    pub const LOCAL_WORKSPACE_BWRAP_V1: Self = Self("process.local_workspace.bwrap.v1");
    /// Read-only shell wrapper lane for plain command sequences under a real shell runner.
    pub const SHELL_READ_ONLY_V1: Self = Self("process.shell.read_only.v1");
    /// Process lane admitted by an explicit permission request review.
    pub const APPROVED_PERMISSION_REQUEST_V1: Self = Self("process.permission_request.approved.v1");

    /// Returns the stable profile identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl Serialize for ProcessPermissionProfileId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str((*self).as_str())
    }
}

impl<'de> Deserialize<'de> for ProcessPermissionProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "process.read_only.v1" => Ok(Self::READ_ONLY_V1),
            "process.local_workspace.bwrap.v1" => Ok(Self::LOCAL_WORKSPACE_BWRAP_V1),
            "process.shell.read_only.v1" => Ok(Self::SHELL_READ_ONLY_V1),
            "process.permission_request.approved.v1" => Ok(Self::APPROVED_PERMISSION_REQUEST_V1),
            _ => Err(serde::de::Error::custom(format!(
                "unsupported process permission profile id `{value}`"
            ))),
        }
    }
}

impl fmt::Display for ProcessPermissionProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
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

/// Factory for runners scoped to one approved permission request.
///
/// Implementations translate a runtime-approved request into the concrete
/// process backend/profile for that exact action. They must not grant reusable
/// authority back to the model.
pub trait PermissionedProcessRunnerFactory: Send + Sync {
    /// Validates the request against the backend's hard capability policy.
    ///
    /// Backends that do not have additional policy constraints may keep the
    /// default only when they can enforce every capability in the request
    /// through their existing runner. A backend that cannot materialize a
    /// requested path or network grant must reject it. A validation failure
    /// must happen before any reviewer or process side effect is started.
    fn validate_request(&self, _request: &PermissionRequest) -> Result<(), ProcessRunnerError> {
        Ok(())
    }

    /// Creates the process runner for one approved permission request.
    fn runner_for(&self, request: &PermissionRequest) -> Arc<dyn ProcessRunner>;
}

/// Compatibility factory that always returns the same runner.
#[derive(Clone)]
pub struct StaticPermissionedProcessRunnerFactory {
    runner: Arc<dyn ProcessRunner>,
}

impl StaticPermissionedProcessRunnerFactory {
    /// Creates a static runner factory.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

impl PermissionedProcessRunnerFactory for StaticPermissionedProcessRunnerFactory {
    fn validate_request(&self, request: &PermissionRequest) -> Result<(), ProcessRunnerError> {
        if request
            .requested()
            .iter()
            .any(|capability| matches!(capability, RequestedCapability::Path(_)))
        {
            return Err(ProcessRunnerError::infrastructure(
                "static permissioned process runner cannot enforce requested path capabilities",
            ));
        }
        Ok(())
    }

    fn runner_for(&self, _request: &PermissionRequest) -> Arc<dyn ProcessRunner> {
        Arc::clone(&self.runner)
    }
}

/// Provider-neutral, typed intent for a local process action.
///
/// The argv vector is intentionally open and does not enumerate allowed
/// commands. This value is proposal evidence only in SP1; it is not an
/// executor and must not be treated as authorization to spawn a process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        permission_profile_id: ProcessPermissionProfileId,
    ) -> Result<ProcessExecutionEvidence, ProcessActionError> {
        ProcessExecutionEvidence::new(
            intent,
            permission_profile_id,
            self.status,
            self.stdout_bytes,
            self.stdout_truncated,
            self.stderr_bytes,
            self.stderr_truncated,
        )
    }
}

/// Provider-neutral process completion status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessExecutionEvidence {
    intent_summary: String,
    argv: Vec<String>,
    cwd: Option<String>,
    permission_profile_id: ProcessPermissionProfileId,
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
        permission_profile_id: ProcessPermissionProfileId,
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
            permission_profile_id,
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

    /// Returns the permission profile used for this process execution.
    #[must_use]
    pub const fn permission_profile_id(&self) -> ProcessPermissionProfileId {
        self.permission_profile_id
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

/// Exact shell-wrapper input plus payload-free metadata helpers.
///
/// This value recognizes only the validated wrapper shape used by the current
/// shell read-only lane. It is not a shell parser and it does not authorize
/// execution by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShellProcessInput<'a> {
    shell: &'a str,
    flag: &'a str,
    script: &'a str,
}

impl<'a> ShellProcessInput<'a> {
    pub(crate) const fn shell(self) -> &'a str {
        self.shell
    }

    pub(crate) const fn flag(self) -> &'a str {
        self.flag
    }

    pub(crate) const fn script(self) -> &'a str {
        self.script
    }

    pub(crate) const fn script_bytes(self) -> usize {
        self.script.len()
    }

    pub(crate) fn script_fingerprint(self) -> String {
        stable_process_input_fingerprint(self.script.as_bytes())
    }
}

pub(crate) fn shell_process_input(intent: &ProcessActionIntent) -> Option<ShellProcessInput<'_>> {
    shell_process_input_from_argv(intent.argv())
}

/// Classifies a process intent using validated argv only.
#[must_use]
pub fn classify_process_intent(intent: &ProcessActionIntent) -> ProcessIntentClass {
    classify_process_argv(intent.argv())
}

fn classify_process_argv(argv: &[String]) -> ProcessIntentClass {
    if is_forbidden_process_argv(argv) {
        return ProcessIntentClass::Forbidden;
    }
    if is_informational_process_argv(argv) {
        return ProcessIntentClass::Informational;
    }
    if is_local_workspace_effect_process_argv(argv) {
        return ProcessIntentClass::LocalWorkspaceEffect;
    }
    ProcessIntentClass::Unknown
}

fn is_informational_process_argv(argv: &[String]) -> bool {
    if is_read_only_direct_process_argv(argv) {
        return true;
    }

    is_read_only_plain_shell_process_argv(argv)
}

fn is_read_only_direct_process_argv(argv: &[String]) -> bool {
    match argv {
        [executable, version]
            if executable_token_is(executable, "rustc") && version.as_str() == "--version" =>
        {
            true
        }
        [executable, rg_arg] if executable_token_is(executable, "rg") => {
            is_read_only_rg_single_argument(rg_arg)
        }
        [executable, print_flag, range, file]
            if executable_token_is(executable, "sed")
                && print_flag.as_str() == "-n"
                && is_read_only_sed_print_range(range)
                && is_workspace_relative_file_argument(file) =>
        {
            true
        }
        [executable, subcommand, args @ ..] if executable_token_is(executable, "git") => {
            is_read_only_git_command(subcommand, args)
        }
        [executable] if executable_token_is(executable, "pwd") => true,
        [executable] if executable_token_is(executable, "true") => true,
        [executable] if executable_token_is(executable, "false") => true,
        [executable, args @ ..] if executable_token_is(executable, "echo") => {
            is_read_only_echo_args(args)
        }
        [executable, args @ ..] if executable_token_is(executable, "wc") => {
            is_read_only_wc_args(args)
        }
        [executable, args @ ..] if executable_token_is(executable, "head") => {
            is_read_only_head_or_tail_args(args)
        }
        [executable, args @ ..] if executable_token_is(executable, "tail") => {
            is_read_only_head_or_tail_args(args)
        }
        [executable, args @ ..] if executable_token_is(executable, "cargo") => {
            is_read_only_cargo_fmt_check_args(args)
        }
        [executable] if executable_token_is(executable, "ls") => true,
        [executable, file] if executable_token_is(executable, "ls") => {
            is_workspace_relative_file_argument(file)
        }
        [executable, file] if executable_token_is(executable, "cat") => {
            is_workspace_relative_file_argument(file)
        }
        _ => false,
    }
}

fn is_read_only_cargo_fmt_check_args(args: &[String]) -> bool {
    let mut saw_fmt = false;
    let mut saw_check = false;
    for argument in args {
        match argument.as_str() {
            "fmt" if !saw_fmt => saw_fmt = true,
            "--all" | "--check" if !saw_check || argument == "--all" => {
                if argument == "--check" {
                    saw_check = true;
                }
            }
            _ => return false,
        }
    }
    saw_fmt && saw_check
}

fn is_read_only_plain_shell_process_argv(argv: &[String]) -> bool {
    let Some(shell_input) = shell_process_input_from_argv(argv) else {
        return false;
    };

    parse_plain_shell_command_sequence(shell_input.script()).is_some_and(|commands| {
        !commands.is_empty()
            && commands
                .iter()
                .all(|command| is_read_only_direct_process_argv(command))
    })
}

fn shell_process_input_from_argv(argv: &[String]) -> Option<ShellProcessInput<'_>> {
    let [shell, flag, script] = argv else {
        return None;
    };
    if !is_supported_plain_shell_token(shell) || !matches!(flag.as_str(), "-c" | "-lc") {
        return None;
    }

    Some(ShellProcessInput {
        shell,
        flag,
        script,
    })
}

fn is_supported_plain_shell_token(shell: &str) -> bool {
    matches!(shell, "bash" | "sh" | "zsh")
}

pub(crate) fn stable_process_input_fingerprint(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = bytes.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    format!("fnv1a64:{hash:016x}")
}

fn is_read_only_echo_args(args: &[String]) -> bool {
    !args.iter().any(|arg| arg.starts_with('-'))
}

fn is_read_only_wc_args(args: &[String]) -> bool {
    match args {
        [] => true,
        [flag_or_file] => {
            is_read_only_wc_flag(flag_or_file) || is_workspace_relative_file_argument(flag_or_file)
        }
        [flag, file] => is_read_only_wc_flag(flag) && is_workspace_relative_file_argument(file),
        _ => false,
    }
}

fn is_read_only_wc_flag(flag: &str) -> bool {
    matches!(flag, "-l" | "-w" | "-c" | "-m")
}

fn is_read_only_head_or_tail_args(args: &[String]) -> bool {
    match args {
        [] => true,
        [file] => is_workspace_relative_file_argument(file),
        [count_flag, count] if count_flag.as_str() == "-n" => is_positive_decimal(count),
        [count_flag, count, file] if count_flag.as_str() == "-n" => {
            is_positive_decimal(count) && is_workspace_relative_file_argument(file)
        }
        _ => false,
    }
}

fn parse_plain_shell_command_sequence(script: &str) -> Option<Vec<Vec<String>>> {
    let mut chars = script.chars().peekable();
    let mut commands = Vec::new();
    let mut current_command = Vec::new();
    let mut last_token_was_operator = false;

    loop {
        skip_shell_whitespace(&mut chars);
        let Some(next) = chars.peek().copied() else {
            break;
        };

        if is_shell_sequence_operator_start(next) {
            parse_shell_sequence_operator(&mut chars)?;
            if current_command.is_empty() {
                return None;
            }
            commands.push(std::mem::take(&mut current_command));
            last_token_was_operator = true;
            continue;
        }

        let word = parse_plain_shell_word(&mut chars)?;
        if word.is_empty() {
            return None;
        }
        current_command.push(word);
        last_token_was_operator = false;
    }

    if last_token_was_operator {
        return None;
    }
    if !current_command.is_empty() {
        commands.push(current_command);
    }
    if commands.is_empty() {
        return None;
    }

    Some(commands)
}

fn skip_shell_whitespace(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while chars
        .peek()
        .is_some_and(|character| character.is_whitespace())
    {
        chars.next();
    }
}

fn parse_shell_sequence_operator(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<()> {
    match chars.next()? {
        ';' | '|' if chars.peek() != Some(&'|') => Some(()),
        '|' if chars.peek() == Some(&'|') => {
            chars.next();
            Some(())
        }
        '&' if chars.peek() == Some(&'&') => {
            chars.next();
            Some(())
        }
        _ => None,
    }
}

fn parse_plain_shell_word(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let mut word = String::new();
    while let Some(next) = chars.peek().copied() {
        if next.is_whitespace() || is_shell_sequence_operator_start(next) {
            break;
        }

        match next {
            '\'' => {
                chars.next();
                parse_plain_single_quoted_shell_fragment(chars, &mut word)?;
            }
            '"' => {
                chars.next();
                parse_plain_double_quoted_shell_fragment(chars, &mut word)?;
            }
            character if shell_word_character_is_disallowed(character) => return None,
            character => {
                chars.next();
                word.push(character);
            }
        }
    }

    Some(word)
}

fn parse_plain_single_quoted_shell_fragment(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    word: &mut String,
) -> Option<()> {
    for character in chars.by_ref() {
        if character == '\'' {
            return Some(());
        }
        word.push(character);
    }
    None
}

fn parse_plain_double_quoted_shell_fragment(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    word: &mut String,
) -> Option<()> {
    for character in chars.by_ref() {
        match character {
            '"' => return Some(()),
            '$' | '`' | '\\' | '!' => return None,
            _ => word.push(character),
        }
    }
    None
}

fn is_shell_sequence_operator_start(character: char) -> bool {
    matches!(character, ';' | '|' | '&')
}

fn shell_word_character_is_disallowed(character: char) -> bool {
    matches!(
        character,
        '$' | '`'
            | '\\'
            | '<'
            | '>'
            | '('
            | ')'
            | '{'
            | '}'
            | '['
            | ']'
            | '*'
            | '?'
            | '~'
            | '#'
            | '!'
    )
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

fn is_read_only_sed_print_range(range: &str) -> bool {
    let Some(line_range) = range.strip_suffix('p') else {
        return false;
    };
    if line_range.is_empty() {
        return false;
    }
    let mut parts = line_range.split(',');
    let Some(start) = parts.next() else {
        return false;
    };
    if !is_positive_decimal(start) {
        return false;
    }
    match (parts.next(), parts.next()) {
        (None, None) => true,
        (Some(end), None) => is_positive_decimal(end),
        _ => false,
    }
}

fn is_positive_decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) && value != "0"
}

fn is_workspace_relative_file_argument(argument: &str) -> bool {
    !argument.starts_with('-')
        && !Path::new(argument).is_absolute()
        && !argument.split('/').any(|segment| {
            segment.is_empty() || segment == "." || segment == ".." || segment.contains('\\')
        })
}

fn is_read_only_git_command(subcommand: &str, args: &[String]) -> bool {
    match subcommand {
        "status" => {
            let mut short = false;
            let mut branch = false;
            for argument in args {
                match argument.as_str() {
                    "--short" if !short => short = true,
                    "--branch" if !branch => branch = true,
                    _ => return false,
                }
            }
            true
        }
        "branch" => matches!(args, [arg] if arg.as_str() == "--show-current"),
        "log" => args
            .iter()
            .all(|arg| arg.as_str() == "--oneline" || is_git_count_arg(arg)),
        "diff" => is_read_only_git_diff_args(args),
        "show" => is_read_only_git_show_args(args),
        _ => false,
    }
}

fn is_git_count_arg(argument: &str) -> bool {
    let Some(count) = argument.strip_prefix('-') else {
        return false;
    };
    is_positive_decimal(count)
}

fn is_read_only_git_diff_args(args: &[String]) -> bool {
    match args {
        [] => true,
        [separator, path] if separator.as_str() == "--" => {
            is_workspace_relative_file_argument(path)
        }
        _ => false,
    }
}

fn is_read_only_git_show_args(args: &[String]) -> bool {
    match args {
        [arg] => !arg.starts_with('-') && !arg.contains(':'),
        [stat, rev] if stat.as_str() == "--stat" => !rev.starts_with('-') && !rev.contains(':'),
        _ => false,
    }
}

fn is_local_workspace_effect_process_argv(argv: &[String]) -> bool {
    matches!(
        argv,
        [cargo, command, package_flag, package]
            if executable_token_is(cargo, "cargo")
                && matches!(command.as_str(), "test" | "check")
                && (package_flag.as_str() == "-p" || package_flag.as_str() == "--package")
                && is_safe_cargo_package_token(package)
    )
}

fn is_safe_cargo_package_token(package: &str) -> bool {
    !package.is_empty()
        && !package.starts_with('-')
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_forbidden_process_argv(argv: &[String]) -> bool {
    if let Some(shell_input) = shell_like_process_input_from_argv(argv) {
        return shell_script_contains_forbidden_process(shell_input.script());
    }

    is_forbidden_direct_process_argv(argv)
}

fn is_forbidden_direct_process_argv(argv: &[String]) -> bool {
    let Some(executable) = argv.first().map(|argument| executable_name(argument)) else {
        return false;
    };

    if FORBIDDEN_PROCESS_EXECUTABLES.contains(&executable.as_str()) {
        return true;
    }

    executable == "git"
        && argv
            .get(1)
            .is_some_and(|subcommand| FORBIDDEN_GIT_SUBCOMMANDS.contains(&subcommand.as_str()))
}

fn shell_script_contains_forbidden_process(script: &str) -> bool {
    if let Some(commands) = parse_plain_shell_command_sequence(script) {
        return commands.iter().any(|command| {
            let command = shell_command_without_assignment_prefix(command);
            !command.is_empty() && is_forbidden_direct_process_argv(command)
        });
    }

    shell_script_contains_obvious_forbidden_text(script)
}

fn shell_like_process_input_from_argv(argv: &[String]) -> Option<ShellProcessInput<'_>> {
    let [shell, flag, script] = argv else {
        return None;
    };
    if !is_supported_shell_executable_name(shell) || !matches!(flag.as_str(), "-c" | "-lc") {
        return None;
    }

    Some(ShellProcessInput {
        shell,
        flag,
        script,
    })
}

fn is_supported_shell_executable_name(shell: &str) -> bool {
    matches!(executable_name(shell).as_str(), "bash" | "sh" | "zsh")
}

fn shell_command_without_assignment_prefix(command: &[String]) -> &[String] {
    let executable_index = command
        .iter()
        .position(|word| !is_plain_shell_assignment_word(word))
        .unwrap_or(command.len());
    &command[executable_index..]
}

fn is_plain_shell_assignment_word(word: &str) -> bool {
    let Some((name, value)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !value.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
}

fn shell_script_contains_obvious_forbidden_text(script: &str) -> bool {
    let words = rough_shell_words(script);
    words.iter().enumerate().any(|(index, word)| {
        let executable = executable_name(word);
        FORBIDDEN_PROCESS_EXECUTABLES.contains(&executable.as_str())
            || (executable == "git"
                && words.get(index + 1).is_some_and(|subcommand| {
                    FORBIDDEN_GIT_SUBCOMMANDS.contains(&subcommand.as_str())
                }))
    })
}

fn rough_shell_words(script: &str) -> Vec<String> {
    script
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    ';' | '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}' | '[' | ']' | '\'' | '"'
                )
        })
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
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
    "cmd",
    "curl",
    "nc",
    "netcat",
    "powershell",
    "pwsh",
    "rm",
    "rsync",
    "scp",
    "ssh",
    "su",
    "sudo",
    "wget",
];

const FORBIDDEN_GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "apply",
    "checkout",
    "cherry-pick",
    "clean",
    "commit",
    "merge",
    "mv",
    "pull",
    "push",
    "rebase",
    "reset",
    "restore",
    "rm",
    "stash",
    "switch",
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
    required_process_permission_profile_id(intent) == Some(ProcessPermissionProfileId::READ_ONLY_V1)
}

/// Returns whether a process intent is a plain read-only shell-wrapper action.
///
/// This predicate is intentionally separate from the structured read-only
/// process lane. It recognizes only `bash`/`sh`/`zsh -c|-lc` scripts composed
/// of plain word commands joined by `|`, `&&`, `||`, or `;`, and it requires
/// each segment to match the direct read-only process classifier. It is not a
/// general shell parser and must be paired with an explicit shell runner
/// admission before execution.
#[must_use]
pub fn is_read_only_shell_process_action_intent(intent: &ProcessActionIntent) -> bool {
    required_process_permission_profile_id(intent)
        == Some(ProcessPermissionProfileId::SHELL_READ_ONLY_V1)
}

pub(crate) fn required_process_permission_profile_id(
    intent: &ProcessActionIntent,
) -> Option<ProcessPermissionProfileId> {
    if intent.env_policy() != ProcessEnvPolicy::Empty || intent.stdin_text().is_some() {
        return None;
    }

    if is_read_only_plain_shell_process_argv(intent.argv()) {
        return Some(ProcessPermissionProfileId::SHELL_READ_ONLY_V1);
    }

    match classify_process_intent(intent) {
        ProcessIntentClass::Informational => Some(ProcessPermissionProfileId::READ_ONLY_V1),
        ProcessIntentClass::LocalWorkspaceEffect => {
            Some(ProcessPermissionProfileId::LOCAL_WORKSPACE_BWRAP_V1)
        }
        ProcessIntentClass::Unknown => Some(ProcessPermissionProfileId::LOCAL_WORKSPACE_BWRAP_V1),
        ProcessIntentClass::Forbidden => None,
    }
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
        if argument.chars().any(disallowed_argv_control_character) {
            return Err(ProcessActionError::InvalidArgument {
                index,
                reason: "must not contain control characters other than newline or tab",
            });
        }
    }

    Ok(())
}

fn disallowed_argv_control_character(character: char) -> bool {
    character.is_control() && !matches!(character, '\n' | '\t')
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
        AcceptedLocalWorkspaceProcessAdmission, MAX_PROCESS_OUTPUT_LIMIT_BYTES, ProcessActionError,
        ProcessActionIntent, ProcessEnvPolicy, ProcessExecutionEvidence, ProcessExitStatus,
        ProcessIntentClass, ProcessPermissionProfileId, classify_process_intent,
        is_low_risk_process_action_intent, is_safe_cargo_package_token,
        required_process_permission_profile_id,
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

        let multiline_shell = ProcessActionIntent::new(
            vec![
                "bash".to_owned(),
                "-lc".to_owned(),
                "cargo check -p merry-runtime\ncargo test -p merry-runtime".to_owned(),
            ],
            Some(".".to_owned()),
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("shell argv may contain newline scripts");
        assert_eq!(
            multiline_shell.argv()[2],
            "cargo check -p merry-runtime\ncargo test -p merry-runtime"
        );

        for bad_arg in ["bad\u{0}arg", "bad\rarg", "bad\u{7f}arg"] {
            let error = ProcessActionIntent::new(
                vec!["bash".to_owned(), "-lc".to_owned(), bad_arg.to_owned()],
                None,
                ProcessEnvPolicy::empty(),
                None,
                1024,
                1024,
            )
            .expect_err("unsafe argv controls are rejected");
            assert!(matches!(
                error,
                ProcessActionError::InvalidArgument { index: 2, .. }
            ));
        }

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
            ProcessPermissionProfileId::READ_ONLY_V1,
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
        assert_eq!(
            evidence.permission_profile_id(),
            ProcessPermissionProfileId::READ_ONLY_V1
        );
        assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
        assert_eq!(evidence.exit_code(), Some(0));
        assert_eq!(evidence.stdout_bytes(), 128);
        assert!(!evidence.stdout_truncated());
        assert_eq!(evidence.stderr_bytes(), 256);
        assert!(evidence.stderr_truncated());
        assert!(evidence.matches_intent(&intent));

        let too_many_bytes = ProcessExecutionEvidence::new(
            &intent,
            ProcessPermissionProfileId::READ_ONLY_V1,
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
    fn process_permission_profile_id_is_derived_from_admitted_intent_shape() {
        let informational = ProcessActionIntent::new(
            vec!["rg".to_owned(), "--files".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("informational intent is valid");
        assert_eq!(
            required_process_permission_profile_id(&informational),
            Some(ProcessPermissionProfileId::READ_ONLY_V1)
        );

        let local_workspace_effect = ProcessActionIntent::new(
            vec![
                "cargo".to_owned(),
                "test".to_owned(),
                "-p".to_owned(),
                "merry-runtime".to_owned(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("local workspace effect intent is valid");
        assert_eq!(
            required_process_permission_profile_id(&local_workspace_effect),
            Some(ProcessPermissionProfileId::LOCAL_WORKSPACE_BWRAP_V1)
        );

        let unknown = ProcessActionIntent::new(
            vec!["unknown-readonly-ish".to_owned(), "--version".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("unknown intent is syntactically valid");
        assert_eq!(
            required_process_permission_profile_id(&unknown),
            Some(ProcessPermissionProfileId::LOCAL_WORKSPACE_BWRAP_V1)
        );

        let with_stdin = ProcessActionIntent::new(
            vec!["rg".to_owned(), "--files".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            Some("stdin is outside the read-only profile".to_owned()),
            1024,
            1024,
        )
        .expect("stdin intent is syntactically valid");
        assert_eq!(required_process_permission_profile_id(&with_stdin), None);

        let shell_read_only = ProcessActionIntent::new(
            vec![
                "bash".to_owned(),
                "-lc".to_owned(),
                "rg ProcessRunner | wc -l".to_owned(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("read-only shell intent is valid");
        assert_eq!(
            required_process_permission_profile_id(&shell_read_only),
            Some(ProcessPermissionProfileId::SHELL_READ_ONLY_V1)
        );

        let shell_workspace_effect = ProcessActionIntent::new(
            vec![
                "bash".to_owned(),
                "-lc".to_owned(),
                "HOME=.merry/local/home cargo check --all-targets -p merry-runtime".to_owned(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("shell workspace effect intent is valid");
        assert_eq!(
            required_process_permission_profile_id(&shell_workspace_effect),
            Some(ProcessPermissionProfileId::LOCAL_WORKSPACE_BWRAP_V1)
        );
    }

    #[test]
    fn local_workspace_process_admission_matches_only_its_permission_profile() {
        let local_workspace_effect = ProcessActionIntent::new(
            vec![
                "cargo".to_owned(),
                "test".to_owned(),
                "-p".to_owned(),
                "merry-runtime".to_owned(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("local workspace effect intent is valid");
        let informational = ProcessActionIntent::new(
            vec!["rg".to_owned(), "--files".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("informational intent is valid");

        let admission = AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1();
        assert_eq!(
            admission.permission_profile_id(),
            ProcessPermissionProfileId::LOCAL_WORKSPACE_BWRAP_V1
        );
        assert!(admission.matches_intent(&local_workspace_effect));
        assert!(!admission.matches_intent(&informational));

        let mismatched_admission =
            AcceptedLocalWorkspaceProcessAdmission::for_test_permission_profile_id(
                ProcessPermissionProfileId::READ_ONLY_V1,
            );
        assert!(!mismatched_admission.matches_intent(&local_workspace_effect));
    }

    #[test]
    fn safe_cargo_package_token_allows_package_names_without_paths_or_flags() {
        for package in [
            "merry-runtime",
            "other-crate",
            "merry_coding_loop_task_status_text",
            "crate123",
        ] {
            assert!(is_safe_cargo_package_token(package));
        }

        for package in [
            "",
            "-package",
            "bad.package",
            "../other-crate",
            "crate/name",
        ] {
            assert!(!is_safe_cargo_package_token(package));
        }
    }

    #[test]
    fn classifies_known_process_argv_shapes() {
        for argv in [
            vec!["rustc", "--version"],
            vec!["rg", "--version"],
            vec!["rg", "--files"],
            vec!["rg", "ProcessRunner"],
            vec!["cargo", "fmt", "--all", "--check"],
            vec!["sed", "-n", "1,80p", "crates/merry-runtime/src/process.rs"],
            vec!["git", "status", "--short"],
            vec!["git", "status", "--short", "--branch"],
            vec!["git", "status", "--branch", "--short"],
            vec!["git", "log", "--oneline", "-5"],
            vec!["git", "diff", "--", "crates/merry-runtime/src/process.rs"],
            vec!["git", "show", "--stat", "HEAD"],
            vec!["git", "branch", "--show-current"],
            vec!["bash", "-lc", "rg ProcessRunner | wc -l"],
            vec![
                "sh",
                "-c",
                "sed -n '1,5p' crates/merry-runtime/src/process.rs | wc -l",
            ],
            vec!["zsh", "-lc", "rg ProcessRunner && pwd"],
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
            vec!["cargo", "check", "-p", "merry-runtime"],
            vec!["cargo", "check", "--package", "merry-runtime"],
            vec!["cargo", "test", "-p", "other-crate"],
            vec!["cargo", "check", "-p", "merry_coding_loop_task_status_text"],
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
            vec!["sh", "-c", "rm -rf target"],
            vec!["bash", "-lc", "rm -rf target"],
            vec!["zsh", "-c", "rm -rf target"],
            vec!["cmd", "/C", "echo unsafe"],
            vec!["powershell", "-Command", "Write-Host unsafe"],
            vec!["pwsh", "-Command", "Write-Host unsafe"],
            vec!["curl", "https://example.invalid"],
            vec!["wget", "https://example.invalid"],
            vec!["ssh", "example.invalid"],
            vec!["scp", "a", "b"],
            vec!["rsync", "a", "b"],
            vec!["nc", "example.invalid", "443"],
            vec!["netcat", "example.invalid", "443"],
            vec!["rm", "-rf", "target"],
            vec!["../bin/rm", "-rf", "target"],
            vec!["git", "clean", "-fd"],
            vec!["git", "reset", "--hard"],
            vec!["git", "checkout", "--", "README.md"],
            vec!["bash", "-lc", "rg ProcessRunner | rm -rf target"],
            vec!["bash", "-lc", "echo $(rm -rf target)"],
            vec!["/bin/bash", "-lc", "rm -rf target"],
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
            vec!["fish", "-c", "echo unsafe"],
            vec!["python", "-c", "print('workspace effect')"],
            vec!["python3", "script.py"],
            vec!["perl", "-e", "print 'workspace effect'"],
            vec!["ruby", "-e", "puts 'workspace effect'"],
            vec!["node", "-e", "console.log('workspace effect')"],
            vec!["cargo", "test"],
            vec!["cargo", "test", "-p", "-package"],
            vec!["cargo", "test", "-p", "bad.package"],
            vec!["cargo", "test", "-p", "../other-crate"],
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
            vec!["sed", "-e", "1,80p", "crates/merry-runtime/src/process.rs"],
            vec!["sed", "-n", "1,80d", "crates/merry-runtime/src/process.rs"],
            vec!["sed", "-n", "1,80p"],
            vec!["sed", "-n", "1,80p", "../outside.rs"],
            vec!["sed", "-n", "1,80p", "/tmp/outside.rs"],
            vec!["git", "status", "--porcelain=v2"],
            vec!["git", "diff", "--cached"],
            vec!["git", "show", "HEAD:README.md"],
            vec!["unknown-readonly-ish", "--version"],
            vec!["python3.12", "-c", "print('unknown')"],
            vec!["docker", "run", "image"],
            vec!["/tmp/sh", "-c", "echo unsafe"],
            vec!["./bash", "-lc", "echo unsafe"],
            vec!["bash", "-lc", "rg ProcessRunner > out.txt"],
            vec!["bash", "-lc", "echo $(pwd)"],
            vec!["bash", "-lc", "(pwd)"],
            vec!["bash", "-lc", "rg ProcessRunner | tee out.txt"],
            vec!["/bin/bash", "-lc", "rg ProcessRunner | wc -l"],
            vec![
                "bash",
                "-lc",
                "HOME=.merry/local/home cargo check --all-targets -p merry-runtime",
            ],
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
            vec!["sed", "-n", "1,80p", "crates/merry-runtime/src/process.rs"],
            vec!["git", "status", "--short"],
            vec!["git", "log", "--oneline", "-5"],
            vec!["git", "diff", "--", "crates/merry-runtime/src/process.rs"],
            vec!["git", "show", "--stat", "HEAD"],
            vec!["git", "branch", "--show-current"],
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
            vec!["sed", "-n", "1,80d", "crates/merry-runtime/src/process.rs"],
            vec!["git", "clean", "-fd"],
            vec!["unknown-readonly-ish", "--version"],
            vec!["sh", "-c", "rm -rf target"],
            vec!["bash", "-lc", "rg ProcessRunner | wc -l"],
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
