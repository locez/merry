use super::{permissions::BwrapProcessRunner, sandbox::BwrapProcessPlan};
use merry_runtime::{
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
};
use std::{io, process::Stdio};
use tokio::io::{AsyncRead, AsyncReadExt};

impl ProcessRunner for BwrapProcessRunner {
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
            let plan = tokio::task::spawn_blocking(move || runner.plan_for(&plan_intent))
                .await
                .map_err(|error| {
                    ProcessRunnerError::infrastructure(format!("sandbox planning failed: {error}"))
                })??;
            run_process_plan(plan, intent, context).await
        })
    }
}
async fn run_process_plan(
    plan: BwrapProcessPlan,
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    if context.cancellation_token().is_cancelled() {
        return Err(ProcessRunnerError::Cancelled);
    }

    let mut command = tokio::process::Command::new(&plan.program);
    #[cfg(target_os = "linux")]
    plan.ssh_config.configure_command(command.as_std_mut())?;
    command
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let backend_program = plan.program.to_string_lossy().into_owned();
    run_spawned_process(command, intent, context, backend_program).await
}

pub(super) async fn run_spawned_process(
    mut command: tokio::process::Command,
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    program_for_error: String,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
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
    let stdout_limit = intent.stdout_limit_bytes();
    let stderr_limit = intent.stderr_limit_bytes();

    let stdout_task = tokio::spawn(async move { read_bounded_output(stdout, stdout_limit).await });
    let stderr_task = tokio::spawn(async move { read_bounded_output(stderr, stderr_limit).await });

    let status = tokio::select! {
        biased;
        () = context.cancellation_token().cancelled() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
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
