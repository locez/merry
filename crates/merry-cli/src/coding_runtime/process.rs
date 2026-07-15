use super::CodingRuntimeError;
use merry_runtime::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, BwrapSessionPermissions,
    PathAccessRule, PermissionedProcessRunnerFactory, ProcessRunner,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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
    ) -> Self {
        let ActionProcessBackendOptions {
            path_rules,
            network_allowed,
        } = options.clone();
        let session_permissions = BwrapSessionPermissions::new();
        let mut runner = BwrapProcessRunner::new_at_workspace_root(&workspace_root)
            .with_path_rules(path_rules.clone())
            .with_session_permissions(session_permissions.clone());
        if network_allowed {
            runner = runner.allow_network();
        }
        let mut permissioned_factory =
            BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(&workspace_root)
                .with_path_rules(path_rules)
                .with_session_permissions(session_permissions);
        if network_allowed {
            permissioned_factory = permissioned_factory.allow_base_network();
        }

        let child_workspace_root = workspace_root.clone();
        let child_options = options;
        Self {
            runner: Arc::new(runner),
            permissioned_factory: Arc::new(permissioned_factory),
            new_session: Arc::new(move || {
                Self::from_bwrap_options(child_workspace_root.clone(), child_options.clone())
            }),
        }
    }
}

pub(crate) fn action_process_runner(
    workspace_root: &Path,
    options: ActionProcessBackendOptions,
) -> Result<ActionProcessBackend, CodingRuntimeError> {
    Ok(ActionProcessBackend::from_bwrap_options(
        workspace_root.to_path_buf(),
        options,
    ))
}
