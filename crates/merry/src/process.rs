//! Host-process execution backend and workspace sandbox entrypoints.

pub use merry_process::{
    GpgAgentSockets, LocalProcessBackend, ProcessBackend, ProcessBackendError, ProcessBackendMode,
    ProcessBackendOptions, ProcessSession, default_development_path_rules,
    default_development_path_rules_with_env, probe_workspace_sandbox,
};
pub use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, BackgroundProcessId, BackgroundProcessOutput,
    BackgroundProcessState, BackgroundProcessSummary, LocalWorkspaceProcessSandboxProfile,
    PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus,
    ProcessOutputEnvelope, ProcessOutputStream, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
    StaticPermissionedProcessRunnerFactory, shell_command_for_argv,
};

/// Runs the internal sandbox helper and exits if the current process was
/// spawned in helper mode.
///
/// Applications that run isolated local process actions should invoke this at
/// the start of `main` before parsing arguments or starting an async runtime.
pub fn maybe_run_sandbox_helper() {
    if merry_process::sandbox_helper_requested() {
        merry_process::sandbox_helper_main();
    }
}
