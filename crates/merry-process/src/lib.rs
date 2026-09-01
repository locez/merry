//! Host-process adapters for Merry's provider-neutral process contract.
//!
//! [`merry_runtime`] owns process intent, admission, permission requests, and
//! execution evidence. This crate owns the host-facing runner selection and
//! binds the runners used by one runtime owner to one [`ProcessSession`]. A
//! platform can replace the concrete backend without changing coding
//! composition or runtime call sites.

mod bwrap_path;
mod process_runner;

pub use bwrap_path::resolve_bwrap_path;

pub use process_runner::TokioProcessRunner;

#[cfg(target_os = "linux")]
pub use process_runner::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessEnvironment, BwrapProcessRunner,
    BwrapSessionPermissions,
};

use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, HostIntegration, PathAccessRule, PermissionRequest,
    PermissionedProcessRunnerFactory, ProcessRunner, ProcessRunnerError,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;

/// Semantic process backend behavior requested by an application surface.
///
/// The selected behavior is intentionally independent of the operating system.
/// A platform backend may implement [`Isolated`](Self::Isolated) with its
/// native mechanism or report that the behavior is unavailable; callers do not
/// need to name bubblewrap or another concrete sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessBackendMode {
    /// Run actions in the host process environment, subject to runtime policy.
    Host,
    /// Request the platform's isolated local-process behavior.
    Isolated,
}

/// Errors raised while selecting or constructing a host-process backend.
///
/// Execution failures remain [`merry_runtime::ProcessRunnerError`]. This error
/// belongs to the adapter layer because it also represents a platform backend
/// that is not available for the requested semantic mode.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProcessBackendError {
    /// The concrete runner rejected its configuration.
    #[error("process backend configuration failed: {source}")]
    Runner {
        /// The runner configuration failure.
        #[source]
        source: ProcessRunnerError,
    },
    /// The requested isolated behavior has no implementation on this target.
    #[error(
        "isolated local process backend is unavailable on target operating system `{target_os}`"
    )]
    UnsupportedIsolation {
        /// Rust's target-operating-system identifier.
        target_os: &'static str,
    },
}

impl From<ProcessRunnerError> for ProcessBackendError {
    fn from(source: ProcessRunnerError) -> Self {
        Self::Runner { source }
    }
}

/// One process execution boundary used by a runtime instance.
///
/// The admission and both runner capabilities are kept together so callers
/// cannot accidentally pair a runner with a permission factory from another
/// backend configuration. Child runtimes obtain their process session through
/// [`ProcessBackend::new_session`], whose backend controls session isolation.
#[derive(Clone)]
pub struct ProcessSession {
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
}

impl ProcessSession {
    /// Creates a session from an explicit, trusted adapter boundary.
    ///
    /// Production callers should select [`LocalProcessBackend::new`] so the
    /// admission, runner, and reviewed-action factory are produced by one
    /// backend implementation. This constructor exists for adapters and
    /// deterministic fixtures that already own all three components.
    #[must_use]
    pub fn from_parts(
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        Self {
            admission,
            runner,
            permissioned_factory,
        }
    }

    /// Returns the runtime admission selected for this process boundary.
    #[must_use]
    pub const fn admission(&self) -> AcceptedLocalWorkspaceProcessAdmission {
        self.admission
    }

    /// Returns the runner for ordinary process actions.
    #[must_use]
    pub fn runner(&self) -> Arc<dyn ProcessRunner> {
        Arc::clone(&self.runner)
    }

    /// Returns the factory for reviewed process actions.
    #[must_use]
    pub fn permissioned_factory(&self) -> Arc<dyn PermissionedProcessRunnerFactory> {
        Arc::clone(&self.permissioned_factory)
    }
}

/// Stable host-process backend boundary consumed by coding composition.
///
/// The trait deliberately says nothing about how a platform enforces the
/// selected behavior. Linux may use bubblewrap, another Unix platform may use
/// its native process sandbox, and Windows may choose a host or isolated
/// process implementation without changing this contract.
pub trait ProcessBackend: Send + Sync {
    /// Creates the process session for one runtime owner.
    ///
    /// Each call returns a session with the backend's configured admission and
    /// runner capabilities. Backends may use a fresh session-scoped capability
    /// store for every call, which keeps parent and child runtime state isolated.
    fn new_session(&self) -> ProcessSession;
}

/// Inputs shared by local process backend implementations.
#[derive(Debug, Clone, Default)]
pub struct ProcessBackendOptions {
    /// Trusted filesystem rules installed for isolated actions.
    path_rules: Vec<PathAccessRule>,
    /// Named host integrations available to sandboxed actions.
    host_integrations: Vec<HostIntegration>,
    /// Environment assignments validated and applied by the host backend.
    environment_overrides: Vec<(OsString, OsString)>,
}

impl ProcessBackendOptions {
    /// Creates empty host-process backend options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets trusted filesystem rules installed for isolated actions.
    #[must_use]
    pub fn with_path_rules(mut self, path_rules: impl IntoIterator<Item = PathAccessRule>) -> Self {
        self.path_rules = path_rules.into_iter().collect();
        self
    }

    /// Sets named host integrations available to sandboxed actions.
    #[must_use]
    pub fn with_host_integrations(
        mut self,
        host_integrations: impl IntoIterator<Item = HostIntegration>,
    ) -> Self {
        self.host_integrations = host_integrations.into_iter().collect();
        self
    }

    /// Sets environment assignments that the backend validates before applying.
    #[must_use]
    pub fn with_environment_overrides(
        mut self,
        environment_overrides: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Self {
        self.environment_overrides = environment_overrides.into_iter().collect();
        self
    }

    /// Returns the trusted filesystem rules installed for isolated actions.
    #[must_use]
    pub fn path_rules(&self) -> &[PathAccessRule] {
        &self.path_rules
    }

    /// Returns the named host integrations available to sandboxed actions.
    #[must_use]
    pub fn host_integrations(&self) -> &[HostIntegration] {
        &self.host_integrations
    }

    /// Returns the validated environment assignments applied by the backend.
    #[must_use]
    pub fn environment_overrides(&self) -> &[(OsString, OsString)] {
        &self.environment_overrides
    }
}

/// Current local host-process backend.
///
/// The backend owns the current host implementation choice. Its public
/// session contract is intentionally independent of that choice, so adding a
/// platform-specific backend does not change coding profile or runtime APIs.
#[derive(Clone)]
pub struct LocalProcessBackend {
    new_session: Arc<dyn Fn() -> ProcessSession + Send + Sync>,
}

impl LocalProcessBackend {
    /// Creates a fixed backend from one already constructed session.
    ///
    /// This is intended for trusted adapters and deterministic test fixtures.
    /// Platform-selected production backends should use [`Self::new`] so the
    /// admission is created together with the concrete enforcement strategy.
    #[must_use]
    pub fn from_session(session: ProcessSession) -> Self {
        let child_session = session.clone();
        Self {
            new_session: Arc::new(move || child_session.clone()),
        }
    }

    fn from_parts(
        admission: AcceptedLocalWorkspaceProcessAdmission,
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        Self::from_session(ProcessSession::from_parts(
            admission,
            runner,
            permissioned_factory,
        ))
    }

    /// Creates a platform-selected local process backend.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessBackendError`] when the selected backend cannot be
    /// configured or the requested isolated behavior is unavailable.
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        mode: ProcessBackendMode,
        options: ProcessBackendOptions,
    ) -> Result<Self, ProcessBackendError> {
        match mode {
            ProcessBackendMode::Host => Self::unrestricted(workspace_root, options),
            ProcessBackendMode::Isolated => Self::sandboxed(workspace_root, options),
        }
    }

    /// Creates the current host-process implementation without an inner
    /// operating-system sandbox.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessBackendError::Runner`] when an environment override
    /// cannot be validated by the host runner.
    pub fn unrestricted(
        workspace_root: impl Into<PathBuf>,
        options: ProcessBackendOptions,
    ) -> Result<Self, ProcessBackendError> {
        let runner = Arc::new(
            TokioProcessRunner::new_at_workspace_root(workspace_root.into())
                .with_environment_overrides(options.environment_overrides)?,
        );
        let permissioned_factory = Arc::new(UnrestrictedPermissionedProcessRunnerFactory::new(
            Arc::clone(&runner) as Arc<dyn ProcessRunner>,
        ));
        Ok(Self::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_host(),
            runner,
            permissioned_factory,
        ))
    }

    /// Creates the current isolated local-process implementation.
    ///
    /// On Linux, the concrete implementation currently uses bubblewrap. On
    /// other targets it returns [`ProcessBackendError::UnsupportedIsolation`]
    /// until that platform supplies its own backend.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessBackendError`] when the workspace or sandbox
    /// environment is invalid, or when this target has no isolated backend.
    #[cfg(target_os = "linux")]
    pub fn sandboxed(
        workspace_root: impl Into<PathBuf>,
        options: ProcessBackendOptions,
    ) -> Result<Self, ProcessBackendError> {
        let workspace_root = workspace_root.into();
        let environment = BwrapProcessEnvironment::from_current_process()
            .with_host_integrations(options.host_integrations)
            .with_overrides(options.environment_overrides)?
            .validate_for_workspace(&workspace_root)?;
        Ok(Self::from_sandboxed_parts(
            workspace_root,
            environment,
            options.path_rules,
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub fn sandboxed(
        workspace_root: impl Into<PathBuf>,
        options: ProcessBackendOptions,
    ) -> Result<Self, ProcessBackendError> {
        let _ = (workspace_root.into(), options);
        Err(ProcessBackendError::UnsupportedIsolation {
            target_os: std::env::consts::OS,
        })
    }

    #[cfg(target_os = "linux")]
    fn from_sandboxed_parts(
        workspace_root: PathBuf,
        environment: BwrapProcessEnvironment,
        path_rules: Vec<PathAccessRule>,
    ) -> Self {
        let child_workspace_root = workspace_root.clone();
        let child_environment = environment;
        let child_path_rules = path_rules;
        Self {
            new_session: Arc::new(move || {
                Self::sandboxed_session(
                    &child_workspace_root,
                    &child_environment,
                    &child_path_rules,
                )
            }),
        }
    }

    #[cfg(target_os = "linux")]
    fn sandboxed_session(
        workspace_root: &Path,
        environment: &BwrapProcessEnvironment,
        path_rules: &[PathAccessRule],
    ) -> ProcessSession {
        let session_permissions = BwrapSessionPermissions::new();
        let runner = BwrapProcessRunner::new_at_workspace_root(workspace_root)
            .with_environment(environment.clone())
            .with_path_rules(path_rules.iter().cloned())
            .with_session_permissions(session_permissions.clone());
        let permissioned_factory =
            BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(workspace_root)
                .with_environment(environment.clone())
                .with_path_rules(path_rules.iter().cloned())
                .with_session_permissions(session_permissions);
        ProcessSession::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            Arc::new(runner),
            Arc::new(permissioned_factory),
        )
    }
}

impl ProcessBackend for LocalProcessBackend {
    fn new_session(&self) -> ProcessSession {
        (self.new_session)()
    }
}

/// Permissioned runner factory for the explicit host-process backend.
///
/// The host backend already exposes the process to the operating system, so a
/// reviewed capability request reuses its runner. Runtime admission and action
/// evidence remain active around the returned runner.
#[derive(Clone)]
pub struct UnrestrictedPermissionedProcessRunnerFactory {
    runner: Arc<dyn ProcessRunner>,
}

impl UnrestrictedPermissionedProcessRunnerFactory {
    /// Creates a factory that reuses the host-process runner.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self { runner }
    }
}

impl PermissionedProcessRunnerFactory for UnrestrictedPermissionedProcessRunnerFactory {
    fn runner_for(&self, _request: &PermissionRequest) -> Arc<dyn ProcessRunner> {
        Arc::clone(&self.runner)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    use super::ProcessBackendError;
    use super::{
        LocalProcessBackend, ProcessBackend, ProcessBackendMode, ProcessBackendOptions,
        TokioProcessRunner,
    };
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, PermissionedProcessRunnerFactory, ProcessRunner,
        StaticPermissionedProcessRunnerFactory,
    };
    use std::sync::Arc;

    #[test]
    fn session_keeps_admission_with_runner_capabilities() {
        let runner: Arc<dyn ProcessRunner> = Arc::new(TokioProcessRunner::new());
        let factory: Arc<dyn PermissionedProcessRunnerFactory> = Arc::new(
            StaticPermissionedProcessRunnerFactory::new(Arc::clone(&runner)),
        );
        let admission = AcceptedLocalWorkspaceProcessAdmission::accept_host();
        let backend = LocalProcessBackend::from_parts(admission, runner, factory);

        assert_eq!(backend.new_session().admission(), admission);
    }

    #[test]
    fn backend_mode_is_semantic_and_does_not_encode_a_platform() {
        assert_ne!(ProcessBackendMode::Host, ProcessBackendMode::Isolated);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn backend_selection_binds_admission_to_the_selected_behavior() {
        let temp = tempfile::tempdir().expect("temporary workspace should be created");
        let host = LocalProcessBackend::new(
            temp.path(),
            ProcessBackendMode::Host,
            ProcessBackendOptions::new(),
        )
        .expect("host backend should build");
        assert_eq!(
            host.new_session().admission().sandbox_profile(),
            merry_runtime::LocalWorkspaceProcessSandboxProfile::Host
        );

        let isolated = LocalProcessBackend::new(
            temp.path(),
            ProcessBackendMode::Isolated,
            ProcessBackendOptions::new(),
        )
        .expect("isolated backend should build before process execution");
        assert_eq!(
            isolated.new_session().admission().sandbox_profile(),
            merry_runtime::LocalWorkspaceProcessSandboxProfile::LocalWorkspace
        );
        assert_eq!(
            isolated.new_session().admission().sandbox_profile(),
            merry_runtime::LocalWorkspaceProcessSandboxProfile::LocalWorkspace
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn isolated_backend_reports_unavailable_platform() {
        let temp = tempfile::tempdir().expect("temporary workspace should be created");
        let error = LocalProcessBackend::new(
            temp.path(),
            ProcessBackendMode::Isolated,
            ProcessBackendOptions::new(),
        )
        .expect_err("isolated backend should report unsupported platforms");

        assert!(matches!(
            error,
            ProcessBackendError::UnsupportedIsolation { .. }
        ));
    }

    #[test]
    fn backend_options_default_to_no_host_overrides() {
        let options = ProcessBackendOptions::new();
        assert!(options.path_rules().is_empty());
        assert!(options.host_integrations().is_empty());
        assert!(options.environment_overrides().is_empty());
    }
}
