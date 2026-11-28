use crate::coding::SandboxMode;
use merry::process::AcceptedLocalWorkspaceProcessAdmission;

/// Admits the process profile for the selected sandbox mode.
///
/// Workspace actions use the isolated backend. Off uses the host backend.
/// Neither mode re-execs Merry into an outer sandbox.
pub(crate) fn coding_agent_process_admission(
    mode: SandboxMode,
) -> AcceptedLocalWorkspaceProcessAdmission {
    match mode {
        SandboxMode::Off => AcceptedLocalWorkspaceProcessAdmission::accept_host(),
        SandboxMode::Workspace => AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace(),
    }
}
