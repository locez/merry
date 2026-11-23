use super::CodingRuntimeError;
use clap::ValueEnum;
#[cfg(test)]
use merry_process::ProcessSession;
use merry_process::{
    LocalProcessBackend, ProcessBackend, ProcessBackendMode, ProcessBackendOptions,
};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

pub(crate) type ActionProcessBackend = Arc<dyn ProcessBackend>;
pub(crate) type ActionProcessBackendOptions = ProcessBackendOptions;

/// Selects whether process actions run in the workspace sandbox or on the host.
///
/// Defaults to [`SandboxMode::Workspace`] on Linux and macOS, and to
/// [`SandboxMode::Off`] on Windows where no kernel workspace sandbox is
/// available yet (explicit `--sandbox workspace` still fails closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub(crate) enum SandboxMode {
    /// Confine each action to the workspace sandbox.
    #[cfg_attr(not(windows), default)]
    Workspace,
    /// Run actions in the host process environment.
    #[cfg_attr(windows, default)]
    Off,
}

impl SandboxMode {
    pub(crate) const fn uses_action_sandbox(self) -> bool {
        matches!(self, Self::Workspace)
    }

    pub(crate) const fn backend_mode(self) -> ProcessBackendMode {
        match self {
            Self::Off => ProcessBackendMode::Host,
            Self::Workspace => ProcessBackendMode::Isolated,
        }
    }
}

pub(crate) fn action_process_runner(
    workspace_root: &Path,
    options: ActionProcessBackendOptions,
) -> Result<ActionProcessBackend, CodingRuntimeError> {
    action_process_runner_for_mode(workspace_root, options, SandboxMode::Workspace)
}

pub(crate) fn action_process_runner_for_mode(
    workspace_root: &Path,
    options: ActionProcessBackendOptions,
    mode: SandboxMode,
) -> Result<ActionProcessBackend, CodingRuntimeError> {
    let backend = LocalProcessBackend::new(workspace_root, mode.backend_mode(), options)?;
    Ok(Arc::new(backend))
}

#[cfg(test)]
pub(crate) fn fixed_process_backend(session: ProcessSession) -> ActionProcessBackend {
    Arc::new(LocalProcessBackend::from_session(session))
}
