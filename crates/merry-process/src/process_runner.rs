//! Host process adapter composition.
//!
//! This module wires the concrete environment, permission, sandbox, and
//! execution components while keeping host-process details out of runtime.

mod background;
mod environment;
mod execution;
mod permissions;
mod review;
mod tokio_runner;

pub use environment::{WorkspaceProcessEnvironment, action_process_path};
pub use permissions::{
    ProcessSessionPermissions, WorkspaceProcessRunner, WorkspaceProcessRunnerFactory,
};
pub use tokio_runner::TokioProcessRunner;

pub(crate) use background::BackgroundSupervisor;
pub(crate) use environment::process_current_dir;

#[cfg(test)]
pub(crate) mod tests;
