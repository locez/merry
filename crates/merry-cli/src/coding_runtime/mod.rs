mod builder;
mod child_runtime;
mod error;
mod process;
mod profile;
mod project_rules;
mod roles;
mod sandbox;

#[cfg(test)]
pub(crate) use builder::resume_headless_coding_runtime;
pub(crate) use builder::{
    CodingLoopRuntimeOptions, CodingSubagentsConfig, HeadlessCodingRuntimeInput,
    build_coding_loop_runtime, build_headless_coding_runtime,
    build_headless_coding_runtime_with_permission_source, coding_agent_loop_config,
    resume_headless_coding_runtime_with_permission_source,
};
pub(crate) use error::CodingRuntimeError;
pub(crate) use process::{
    ActionProcessBackend, ActionProcessBackendOptions, action_process_runner,
};
pub(crate) use profile::{
    coding_loop_workspace_roots, with_workspace_coding_loop_profile, workspace_tools_config,
};
pub(crate) use roles::RuntimeRoleProviderConfig;
pub(crate) use sandbox::{
    coding_agent_process_admission, coding_agent_requires_sandbox_error,
    coding_loop_smoke_admission_from_current_process,
};

#[cfg(test)]
mod tests;
