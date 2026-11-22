//! Host process adapter composition.
//!
//! This module wires the concrete environment, permission, sandbox, and
//! execution components while keeping host-process details out of runtime.

mod background;
mod environment;
mod execution;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod permissions;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod review;
mod tokio_runner;
#[cfg(windows)]
mod windows_job;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use environment::WorkspaceProcessEnvironment;
pub use environment::action_process_path;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use permissions::{
    ProcessSessionPermissions, WorkspaceProcessRunner, WorkspaceProcessRunnerFactory,
};
pub use tokio_runner::TokioProcessRunner;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use background::BackgroundSupervisor;
pub(crate) use environment::process_current_dir;

#[cfg(test)]
pub(crate) mod tests;
