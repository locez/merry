use super::CodingRuntimeError;
use merry_runtime::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessEnvironment, BwrapProcessRunner,
    BwrapSessionPermissions, HostIntegration, PathAccessRule, PermissionedProcessRunnerFactory,
    ProcessRunner, TokioProcessRunner, UnrestrictedPermissionedProcessRunnerFactory,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Selects the process and outer-sandbox boundary for the coding product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessExecutionMode {
    /// Run validated actions directly in the host process environment.
    Unrestricted,
    /// Keep the per-action bubblewrap boundary without Merry's outer re-exec.
    InnerOnly,
    /// Run the per-action bubblewrap boundary inside Merry's outer sandbox.
    OuterAndInner,
}

impl ProcessExecutionMode {
    pub(crate) const fn uses_inner_sandbox(self) -> bool {
        !matches!(self, Self::Unrestricted)
    }
}

#[derive(Clone)]
pub(crate) struct ActionProcessBackend {
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    new_session: Arc<dyn Fn() -> Self + Send + Sync>,
}

#[derive(Clone, Default)]
pub(crate) struct ActionProcessBackendOptions {
    pub(crate) path_rules: Vec<PathAccessRule>,
    pub(crate) network_allowed: bool,
    pub(crate) host_integrations: Vec<HostIntegration>,
    pub(crate) environment_overrides: Vec<(OsString, OsString)>,
}

impl ActionProcessBackend {
    pub(crate) fn runner(&self) -> Arc<dyn ProcessRunner> {
        Arc::clone(&self.runner)
    }

    pub(crate) fn permissioned_factory(&self) -> Arc<dyn PermissionedProcessRunnerFactory> {
        Arc::clone(&self.permissioned_factory)
    }

    pub(crate) fn new_session(&self) -> Self {
        (self.new_session)()
    }

    pub(crate) fn from_parts(
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        let child_runner = Arc::clone(&runner);
        let child_permissioned_factory = Arc::clone(&permissioned_factory);
        Self {
            runner,
            permissioned_factory,
            new_session: Arc::new(move || {
                Self::from_parts(
                    Arc::clone(&child_runner),
                    Arc::clone(&child_permissioned_factory),
                )
            }),
        }
    }

    pub(crate) fn from_bwrap_options(
        workspace_root: PathBuf,
        options: ActionProcessBackendOptions,
    ) -> Result<Self, CodingRuntimeError> {
        let environment = BwrapProcessEnvironment::from_current_process()
            .with_host_integrations(options.host_integrations.clone())
            .with_overrides(options.environment_overrides.clone())?
            .validate_for_workspace(&workspace_root)?;
        Ok(Self::from_bwrap_options_with_environment(
            workspace_root,
            options,
            environment,
        ))
    }

    fn from_bwrap_options_with_environment(
        workspace_root: PathBuf,
        options: ActionProcessBackendOptions,
        environment: BwrapProcessEnvironment,
    ) -> Self {
        let ActionProcessBackendOptions {
            path_rules,
            network_allowed,
            ..
        } = options.clone();
        let session_permissions = BwrapSessionPermissions::new();
        let mut runner = BwrapProcessRunner::new_at_workspace_root(&workspace_root)
            .with_environment(environment.clone())
            .with_path_rules(path_rules.clone())
            .with_session_permissions(session_permissions.clone());
        if network_allowed {
            runner = runner.allow_network();
        }
        let mut permissioned_factory =
            BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(&workspace_root)
                .with_environment(environment.clone())
                .with_path_rules(path_rules)
                .with_session_permissions(session_permissions);
        if network_allowed {
            permissioned_factory = permissioned_factory.allow_base_network();
        }

        let child_workspace_root = workspace_root.clone();
        let child_options = options;
        let child_environment = environment;
        Self {
            runner: Arc::new(runner),
            permissioned_factory: Arc::new(permissioned_factory),
            new_session: Arc::new(move || {
                Self::from_bwrap_options_with_environment(
                    child_workspace_root.clone(),
                    child_options.clone(),
                    child_environment.clone(),
                )
            }),
        }
    }

    pub(crate) fn from_unrestricted_options(
        workspace_root: PathBuf,
        options: ActionProcessBackendOptions,
    ) -> Result<Self, CodingRuntimeError> {
        let runner = Arc::new(
            TokioProcessRunner::new_at_workspace_root(workspace_root)
                .with_environment_overrides(options.environment_overrides)?,
        );
        let permissioned_factory = Arc::new(UnrestrictedPermissionedProcessRunnerFactory::new(
            Arc::clone(&runner) as Arc<dyn ProcessRunner>,
        ));
        Ok(Self::from_parts(runner, permissioned_factory))
    }
}

pub(crate) fn action_process_runner(
    workspace_root: &Path,
    options: ActionProcessBackendOptions,
) -> Result<ActionProcessBackend, CodingRuntimeError> {
    action_process_runner_for_mode(workspace_root, options, ProcessExecutionMode::InnerOnly)
}

pub(crate) fn action_process_runner_for_mode(
    workspace_root: &Path,
    options: ActionProcessBackendOptions,
    mode: ProcessExecutionMode,
) -> Result<ActionProcessBackend, CodingRuntimeError> {
    match mode {
        ProcessExecutionMode::Unrestricted => {
            ActionProcessBackend::from_unrestricted_options(workspace_root.to_path_buf(), options)
        }
        ProcessExecutionMode::InnerOnly | ProcessExecutionMode::OuterAndInner => {
            ActionProcessBackend::from_bwrap_options(workspace_root.to_path_buf(), options)
        }
    }
}
