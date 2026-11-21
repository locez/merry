//! Provider-neutral process action contracts and policy classification.
//!
//! This facade exposes typed process intent, runner contracts, execution
//! evidence, and the narrow runtime policy classifier. It does not execute
//! commands or spawn subprocesses; host execution belongs to `merry-process`.

mod background;
mod classification;
mod contracts;
mod output;
mod session_permissions;
mod validation;

pub use background::{
    BackgroundProcessControl, BackgroundProcessId, BackgroundProcessLookupError,
    BackgroundProcessOutput, BackgroundProcessState, BackgroundProcessStop,
    BackgroundProcessSummary,
};
pub use classification::{
    ProcessIntentClass, classify_process_intent, is_low_risk_process_action_intent,
    is_read_only_shell_process_action_intent, shell_command_for_argv,
};
pub use contracts::{
    AcceptedLocalWorkspaceProcessAdmission, DEFAULT_PROCESS_WAIT_TIMEOUT_SECS,
    LocalWorkspaceProcessSandboxProfile, MAX_PROCESS_ARG_BYTES, MAX_PROCESS_ARGV_ITEMS,
    MAX_PROCESS_CWD_BYTES, MAX_PROCESS_OUTPUT_LIMIT_BYTES, MAX_PROCESS_STDIN_TEXT_BYTES,
    MAX_PROCESS_WAIT_TIMEOUT_SECS, PermissionedProcessRunnerFactory, ProcessActionIntent,
    ProcessEnvPolicy, ProcessExecutionEvidence, ProcessExitStatus, ProcessOutputChunk,
    ProcessOutputSink, ProcessPermissionProfileId, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput, ProcessRunnerResult,
    StaticPermissionedProcessRunnerFactory,
};
#[cfg(test)]
pub(crate) use output::{NON_UTF8_GUIDANCE, STILL_RUNNING_GUIDANCE};
pub(crate) use output::{
    OUTPUT_RECOVERY_GUIDANCE, TRUNCATION_GUIDANCE, derived_process_model_text,
    recovery_guidance_for_output, render_process_model_text, sanitize_process_output_for_journal,
};
pub use output::{ProcessOutputEnvelope, ProcessOutputStream};
pub use session_permissions::{
    PreparedProcessPermission, ProcessPathGrant, ProcessPathGrantConstraint,
    ProcessSessionPermissionSnapshot, ProcessSessionPermissionView, ProcessSessionPermissions,
    SessionPermissionedProcessRunnerFactory,
};
pub use validation::ProcessActionError;

pub(crate) use classification::{
    required_process_permission_profile_id, requires_host_process_path_review,
    requires_process_action_review, shell_command_argv, shell_process_input,
    stable_process_input_fingerprint,
};

#[cfg(test)]
pub(crate) use classification::is_safe_cargo_package_token;

#[cfg(test)]
mod tests;
