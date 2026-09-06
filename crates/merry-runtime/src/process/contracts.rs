use super::validation::{
    ProcessActionError, summarize_intent, validate_argv, validate_captured_bytes, validate_cwd,
    validate_output_limit, validate_stdin_text,
};
use crate::{PermissionRequest, RequestedCapability};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, future::Future, pin::Pin, sync::Arc};
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

/// Explicit admission for the process runner used by the local workspace lane.
///
/// This value is intentionally small and declarative. It records that the
/// caller has selected Merry's local-workspace process profile and accepted
/// the process risk for that profile; it is not proof that any process is
/// actually confined. A host adapter may materialize this profile with a
/// platform-native mechanism such as bubblewrap, a different platform
/// mechanism, or an explicitly unavailable capability. Runtime code treats it
/// as construction-time admission for the configured process runner boundary;
/// argv classification remains a risk signal rather than an executable
/// allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptedLocalWorkspaceProcessAdmission {
    sandbox_profile: LocalWorkspaceProcessSandboxProfile,
    permission_profile_id: ProcessPermissionProfileId,
}

impl AcceptedLocalWorkspaceProcessAdmission {
    /// Creates admission for the local workspace process profile.
    ///
    /// Calling this explicitly accepts validated process intents for the
    /// declared profile. The selected runner still enforces filesystem,
    /// network, and host-integration capabilities.
    #[must_use]
    pub const fn accept_local_workspace() -> Self {
        Self {
            sandbox_profile: LocalWorkspaceProcessSandboxProfile::LocalWorkspace,
            permission_profile_id: ProcessPermissionProfileId::LOCAL_WORKSPACE,
        }
    }

    /// Creates admission for the explicit unrestricted host process profile.
    ///
    /// The runtime still records and audits the process action, but the host
    /// process backend does not add an operating-system sandbox in this mode.
    /// Review policy remains a separate runtime setting; host admission does
    /// not imply fully trusted execution.
    #[must_use]
    pub const fn accept_host() -> Self {
        Self {
            sandbox_profile: LocalWorkspaceProcessSandboxProfile::Host,
            permission_profile_id: ProcessPermissionProfileId::LOCAL_WORKSPACE_HOST,
        }
    }

    #[cfg(test)]
    pub(crate) const fn for_test_permission_profile_id(
        permission_profile_id: ProcessPermissionProfileId,
    ) -> Self {
        Self {
            sandbox_profile: LocalWorkspaceProcessSandboxProfile::LocalWorkspace,
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
}

/// Declared sandbox/profile associated with local workspace process admission.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalWorkspaceProcessSandboxProfile {
    /// Local workspace process profile.
    LocalWorkspace,
    /// Explicit unrestricted host process profile.
    Host,
}

impl LocalWorkspaceProcessSandboxProfile {
    /// Returns the stable profile label used in process composition identity.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalWorkspace => "local-workspace",
            Self::Host => "host",
        }
    }
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
    pub const READ_ONLY: Self = Self("process.read_only");
    /// Local workspace process lane.
    pub const LOCAL_WORKSPACE: Self = Self("process.local_workspace");
    /// Local workspace process lane accepted for the unrestricted host profile.
    pub const LOCAL_WORKSPACE_HOST: Self = Self("process.local_workspace.host");
    /// Read-only shell wrapper lane for plain command sequences under a real shell runner.
    pub const SHELL_READ_ONLY: Self = Self("process.shell.read_only");
    /// Process lane admitted by an explicit permission request review.
    pub const APPROVED_PERMISSION_REQUEST: Self = Self("process.permission_request.approved");

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
            "process.read_only" => Ok(Self::READ_ONLY),
            "process.local_workspace" => Ok(Self::LOCAL_WORKSPACE),
            "process.local_workspace.host" => Ok(Self::LOCAL_WORKSPACE_HOST),
            "process.shell.read_only" => Ok(Self::SHELL_READ_ONLY),
            "process.permission_request.approved" => Ok(Self::APPROVED_PERMISSION_REQUEST),
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

/// Factory for runners created from approved permission requests.
///
/// Implementations translate a runtime-approved request into the concrete
/// process backend/profile for that exact action. Backends may retain approved
/// paths and host integrations in a session-scoped store so later actions in
/// the same session can reuse them. Network access is action-scoped and must
/// not be retained; backends must not grant authority beyond the capabilities
/// approved by the runtime.
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

    /// Returns whether the backend already enforces every capability in the
    /// request for the current process session.
    ///
    /// This query covers capability state only; it does not authorize the
    /// requested action. Implementations must return `false` for capabilities
    /// that are action-scoped, such as network access, or when the backend
    /// cannot prove that the requested access is covered.
    fn request_capabilities_are_satisfied(
        &self,
        _request: &PermissionRequest,
    ) -> Result<bool, ProcessRunnerError> {
        Ok(false)
    }

    /// Creates the process runner for one approved permission request and
    /// records only the session-scoped capabilities supported by the backend.
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
        if request
            .requested()
            .iter()
            .any(|capability| matches!(capability, RequestedCapability::HostIntegration(_)))
        {
            return Err(ProcessRunnerError::infrastructure(
                "static permissioned process runner cannot enforce requested host integrations",
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
/// Process output is an opaque byte stream. The runner keeps the exact bounded
/// bytes and a loss-tolerant UTF-8 view separately so binary tools cannot turn a
/// successful process into an infrastructure error merely by writing invalid
/// UTF-8. Internal execution audit evidence stores byte counts and truncation
/// flags, not these payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRunnerOutput {
    status: ProcessExitStatus,
    stdout_data: Vec<u8>,
    stdout_text: String,
    stdout_bytes: usize,
    stdout_truncated: bool,
    stderr_data: Vec<u8>,
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
        Self::from_bytes(
            intent,
            status,
            stdout_text.into().into_bytes(),
            stdout_truncated,
            stderr_text.into().into_bytes(),
            stderr_truncated,
        )
    }

    /// Creates output from exact bounded process bytes.
    pub fn from_bytes(
        intent: &ProcessActionIntent,
        status: ProcessExitStatus,
        stdout_data: Vec<u8>,
        stdout_truncated: bool,
        stderr_data: Vec<u8>,
        stderr_truncated: bool,
    ) -> Result<Self, ProcessActionError> {
        validate_captured_bytes(
            "stdout_bytes",
            stdout_data.len(),
            intent.stdout_limit_bytes(),
        )?;
        validate_captured_bytes(
            "stderr_bytes",
            stderr_data.len(),
            intent.stderr_limit_bytes(),
        )?;

        Ok(Self {
            status,
            stdout_text: String::from_utf8_lossy(&stdout_data).into_owned(),
            stdout_bytes: stdout_data.len(),
            stdout_data,
            stdout_truncated,
            stderr_text: String::from_utf8_lossy(&stderr_data).into_owned(),
            stderr_bytes: stderr_data.len(),
            stderr_data,
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

    /// Returns the exact bounded stdout bytes.
    #[must_use]
    pub fn stdout_data(&self) -> &[u8] {
        &self.stdout_data
    }

    /// Returns whether stdout was valid UTF-8 without replacement.
    #[must_use]
    pub fn stdout_is_utf8(&self) -> bool {
        std::str::from_utf8(&self.stdout_data).is_ok()
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

    /// Returns the exact bounded stderr bytes.
    #[must_use]
    pub fn stderr_data(&self) -> &[u8] {
        &self.stderr_data
    }

    /// Returns whether stderr was valid UTF-8 without replacement.
    #[must_use]
    pub fn stderr_is_utf8(&self) -> bool {
        std::str::from_utf8(&self.stderr_data).is_ok()
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
