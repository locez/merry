use super::{
    background::{self, BackgroundSupervisor},
    permissions::WorkspaceProcessRunner,
};
use crate::confinement::{self, SandboxLaunch};
use merry_runtime::{
    BackgroundProcessControl, ProcessActionIntent, ProcessExitStatus, ProcessRunner,
    ProcessRunnerContext, ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
};
use std::{fs, io, process::Stdio, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt};

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

async fn run_sandbox_launch(
    launch: SandboxLaunch,
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    background: BackgroundSupervisor,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    if context.cancellation_token().is_cancelled() {
        return Err(ProcessRunnerError::Cancelled);
    }
    let policy = confinement::write_policy(&launch)?;
    let helper = confinement::helper_program();
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
    let stdout = child.stdout.take().ok_or_else(|| {
        ProcessRunnerError::infrastructure("process stdout pipe was not available")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ProcessRunnerError::infrastructure("process stderr pipe was not available")
    })?;
    if intent.background() {
        return background::finish_background(
            child,
            stdout,
            stderr,
            intent,
            context,
            background,
            program_for_error,
        )
        .await;
    }
    let stdout_limit = intent.stdout_limit_bytes();
    let stderr_limit = intent.stderr_limit_bytes();
    let stdout_sink = context.output_sink().cloned();
    let stderr_sink = context.output_sink().cloned();

    let stdout_task = tokio::spawn(async move {
        read_bounded_output(
            stdout,
            stdout_limit,
            stdout_sink,
            merry_runtime::ToolOutputStream::Stdout,
        )
        .await
    });
    let stderr_task = tokio::spawn(async move {
        read_bounded_output(
            stderr,
            stderr_limit,
            stderr_sink,
            merry_runtime::ToolOutputStream::Stderr,
        )
        .await
    });

    let status = tokio::select! {
        biased;
        () = context.cancellation_token().cancelled() => {
            stdout_task.abort();
            stderr_task.abort();
            background::kill_child_group(&mut child);
            let _ = child.wait().await;
            return Err(ProcessRunnerError::Cancelled);
        }
        status = child.wait() => status,
    };
    let status = status.map_err(|source| {
        ProcessRunnerError::infrastructure(format!(
            "failed to wait for process executable `{program_for_error}`: {source}"
        ))
    })?;
    let stdout = join_bounded_output(stdout_task, "stdout").await?;
    let stderr = join_bounded_output(stderr_task, "stderr").await?;
    let status = status
        .code()
        .map(ProcessExitStatus::Exited)
        .unwrap_or(ProcessExitStatus::DomainFailed);

    if let Some(message) = confinement::setup_failure(&stderr.bytes) {
        return Err(ProcessRunnerError::infrastructure(message));
    }

    ProcessRunnerOutput::from_bytes(
        &intent,
        status,
        stdout.bytes,
        stdout.truncated,
        stderr.bytes,
        stderr.truncated,
    )
    .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
}

async fn join_bounded_output(
    task: tokio::task::JoinHandle<Result<BoundedOutput, ProcessRunnerError>>,
    stream_name: &'static str,
) -> Result<BoundedOutput, ProcessRunnerError> {
    task.await.map_err(|source| {
        ProcessRunnerError::infrastructure(format!(
            "process {stream_name} reader task failed: {source}"
        ))
    })?
}

struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded_output<R>(
    mut reader: R,
    limit: usize,
    output_sink: Option<merry_runtime::ProcessOutputSink>,
    stream: merry_runtime::ToolOutputStream,
) -> Result<BoundedOutput, ProcessRunnerError>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut scratch = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let count = reader.read(&mut scratch).await.map_err(|source| {
            ProcessRunnerError::infrastructure(format!("failed to read process output: {source}"))
        })?;
        if count == 0 {
            break;
        }
        if let Some(sink) = &output_sink {
            sink.emit(stream, &scratch[..count]);
        }

        if bytes.len() < limit {
            let remaining = limit - bytes.len();
            let kept = count.min(remaining);
            bytes.extend_from_slice(&scratch[..kept]);
            truncated |= kept < count;
        } else {
            truncated = true;
        }
    }

    Ok(BoundedOutput { bytes, truncated })
}
