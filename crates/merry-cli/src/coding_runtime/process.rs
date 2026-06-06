use super::CodingRuntimeError;
use merry_runtime::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, PathAccessRule,
    PermissionedProcessRunnerFactory, ProcessRunner,
};
use std::{path::Path, sync::Arc};

#[derive(Clone)]
pub(crate) struct ActionProcessBackend {
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
}

#[derive(Default)]
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

    pub(crate) fn from_parts(
        runner: Arc<dyn ProcessRunner>,
        permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
    ) -> Self {
        Self {
            runner,
            permissioned_factory,
        }
    }
}

pub(crate) fn action_process_runner(
    workspace_root: &Path,
    options: ActionProcessBackendOptions,
) -> Result<ActionProcessBackend, CodingRuntimeError> {
    let ActionProcessBackendOptions {
        path_rules,
        network_allowed,
    } = options;
    let mut runner = BwrapProcessRunner::new_at_workspace_root(workspace_root)
        .with_path_rules(path_rules.clone());
    if network_allowed {
        runner = runner.allow_network();
    }
    let mut permissioned_factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(workspace_root)
            .with_path_rules(path_rules);
    if network_allowed {
        permissioned_factory = permissioned_factory.allow_base_network();
    }
    Ok(ActionProcessBackend {
        runner: Arc::new(runner),
        permissioned_factory: Arc::new(permissioned_factory),
    })
}
