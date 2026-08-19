mod error;
mod process;
mod roles;
mod runtime;
mod sandbox;

pub(crate) use error::CodingRuntimeError;
#[cfg(test)]
pub(crate) use process::ActionProcessBackend;
#[cfg(test)]
pub(crate) use process::fixed_process_backend;
pub(crate) use process::{
    ActionProcessBackendOptions, ProcessExecutionMode, action_process_runner,
    action_process_runner_for_mode,
};
pub(crate) use roles::RuntimeRoleProviderConfig;
#[cfg(test)]
pub(crate) use runtime::{CodingRuntimeOptions, build_coding_runtime, resume_headless_coding};
pub(crate) use runtime::{
    CodingSubagentsConfig, HeadlessCodingRuntimeInput, build_headless_coding,
    build_headless_coding_with_permission_review_mode,
    build_headless_coding_with_permission_source, coding_agent_loop_config,
    resume_headless_coding_with_permission_source,
};
pub(crate) use sandbox::{coding_agent_process_admission, coding_agent_requires_sandbox_error};

#[cfg(test)]
mod tests;
