//! Tokio-backed process runner adapter.
//!
//! This module is the concrete OS process adapter for Merry's runtime-owned
//! [`crate::ProcessRunner`] boundary. It does not decide whether a process is
//! admitted; callers must still opt in through runtime permission profiles.

use crate::{
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
};
use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Runtime-owned process runner backed by [`tokio::process::Command`].
///
/// The runner executes the exact validated argv supplied by
/// [`ProcessActionIntent`], clears inherited environment, closes stdin, captures
/// stdout/stderr up to the intent limits, and cooperatively cancels by killing
/// the child process. Permission profiles and sandbox constraints are enforced
/// by the runtime construction path that selects this runner, not by this type.
#[derive(Debug, Default, Clone)]
pub struct TokioProcessRunner {
    cwd_root: Option<PathBuf>,
}

impl TokioProcessRunner {
    /// Creates a Tokio-backed process runner.
    #[must_use]
    pub const fn new() -> Self {
        Self { cwd_root: None }
    }

    /// Creates a Tokio-backed process runner whose process cwd values are
    /// resolved under a stable workspace root.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: Some(root.into()),
        }
    }
}

impl ProcessRunner for TokioProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        let cwd_root = self.cwd_root.clone();
        Box::pin(async move { run_tokio_process(intent, context, cwd_root.as_deref()).await })
    }
}

async fn run_tokio_process(
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    cwd_root: Option<&Path>,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    let Some((program, args)) = intent.argv().split_first() else {
        return Err(ProcessRunnerError::infrastructure(
            "validated process argv was unexpectedly empty",
        ));
    };

    if context.cancellation_token().is_cancelled() {
        return Err(ProcessRunnerError::Cancelled);
    }

    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(process_current_dir(cwd_root, &intent))
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            ProcessRunnerError::infrastructure(format!(
                "process executable `{program}` was not found"
            ))
        } else {
            ProcessRunnerError::infrastructure(format!(
                "failed to start process executable `{program}`: {source}"
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
            "failed to wait for process executable `{program}`: {source}"
        ))
    })?;
    let stdout = join_bounded_output(stdout_task, "stdout").await?;
    let stderr = join_bounded_output(stderr_task, "stderr").await?;
    let stdout_text = String::from_utf8(stdout.bytes).map_err(|source| {
        ProcessRunnerError::infrastructure(format!("process stdout was not UTF-8: {source}"))
    })?;
    let stderr_text = String::from_utf8(stderr.bytes).map_err(|source| {
        ProcessRunnerError::infrastructure(format!("process stderr was not UTF-8: {source}"))
    })?;
    let status = status
        .code()
        .map(ProcessExitStatus::Exited)
        .unwrap_or(ProcessExitStatus::DomainFailed);

    ProcessRunnerOutput::new(
        &intent,
        status,
        stdout_text,
        stdout.truncated,
        stderr_text,
        stderr.truncated,
    )
    .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
}

fn process_current_dir(cwd_root: Option<&Path>, intent: &ProcessActionIntent) -> PathBuf {
    let cwd = intent.cwd().unwrap_or(".");
    let Some(root) = cwd_root else {
        return PathBuf::from(cwd);
    };
    if cwd == "." {
        root.to_path_buf()
    } else {
        root.join(cwd)
    }
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

#[cfg(test)]
mod tests {
    use super::process_current_dir;
    use crate::{ProcessActionIntent, ProcessEnvPolicy};
    use std::path::{Path, PathBuf};

    fn intent(cwd: Option<&str>) -> ProcessActionIntent {
        ProcessActionIntent::new(
            vec!["pwd".to_owned()],
            cwd.map(str::to_owned),
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("process intent should be valid")
    }

    #[test]
    fn process_current_dir_uses_workspace_root_for_default_cwd() {
        let root = Path::new("/tmp/merry-workspace");

        assert_eq!(
            process_current_dir(Some(root), &intent(None)),
            PathBuf::from("/tmp/merry-workspace")
        );
        assert_eq!(
            process_current_dir(Some(root), &intent(Some("."))),
            PathBuf::from("/tmp/merry-workspace")
        );
    }

    #[test]
    fn process_current_dir_joins_workspace_relative_cwd_under_root() {
        assert_eq!(
            process_current_dir(
                Some(Path::new("/tmp/merry-workspace")),
                &intent(Some("crates"))
            ),
            PathBuf::from("/tmp/merry-workspace/crates")
        );
    }
}
