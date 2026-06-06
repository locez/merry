use crate::cli_error::{CliError, unexpected};
use crate::config::MerryConfig;
use merry_runtime::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, PermissionedProcessRunnerFactory,
    ProcessRunner,
};
use std::{path::Path, sync::Arc};

#[derive(Clone)]
pub(crate) struct ActionProcessBackend {
    runner: Arc<dyn ProcessRunner>,
    permissioned_factory: Arc<dyn PermissionedProcessRunnerFactory>,
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
    merry_config: Option<&MerryConfig>,
) -> Result<ActionProcessBackend, CliError> {
    let path_rules = merry_config
        .map(MerryConfig::trusted_global_path_rules)
        .transpose()
        .map_err(unexpected)?
        .unwrap_or_default();
    let network_allowed = merry_config
        .map(MerryConfig::permissions_network_allowed)
        .unwrap_or(false);
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
