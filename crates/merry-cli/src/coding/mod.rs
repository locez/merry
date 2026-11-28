mod error;
mod permission;
mod process;
mod runtime;
mod sandbox;

pub(crate) use error::CodingRuntimeError;
pub(crate) use merry::profiles::{
    CodingAutoFallback, CodingModelRoleConfig as RuntimeRoleProviderConfig, CodingPermissionMode,
    CodingPermissionPolicy, CodingSubagentsConfig,
};
pub(crate) use permission::{AutoFallback, PermissionMode};
#[cfg(test)]
pub(crate) use process::ActionProcessBackend;
#[cfg(test)]
pub(crate) use process::fixed_process_backend;
pub(crate) use process::{
    ActionProcessBackendOptions, SandboxMode, action_process_runner_for_mode,
};
#[cfg(test)]
pub(crate) use runtime::build_headless_coding;
#[cfg(test)]
pub(crate) use runtime::{
    CodingRuntimeOptions, build_coding_runtime, build_headless_coding_with_policy_composition,
    resume_headless_coding,
};
pub(crate) use runtime::{HeadlessCodingRuntimeInput, build_coding_session_builder};
pub(crate) use sandbox::coding_agent_process_admission;

#[cfg(test)]
mod tests;
