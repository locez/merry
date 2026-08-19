use super::CodingRuntimeError;
#[cfg(test)]
use merry_process::ProcessSession;
use merry_process::{
    LocalProcessBackend, ProcessBackend, ProcessBackendMode, ProcessBackendOptions,
};
use std::{path::Path, sync::Arc};

pub(crate) type ActionProcessBackend = Arc<dyn ProcessBackend>;
pub(crate) type ActionProcessBackendOptions = ProcessBackendOptions;

/// Selects the process and outer-sandbox boundary for the coding product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessExecutionMode {
    /// Run validated actions directly in the host process environment.
    Unrestricted,
    /// Keep the per-action process boundary without Merry's outer re-exec.
    InnerOnly,
    /// Run the per-action process boundary inside Merry's outer sandbox.
    OuterAndInner,
}

impl ProcessExecutionMode {
    pub(crate) const fn uses_inner_sandbox(self) -> bool {
        !matches!(self, Self::Unrestricted)
    }

    pub(crate) const fn backend_mode(self) -> ProcessBackendMode {
        match self {
            Self::Unrestricted => ProcessBackendMode::Host,
            Self::InnerOnly | Self::OuterAndInner => ProcessBackendMode::Isolated,
        }
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
    let backend = LocalProcessBackend::new(workspace_root, mode.backend_mode(), options)?;
    Ok(Arc::new(backend))
}

#[cfg(test)]
pub(crate) fn fixed_process_backend(session: ProcessSession) -> ActionProcessBackend {
    Arc::new(LocalProcessBackend::from_session(session))
}
