use super::background::{self, BackgroundSupervisor};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::permissions::WorkspaceProcessRunner;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::confinement::{self, SandboxLaunch};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use merry_runtime::{BackgroundProcessControl, ProcessRunner, ProcessRunnerFuture};
use merry_runtime::{
    ProcessActionIntent, ProcessRunnerContext, ProcessRunnerError, ProcessRunnerOutput,
};
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{fs, process::Stdio, sync::Arc};

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ProcessRunner for WorkspaceProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ProcessRunnerError::Cancelled);
            }
            let runner = self.clone();
            let plan_intent = intent.clone();
            let launch = tokio::task::spawn_blocking(move || runner.plan_for(&plan_intent))
                .await
                .map_err(|error| {
                    ProcessRunnerError::infrastructure(format!("sandbox planning failed: {error}"))
                })??;
            let background = self.background.clone();
            run_sandbox_launch(launch, intent, context, background).await
        })
    }

    fn background_control(&self) -> Option<Arc<dyn BackgroundProcessControl>> {
        Some(Arc::new(self.background.clone()))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn run_sandbox_launch(
    launch: SandboxLaunch,
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    background: BackgroundSupervisor,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    if context.cancellation_token().is_cancelled() {
        return Err(ProcessRunnerError::Cancelled);
    }
    // On macOS, invoke `/usr/bin/sandbox-exec` directly instead of re-execing
    // the running `merry` binary. Overwriting the running binary on disk
    // (for example `cp -p target/release/merry ~/.cargo/bin/merry`) invalidates
    // its kernel code signature cache on macOS and causes any re-exec of
    // `current_exe()` to die immediately with SIGKILL.
    #[cfg(target_os = "macos")]
    if confinement::explicit_helper_program().is_none() {
        let command = confinement::direct_seatbelt_command(&launch);
        return run_spawned_process(
            command,
            intent,
            context,
            "/usr/bin/sandbox-exec".to_owned(),
            background,
        )
        .await;
    }
    let helper = confinement::helper_program()?;
    let policy = confinement::write_policy(&launch)?;
    let mut command = tokio::process::Command::new(&helper);
    command
        .env(confinement::helper_env_name(), "1")
        .env(confinement::policy_env_name(), &policy)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let backend_program = helper.to_string_lossy().into_owned();
    let result = run_spawned_process(command, intent, context, backend_program, background).await;
    let _ = fs::remove_file(policy);
    result
}

pub(super) async fn run_spawned_process(
    mut command: tokio::process::Command,
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    program_for_error: String,
    background: BackgroundSupervisor,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    // A new group makes session-end and cancellation kill the command and its
    // children without signaling Merry's own group.
    #[cfg(unix)]
    command.process_group(0);
    // On Windows, detach from the parent console without `DETACHED_PROCESS`
    // (which breaks stdio pipe inheritance for grandchildren like cmd -> node).
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = command.spawn().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProcessRunnerError::infrastructure(format!(
                "process executable `{program_for_error}` was not found"
            ))
        } else {
            ProcessRunnerError::infrastructure(format!(
                "failed to start process executable `{program_for_error}`: {source}"
            ))
        }
    })?;
    #[cfg(windows)]
    let windows_job = child.id().and_then(
        |pid| match super::windows_job::WindowsProcessJob::for_pid(pid) {
            Ok(job) => Some(job),
            Err(error) => {
                tracing::debug!(
                    pid,
                    error = %error,
                    "failed to attach Windows child process to job object"
                );
                None
            }
        },
    );
    let stdout = child.stdout.take().ok_or_else(|| {
        ProcessRunnerError::infrastructure("process stdout pipe was not available")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ProcessRunnerError::infrastructure("process stderr pipe was not available")
    })?;
    background::finish_process(
        background::SpawnedProcess {
            child,
            #[cfg(windows)]
            windows_job,
            stdout,
            stderr,
            program_for_error,
        },
        intent,
        context,
        background,
    )
    .await
}
