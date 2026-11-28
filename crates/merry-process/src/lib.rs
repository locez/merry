//! Host-process adapters for Merry's provider-neutral process contract.
//!
//! [`merry_runtime`] owns process intent, admission, permission requests, and
//! execution evidence. This crate owns the host-facing runner selection and
//! binds the runners used by one runtime owner to one [`ProcessSession`]. A
//! platform can replace the concrete backend without changing coding
//! composition or runtime call sites.

mod baseline;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod confinement;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[path = "confinement/unsupported.rs"]
mod confinement;
mod gpg;
mod host_path;
mod host_resources;
mod process_runner;

pub use baseline::{default_development_path_rules, default_development_path_rules_with_env};
pub use confinement::{probe_workspace_sandbox, sandbox_helper_main, sandbox_helper_requested};
pub use gpg::GpgAgentSockets;
pub use host_path::resolve_host_path;
pub use host_resources::{HostPathKind, HostPathMetadata, ssh_known_hosts};

pub use process_runner::{TokioProcessRunner, action_process_path};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use process_runner::{
    ProcessSessionPermissions, WorkspaceProcessEnvironment, WorkspaceProcessRunner,
    WorkspaceProcessRunnerFactory,
};

use merry_core::{ProtectedHostPaths, host_paths::is_clean_absolute_path};
use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, HostIntegration, PathAccess, PathAccessRule,
    PathAccessRuleSource, PermissionRequest, PermissionedProcessRunnerFactory, ProcessRunner,
    ProcessRunnerError, ProcessSessionPermissionView,
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
/// need to name Landlock, seccomp, or another concrete mechanism.
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
/// [`ProcessBackend::new_session`] or [`ProcessBackend::new_session_with`],
/// whose backend controls session isolation.
#[derive(Clone)]
pub struct ProcessSession {
    admission: AcceptedLocalWorkspaceProcessAdmission,
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    permission_view: Option<ProcessSessionPermissionView>,
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
            permission_view: None,
        }
    }

    /// Attaches a read-only view of this session's retained permissions so child
    /// runtimes may inherit already-approved ordinary capabilities without
    /// mutating the parent session.
    #[must_use]
    pub fn with_permission_view(mut self, view: ProcessSessionPermissionView) -> Self {
        self.permission_view = Some(view);
        self
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

    /// Returns a read-only view of capabilities retained by this session, when
    /// backed by a session-scoped permission store.
    #[must_use]
    pub fn permission_view(&self) -> Option<ProcessSessionPermissionView> {
        self.permission_view.clone()
    }
}

/// Request options for deriving a scoped [`ProcessSession`] from a [`ProcessBackend`].
#[derive(Debug, Clone)]
pub struct ProcessSessionRequest {
    workspace_root: Option<PathBuf>,
    workspace_writable: bool,
    inherited_permissions: Option<ProcessSessionPermissionView>,
    extra_path_rules: Vec<PathAccessRule>,
}

impl Default for ProcessSessionRequest {
    fn default() -> Self {
        Self {
            workspace_root: None,
            workspace_writable: true,
            inherited_permissions: None,
            extra_path_rules: Vec::new(),
        }
    }
}

impl ProcessSessionRequest {
    /// Creates a default session request inheriting the backend's workspace root
    /// and writability.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the workspace root directory for the new session (for example,
    /// an isolated git worktree).
    #[must_use]
    pub fn with_workspace_root(mut self, workspace_root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(workspace_root.into());
        self
    }

    /// Keeps the workspace readable and not writable for isolated actions in
    /// this session.
    #[must_use]
    pub fn read_only_workspace(mut self) -> Self {
        self.workspace_writable = false;
        self
    }

    /// Sets whether isolated actions in this session may write inside the
    /// workspace root.
    ///
    /// A request cannot widen a backend that was already configured read-only.
    #[must_use]
    pub fn with_workspace_writable(mut self, writable: bool) -> Self {
        self.workspace_writable = writable;
        self
    }

    /// Seeds the new session with a read-only snapshot of an existing session's
    /// approved ordinary capabilities.
    #[must_use]
    pub fn with_inherited_permissions(mut self, view: ProcessSessionPermissionView) -> Self {
        self.inherited_permissions = Some(view);
        self
    }

    /// Appends additional filesystem rules for the new session.
    #[must_use]
    pub fn with_extra_path_rules(
        mut self,
        rules: impl IntoIterator<Item = PathAccessRule>,
    ) -> Self {
        self.extra_path_rules = rules.into_iter().collect();
        self
    }

    /// Returns the optional workspace root override.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    /// Returns whether the caller requested workspace writability.
    #[must_use]
    pub const fn workspace_writable(&self) -> bool {
        self.workspace_writable
    }

    /// Returns the optional inherited permission view.
    #[must_use]
    pub fn inherited_permissions(&self) -> Option<&ProcessSessionPermissionView> {
        self.inherited_permissions.as_ref()
    }

    /// Returns any extra filesystem access rules attached to this request.
    #[must_use]
    pub fn extra_path_rules(&self) -> &[PathAccessRule] {
        &self.extra_path_rules
    }
}

/// Stable host-process backend boundary consumed by coding composition.
///
/// The trait deliberately says nothing about how a platform enforces the
/// selected behavior. Linux confines each action with Landlock and seccomp.
/// Other targets report that isolation is unavailable instead of silently
/// running on the host.
pub trait ProcessBackend: Send + Sync {
    /// Creates the process session for one runtime owner.
    ///
    /// Each call returns a session with the backend's configured admission and
    /// runner capabilities. Backends may use a fresh session-scoped capability
    /// store for every call, which keeps parent and child runtime state isolated.
    fn new_session(&self) -> ProcessSession;

    /// Creates a process session configured with scoped request options.
    ///
    /// The default implementation delegates to [`Self::new_session`] for custom
    /// test or adapter backends that do not vary session parameters.
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessBackendError`] when the requested workspace root,
    /// sandbox rules, or inherited permissions cannot be validated.
    fn new_session_with(
        &self,
        _request: ProcessSessionRequest,
    ) -> Result<ProcessSession, ProcessBackendError> {
        Ok(self.new_session())
    }
}

/// Inputs shared by local process backend implementations.
#[derive(Debug, Clone)]
pub struct ProcessBackendOptions {
    /// Trusted filesystem rules installed for isolated actions.
    path_rules: Vec<PathAccessRule>,
    /// Isolated-session capability ceiling, not a grant to an ordinary runner.
    network_requests_allowed: bool,
    /// When false, the workspace itself is readable and not writable.
    workspace_writable: bool,
    /// Host integrations the embedding application authorizes for isolated actions.
    host_integrations: Vec<HostIntegration>,
    gpg_agent_sockets: Option<GpgAgentSockets>,
    /// Environment assignments validated and applied by the host backend.
    environment_overrides: Vec<(OsString, OsString)>,
}

impl Default for ProcessBackendOptions {
    fn default() -> Self {
        Self {
            path_rules: Vec::new(),
            network_requests_allowed: true,
            workspace_writable: true,
            host_integrations: Vec::new(),
            gpg_agent_sockets: None,
            environment_overrides: Vec::new(),
        }
    }
}

impl ProcessBackendOptions {
    /// Creates default options without preauthorized process capabilities.
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

    /// Appends filesystem rules installed for isolated actions.
    #[must_use]
    pub fn extend_path_rules(
        mut self,
        path_rules: impl IntoIterator<Item = PathAccessRule>,
    ) -> Self {
        self.path_rules.extend(path_rules);
        self
    }

    /// Appends the default local development toolchain and cache baseline rules.
    #[must_use]
    pub fn with_default_development_baseline(self, home: &Path) -> Self {
        self.extend_path_rules(default_development_path_rules(home))
    }

    /// Appends Merry's product-private deny rules from a shared
    /// [`ProtectedHostPaths`] contract.
    #[must_use]
    pub fn with_protected_host_paths(self, protected: &ProtectedHostPaths) -> Self {
        self.extend_path_rules(protected.as_slice().iter().cloned().map(|path| {
            PathAccessRule::new(path, PathAccess::Deny, PathAccessRuleSource::ProductPrivate)
        }))
    }

    /// Appends Merry's non-secret writable product directories (config, skills,
    /// and optional durable memory root).
    #[must_use]
    pub fn with_product_writable_paths(
        mut self,
        config_dir: &Path,
        skill_roots: &[PathBuf],
        memory_root: Option<&Path>,
    ) -> Self {
        if is_clean_absolute_path(config_dir) {
            self.path_rules.push(PathAccessRule::new(
                config_dir.to_path_buf(),
                PathAccess::ReadWrite,
                PathAccessRuleSource::ProductConfig,
            ));
        }
        for skill_root in skill_roots {
            if is_clean_absolute_path(skill_root) {
                self.path_rules.push(PathAccessRule::new(
                    skill_root.clone(),
                    PathAccess::ReadWrite,
                    PathAccessRuleSource::ProductConfig,
                ));
            }
        }
        if let Some(memory_root) = memory_root
            && is_clean_absolute_path(memory_root)
        {
            self.path_rules.push(PathAccessRule::new(
                memory_root.to_path_buf(),
                PathAccess::ReadWrite,
                PathAccessRuleSource::DurableMemory,
            ));
        }
        self
    }

    /// Sets whether isolated process sessions may request network capability.
    ///
    /// Defaults to true, which still requires approval for each action. False
    /// rejects network requests before review. Explicit host execution is not
    /// isolated and is not restricted by this ceiling.
    #[must_use]
    pub fn with_network_requests_allowed(mut self, allowed: bool) -> Self {
        self.network_requests_allowed = allowed;
        self
    }

    /// Keeps the workspace readable and not writable for isolated actions.
    ///
    /// Baseline paths such as `/tmp` stay writable. Host execution ignores this.
    #[must_use]
    pub fn read_only_workspace(mut self) -> Self {
        self.workspace_writable = false;
        self
    }

    /// Sets host integrations the embedding application authorizes.
    ///
    /// The embedding application owns this decision; the backend treats the
    /// listed integrations as part of the ordinary action baseline instead of
    /// requiring a per-action request for them. Merry's CLI derives the list
    /// from user-trusted global configuration, which preauthorizes the inner
    /// action sandbox; project-local configuration never does. Endpoint
    /// validation and path policy still apply per action, so a `deny_paths`
    /// entry can keep a listed integration masked when it is not inside a
    /// broader grant.
    #[must_use]
    pub fn with_host_integrations(
        mut self,
        host_integrations: impl IntoIterator<Item = HostIntegration>,
    ) -> Self {
        self.host_integrations = host_integrations.into_iter().collect();
        self
    }

    /// Installs explicitly discovered native and auxiliary GnuPG socket paths.
    #[must_use]
    pub fn with_gpg_agent_sockets(mut self, sockets: GpgAgentSockets) -> Self {
        self.gpg_agent_sockets = Some(sockets);
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

    /// Returns whether isolated sessions may request, not preauthorize, network access.
    #[must_use]
    pub fn network_requests_allowed(&self) -> bool {
        self.network_requests_allowed
    }

    /// Returns host integrations already approved by the embedding application.
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
    new_session_with: Arc<
        dyn Fn(ProcessSessionRequest) -> Result<ProcessSession, ProcessBackendError> + Send + Sync,
    >,
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
        let scoped_session = session;
        Self {
            new_session: Arc::new(move || child_session.clone()),
            new_session_with: Arc::new(move |_request| Ok(scoped_session.clone())),
        }
    }

    #[cfg(test)]
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
        let workspace_root = workspace_root.into();
        let environment_overrides = options.environment_overrides;
        let runner = Arc::new(
            TokioProcessRunner::new_at_workspace_root(workspace_root.clone())
                .with_environment_overrides(environment_overrides.clone())?,
        );
        let permissioned_factory = Arc::new(UnrestrictedPermissionedProcessRunnerFactory::new(
            Arc::clone(&runner) as Arc<dyn ProcessRunner>,
        ));
        let default_session = ProcessSession::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_host(),
            runner,
            permissioned_factory,
        );
        let fallback_session = default_session.clone();
        Ok(Self {
            new_session: Arc::new(move || default_session.clone()),
            new_session_with: Arc::new(move |request| {
                let Some(root_override) = request.workspace_root else {
                    return Ok(fallback_session.clone());
                };
                let runner = Arc::new(
                    TokioProcessRunner::new_at_workspace_root(root_override)
                        .with_environment_overrides(environment_overrides.clone())?,
                );
                let permissioned_factory =
                    Arc::new(UnrestrictedPermissionedProcessRunnerFactory::new(
                        Arc::clone(&runner) as Arc<dyn ProcessRunner>,
                    ));
                Ok(ProcessSession::from_parts(
                    AcceptedLocalWorkspaceProcessAdmission::accept_host(),
                    runner,
                    permissioned_factory,
                ))
            }),
        })
    }

    /// Creates the current isolated local-process implementation.
    ///
    /// On Linux, each action is confined with Landlock and, unless that action
    /// was granted network, a seccomp filter. On macOS, each action is confined
    /// with a Seatbelt profile. Other targets return
    /// [`ProcessBackendError::UnsupportedIsolation`].
    ///
    /// # Errors
    ///
    /// Returns a [`ProcessBackendError`] when the workspace or sandbox
    /// environment is invalid, a configured path cannot be enforced, or this
    /// target has no isolated backend.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn sandboxed(
        workspace_root: impl Into<PathBuf>,
        options: ProcessBackendOptions,
    ) -> Result<Self, ProcessBackendError> {
        let workspace_root = workspace_root.into();
        let mut environment = WorkspaceProcessEnvironment::from_current_process()
            .with_host_integrations(options.host_integrations)
            .with_overrides(options.environment_overrides)?
            .validate_for_workspace(&workspace_root)?;
        if let Some(sockets) = options.gpg_agent_sockets {
            environment = environment.with_gpg_agent_sockets(sockets);
        }
        let mut rules_runner =
            WorkspaceProcessRunner::new_at_workspace_root(workspace_root.clone())
                .with_environment(environment.clone())
                .with_path_rules(options.path_rules.iter().cloned());
        if !options.workspace_writable {
            rules_runner = rules_runner.read_only_workspace();
        }
        rules_runner.ensure_sandbox_rules()?;
        Ok(Self::from_sandboxed_parts(
            workspace_root,
            environment,
            options.path_rules,
            options.network_requests_allowed,
            options.workspace_writable,
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn sandboxed(
        workspace_root: impl Into<PathBuf>,
        options: ProcessBackendOptions,
    ) -> Result<Self, ProcessBackendError> {
        let _ = (workspace_root.into(), options);
        Err(ProcessBackendError::UnsupportedIsolation {
            target_os: std::env::consts::OS,
        })
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn from_sandboxed_parts(
        workspace_root: PathBuf,
        environment: WorkspaceProcessEnvironment,
        path_rules: Vec<PathAccessRule>,
        network_requests_allowed: bool,
        workspace_writable: bool,
    ) -> Self {
        let child_workspace_root = workspace_root.clone();
        let child_environment = environment.clone();
        let child_path_rules = path_rules.clone();
        let scoped_workspace_root = workspace_root;
        let scoped_environment = environment;
        let scoped_path_rules = path_rules;
        Self {
            new_session: Arc::new(move || {
                Self::sandboxed_session(
                    &child_workspace_root,
                    &child_environment,
                    &child_path_rules,
                    network_requests_allowed,
                    workspace_writable,
                    ProcessSessionPermissions::new(),
                )
            }),
            new_session_with: Arc::new(move |request| {
                let target_root = request
                    .workspace_root
                    .as_deref()
                    .unwrap_or(&scoped_workspace_root);
                // Intersection: a scoped session can narrow a writable backend
                // to read-only, never widen a read-only backend to writable.
                let effective_writable = workspace_writable && request.workspace_writable;
                let session_permissions = match request.inherited_permissions.as_ref() {
                    Some(view) => ProcessSessionPermissions::from_inherited_view(view)?,
                    None => ProcessSessionPermissions::new(),
                };
                let mut combined_rules =
                    Vec::with_capacity(scoped_path_rules.len() + request.extra_path_rules.len());
                combined_rules.extend(scoped_path_rules.iter().cloned());
                combined_rules.extend(request.extra_path_rules);
                let validated_env = if request.workspace_root.is_some() {
                    scoped_environment
                        .clone()
                        .validate_for_workspace(target_root)?
                } else {
                    scoped_environment.clone()
                };
                if request.workspace_root.is_some() || !combined_rules.is_empty() {
                    let mut rules_runner =
                        WorkspaceProcessRunner::new_at_workspace_root(target_root)
                            .with_environment(validated_env.clone())
                            .with_path_rules(combined_rules.iter().cloned());
                    if !effective_writable {
                        rules_runner = rules_runner.read_only_workspace();
                    }
                    rules_runner.ensure_sandbox_rules()?;
                }
                Ok(Self::sandboxed_session(
                    target_root,
                    &validated_env,
                    &combined_rules,
                    network_requests_allowed,
                    effective_writable,
                    session_permissions,
                ))
            }),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn sandboxed_session(
        workspace_root: &Path,
        environment: &WorkspaceProcessEnvironment,
        path_rules: &[PathAccessRule],
        network_requests_allowed: bool,
        workspace_writable: bool,
        session_permissions: ProcessSessionPermissions,
    ) -> ProcessSession {
        let permission_view = session_permissions.view();
        let background = crate::process_runner::BackgroundSupervisor::new();
        let mut runner = WorkspaceProcessRunner::new_at_workspace_root(workspace_root)
            .with_environment(environment.clone())
            .with_path_rules(path_rules.iter().cloned())
            .with_background(background.clone());
        let mut permissioned_factory =
            WorkspaceProcessRunnerFactory::new_at_workspace_root(workspace_root)
                .with_environment(environment.clone())
                .with_path_rules(path_rules.iter().cloned())
                .with_network_requests_allowed(network_requests_allowed)
                .with_background(background);
        if !workspace_writable {
            runner = runner.read_only_workspace();
            permissioned_factory = permissioned_factory.read_only_workspace();
        }
        let runner = runner.with_session_permissions(session_permissions.clone());
        let permissioned_factory =
            permissioned_factory.with_session_permissions(session_permissions);
        ProcessSession::from_parts(
            AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
            Arc::new(runner),
            Arc::new(permissioned_factory),
        )
        .with_permission_view(permission_view)
    }
}

impl ProcessBackend for LocalProcessBackend {
    fn new_session(&self) -> ProcessSession {
        (self.new_session)()
    }

    fn new_session_with(
        &self,
        request: ProcessSessionRequest,
    ) -> Result<ProcessSession, ProcessBackendError> {
        (self.new_session_with)(request)
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
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    use super::ProcessBackendError;
    use super::{
        LocalProcessBackend, ProcessBackend, ProcessBackendMode, ProcessBackendOptions,
        TokioProcessRunner,
    };
    use merry_runtime::{
        AcceptedLocalWorkspaceProcessAdmission, PermissionedProcessRunnerFactory, ProcessRunner,
        StaticPermissionedProcessRunnerFactory,
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    use std::path::PathBuf;
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn isolated_backend_reports_unavailable_platform() {
        let temp = tempfile::tempdir().expect("temporary workspace should be created");
        let Err(error) = LocalProcessBackend::new(
            temp.path(),
            ProcessBackendMode::Isolated,
            ProcessBackendOptions::new(),
        ) else {
            panic!("isolated backend should report unsupported platforms");
        };

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
        assert!(options.workspace_writable);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn sandboxed_backend_allows_user_grant_covering_product_private_directory() {
        use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};

        let workspace = tempfile::tempdir().expect("workspace");
        let parent = PathBuf::from("/var/merry-path-check");
        let private = parent.join("merry/providers");
        let options = ProcessBackendOptions::new().with_path_rules([
            PathAccessRule::new(
                parent,
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
            PathAccessRule::new(
                private,
                PathAccess::Deny,
                PathAccessRuleSource::ProductPrivate,
            ),
        ]);

        LocalProcessBackend::sandboxed(workspace.path(), options)
            .expect("explicit user grant covering a ProductPrivate path must not block startup");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(flavor = "current_thread")]
    async fn new_session_with_read_only_workspace_and_inherited_permissions_stays_isolated() {
        use crate::{
            ProcessSessionRequest,
            process_runner::tests::{fixture_root, permission_request},
        };
        use merry_runtime::{
            ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus, ProcessRunnerContext,
        };
        use serde_json::json;

        let root = tempfile::tempdir_in(fixture_root()).expect("fixture root outside /tmp");
        let workspace = root.path().join("workspace");
        let extra = root.path().join("extra");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::create_dir_all(&extra).expect("extra dir");
        std::fs::write(workspace.join("hello.txt"), "hi\n").expect("seed workspace");
        std::fs::write(extra.join("shared.txt"), "from-extra\n").expect("seed extra");

        let backend = LocalProcessBackend::sandboxed(&workspace, ProcessBackendOptions::new())
            .expect("isolated backend should build");
        let parent_session = backend.new_session();
        let parent_view = parent_session
            .permission_view()
            .expect("sandboxed session should expose a permission view");

        // Grant the parent session read access to `extra` via an approved request.
        let grant_request = permission_request(json!({
            "requested": {
                "paths": [{
                    "path": extra.display().to_string(),
                    "access": "ro"
                }]
            },
            "for_action": { "command": "true", "cwd": "." }
        }));
        let _ = parent_session
            .permissioned_factory()
            .runner_for(&grant_request);

        let child_session = backend
            .new_session_with(
                ProcessSessionRequest::new()
                    .read_only_workspace()
                    .with_inherited_permissions(parent_view.clone()),
            )
            .expect("read-only child session should build");
        let child_view = child_session
            .permission_view()
            .expect("child session should expose its own permission view");
        assert_eq!(
            child_view
                .snapshot()
                .expect("child snapshot")
                .path_rules()
                .len(),
            parent_view
                .snapshot()
                .expect("parent snapshot")
                .path_rules()
                .len(),
            "child should inherit parent's approved ordinary path grants"
        );

        // Child can read inside workspace & inherited extra dir, and cannot write workspace.
        let read_intent = ProcessActionIntent::new(
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!("cat hello.txt && cat {}/shared.txt", extra.display()),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            4096,
            4096,
        )
        .expect("valid read intent");
        let read_out = child_session
            .runner()
            .run(
                read_intent,
                ProcessRunnerContext::new(tokio_util::sync::CancellationToken::new()),
            )
            .await
            .expect("read command should run in read-only sandbox");
        assert_eq!(
            read_out.status(),
            ProcessExitStatus::Exited(0),
            "stderr: {}",
            read_out.stderr_text()
        );
        assert!(read_out.stdout_text().contains("hi"));
        assert!(read_out.stdout_text().contains("from-extra"));

        let write_intent = ProcessActionIntent::new(
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "touch should-fail.txt".to_owned(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            4096,
            4096,
        )
        .expect("valid write intent");
        let write_out = child_session
            .runner()
            .run(
                write_intent,
                ProcessRunnerContext::new(tokio_util::sync::CancellationToken::new()),
            )
            .await
            .expect("write command should execute and be refused by OS sandbox");
        assert_ne!(write_out.status(), ProcessExitStatus::Exited(0));
        assert!(!workspace.join("should-fail.txt").exists());

        // Child's extra grant must not leak back to parent.
        let child_only_dir = root.path().join("child-only");
        std::fs::create_dir_all(&child_only_dir).expect("child only dir");
        let child_grant_request = permission_request(json!({
            "requested": {
                "paths": [{
                    "path": child_only_dir.display().to_string(),
                    "access": "ro"
                }]
            },
            "for_action": { "command": "true", "cwd": "." }
        }));
        let _ = child_session
            .permissioned_factory()
            .runner_for(&child_grant_request);
        assert_eq!(
            parent_view
                .snapshot()
                .expect("parent snapshot")
                .path_rules()
                .len(),
            1,
            "child-only grants must not mutate parent session permissions"
        );
    }

    #[test]
    fn backend_options_attach_protected_and_writable_product_paths() {
        let home = if cfg!(windows) {
            std::path::PathBuf::from(r"C:\Users\alice")
        } else {
            std::path::PathBuf::from("/home/alice")
        };
        let state_dir = home.join(".local").join("state").join("merry");
        let providers_dir = home
            .join(".local")
            .join("share")
            .join("merry")
            .join("providers");
        let config_dir = home.join(".config").join("merry");
        let skills_dir = config_dir.join("skills");
        let memory_dir = home
            .join(".local")
            .join("share")
            .join("merry")
            .join("memory");

        let protected = merry_core::ProtectedHostPaths::from_merry_dirs(
            state_dir.clone(),
            providers_dir.clone(),
        );
        let options = ProcessBackendOptions::new()
            .with_product_writable_paths(&config_dir, &[skills_dir], Some(&memory_dir))
            .with_protected_host_paths(&protected);

        assert!(options.path_rules().iter().any(|rule| {
            rule.path() == state_dir
                && rule.access() == merry_runtime::PathAccess::Deny
                && rule.source() == merry_runtime::PathAccessRuleSource::ProductPrivate
        }));
        assert!(options.path_rules().iter().any(|rule| {
            rule.path() == providers_dir
                && rule.access() == merry_runtime::PathAccess::Deny
                && rule.source() == merry_runtime::PathAccessRuleSource::ProductPrivate
        }));
        assert!(options.path_rules().iter().any(|rule| {
            rule.path() == config_dir
                && rule.access() == merry_runtime::PathAccess::ReadWrite
                && rule.source() == merry_runtime::PathAccessRuleSource::ProductConfig
        }));
        assert!(options.path_rules().iter().any(|rule| {
            rule.path() == memory_dir
                && rule.access() == merry_runtime::PathAccess::ReadWrite
                && rule.source() == merry_runtime::PathAccessRuleSource::DurableMemory
        }));
    }
}
