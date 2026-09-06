//! Host process adapter composition.
//!
//! This module wires the concrete environment, permission, sandbox, and
//! execution components while keeping host-process details out of runtime.

mod environment;
mod execution;
mod permissions;
mod sandbox;
mod tokio_runner;

pub use environment::BwrapProcessEnvironment;
pub use permissions::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessRunner, BwrapSessionPermissions,
};
pub use tokio_runner::TokioProcessRunner;

#[cfg(test)]
pub(crate) use environment::process_current_dir;
#[cfg(test)]
pub(crate) use sandbox::{bwrap_process_plan, bwrap_process_plan_with_environment};

#[cfg(test)]
mod tests;
