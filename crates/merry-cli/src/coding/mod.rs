mod error;
mod process;
mod runtime;
mod sandbox;

pub(crate) use error::CodingRuntimeError;
pub(crate) use merry::profiles::{
    CodingModelRoleConfig as RuntimeRoleProviderConfig, CodingPermissionPolicy,
    CodingSubagentsConfig, CodingTrustMode,
};
#[cfg(test)]
pub(crate) use process::ActionProcessBackend;
#[cfg(test)]
pub(crate) use process::fixed_process_backend;
pub(crate) use process::{
    ActionProcessBackendOptions, ProcessExecutionMode, action_process_runner,
    action_process_runner_for_mode,
};
#[cfg(test)]
pub(crate) use runtime::build_headless_coding;
#[cfg(test)]
pub(crate) use runtime::{CodingRuntimeOptions, build_coding_runtime, resume_headless_coding};
pub(crate) use runtime::{
    HeadlessCodingRuntimeInput, build_headless_coding_with_policy_composition,
    resume_headless_coding_composition_with_policy,
};
pub(crate) use sandbox::{coding_agent_process_admission, coding_agent_requires_sandbox_error};

#[cfg(test)]
mod tests;
