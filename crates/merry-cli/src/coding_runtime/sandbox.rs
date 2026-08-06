use crate::cli_error::CliError;
use crate::coding_runtime::ProcessExecutionMode;
use crate::debug;
use crate::sandbox::{
    ChildHandoff as SandboxChildHandoff, MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION_ENV,
    RuntimeProfile as SandboxRuntimeProfile, read_proc_self_mountinfo,
    runtime_profile_from_evidence as sandbox_runtime_profile_from_evidence,
};
use merry_runtime::AcceptedLocalWorkspaceProcessAdmission;
use std::{env, ffi::OsStr};

pub(crate) async fn coding_loop_smoke_admission_from_current_process(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
    let sandbox_marker = env::var_os(MERRY_SANDBOX_ENV);
    let sandbox_version = env::var_os(MERRY_SANDBOX_VERSION_ENV);
    let home = env::var_os("HOME");
    let tmpdir = env::var_os("TMPDIR");
    let mountinfo = read_proc_self_mountinfo().await;
    let sandbox_runtime_profile = sandbox_runtime_profile_from_evidence(
        home.as_deref(),
        tmpdir.as_deref(),
        mountinfo.as_deref(),
    );
    coding_loop_smoke_admission(
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox_marker.as_deref(),
        sandbox_version.as_deref(),
    )
}

pub(crate) async fn coding_agent_process_admission(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    mode: ProcessExecutionMode,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
    if matches!(mode, ProcessExecutionMode::Unrestricted) {
        return Some(AcceptedLocalWorkspaceProcessAdmission::accept_host_v1());
    }
    if matches!(mode, ProcessExecutionMode::InnerOnly) {
        return Some(AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1());
    }
    coding_loop_smoke_admission_from_current_process(sandbox_child_handoff).await
}

pub(crate) fn coding_agent_requires_sandbox_error(command: &str) -> CliError {
    CliError::DebugUsage(format!(
        "merry {command} requires the automatic bubblewrap sandbox"
    ))
}

fn coding_loop_smoke_admission(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    sandbox_runtime_profile: Option<SandboxRuntimeProfile>,
    sandbox: Option<&OsStr>,
    version: Option<&OsStr>,
) -> Option<AcceptedLocalWorkspaceProcessAdmission> {
    debug::shell::runtime_admission(
        true,
        sandbox_child_handoff,
        sandbox_runtime_profile,
        sandbox,
        version,
    )
}
