//! Session-scoped ownership for processes that outlive one tool call.
//!
//! `runner_for` builds a fresh runner and drops it when the call returns.
//! A child stored only on that runner would die with the call. The factory
//! and each long-lived runner therefore share this supervisor and drop it
//! with the session.

use merry_runtime::{
    BackgroundProcessControl, BackgroundProcessId, BackgroundProcessLookupError,
    BackgroundProcessOutput, BackgroundProcessState, BackgroundProcessStop,
    BackgroundProcessSummary, ProcessActionIntent, ProcessExitStatus, ProcessOutputSink,
    ProcessRunnerContext, ProcessRunnerError, ProcessRunnerOutput, ToolOutputStream,
};
use std::{
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Child,
    task::JoinHandle,
};

/// How long a background command may run before the tool returns.
///
/// A process that exits inside this window is reported as a normal exit, so a
/// startup failure stays visible. A process that is still alive is retained.
const BACKGROUND_SETTLE: Duration = Duration::from_secs(1);

/// How long to collect output after a background command exits during settle.
///
/// A shell that backgrounds another process can leave the pipe open. Waiting
/// for EOF would block the turn again.
const BACKGROUND_EXIT_OUTPUT_WAIT: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub(crate) struct BackgroundSupervisor {
    inner: Arc<SupervisorInner>,
}

#[derive(Debug)]
struct SupervisorInner {
    next_id: AtomicU64,
    processes: Mutex<Vec<RetainedProcess>>,
}

struct RetainedProcess {
    id: BackgroundProcessId,
    display_name: Option<String>,
    command: String,
    cwd: Option<String>,
    pid: u32,
    life: Life,
    child: Child,
    #[cfg(windows)]
    windows_job: Option<super::windows_job::WindowsProcessJob>,
    readers: Vec<JoinHandle<Result<(), ProcessRunnerError>>>,
    stdout: Arc<Capture>,
    stderr: Arc<Capture>,
}

enum Life {
    Running,
    Exited(i32),
    Killed,
}

impl std::fmt::Debug for RetainedProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RetainedProcess")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl BackgroundSupervisor {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                next_id: AtomicU64::new(0),
                processes: Mutex::new(Vec::new()),
            }),
        }
    }

    fn mint_id(&self, explicit_background: bool) -> BackgroundProcessId {
        let number = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let prefix = if explicit_background { "bg" } else { "proc" };
        BackgroundProcessId::new(format!("{prefix}-{number}"))
            .expect("minted retained process id is valid")
    }

    fn retain(&self, process: RetainedProcess) {
        let mut processes = self
            .inner
            .processes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        processes.push(process);
    }
}

impl Default for BackgroundSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SupervisorInner {
    fn drop(&mut self) {
        let processes = std::mem::take(
            self.processes
                .get_mut()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for mut process in processes {
            stop_retained(&mut process);
        }
    }
}

/// Owned child process, stdio pipes, and display label passed into [`finish_process`].
pub(super) struct SpawnedProcess<Stdout, Stderr> {
    pub(super) child: Child,
    #[cfg(windows)]
    pub(super) windows_job: Option<super::windows_job::WindowsProcessJob>,
    pub(super) stdout: Stdout,
    pub(super) stderr: Stderr,
    pub(super) program_for_error: String,
}

/// Waits for a spawned process up to its background settle window or foreground
/// wait timeout, retaining it in `BackgroundSupervisor` without killing it if it
/// is still alive when the wait window elapses.
pub(super) async fn finish_process<Stdout, Stderr>(
    spawned: SpawnedProcess<Stdout, Stderr>,
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    supervisor: BackgroundSupervisor,
) -> Result<ProcessRunnerOutput, ProcessRunnerError>
where
    Stdout: AsyncRead + Unpin + Send + 'static,
    Stderr: AsyncRead + Unpin + Send + 'static,
{
    let SpawnedProcess {
        mut child,
        #[cfg(windows)]
        windows_job,
        stdout,
        stderr,
        program_for_error,
    } = spawned;
    let explicit_background = intent.background();
    let wait_timeout_secs = (!explicit_background).then(|| intent.effective_wait_timeout_secs());
    let wait_window = match wait_timeout_secs {
        Some(secs) => Duration::from_secs(secs),
        None => BACKGROUND_SETTLE,
    };

    let stdout_capture = Arc::new(Capture::default());
    let stderr_capture = Arc::new(Capture::default());
    let live_sink = if explicit_background {
        None
    } else {
        context
            .output_sink()
            .cloned()
            .map(|sink| Arc::new(Mutex::new(Some(sink))))
    };
    let mut stdout_task = tokio::spawn(drain_capture(
        stdout,
        intent.stdout_limit_bytes(),
        Arc::clone(&stdout_capture),
        live_sink.clone(),
        ToolOutputStream::Stdout,
    ));
    let mut stderr_task = tokio::spawn(drain_capture(
        stderr,
        intent.stderr_limit_bytes(),
        Arc::clone(&stderr_capture),
        live_sink.clone(),
        ToolOutputStream::Stderr,
    ));

    let settled = tokio::select! {
        biased;
        () = context.cancellation_token().cancelled() => Settle::Cancelled,
        status = child.wait() => Settle::Exited(status),
        () = tokio::time::sleep(wait_window) => Settle::StillRunning,
    };

    match settled {
        Settle::Cancelled => {
            detach_live_sink(&live_sink);
            stdout_task.abort();
            stderr_task.abort();
            #[cfg(windows)]
            if let Some(job) = windows_job.as_ref() {
                job.terminate();
            }
            kill_child_group(&mut child);
            let _ = child.wait().await;
            Err(ProcessRunnerError::Cancelled)
        }
        Settle::Exited(status) => {
            let status = status.map_err(|source| {
                ProcessRunnerError::infrastructure(format!(
                    "failed to wait for process executable `{program_for_error}`: {source}"
                ))
            })?;
            if explicit_background {
                collect_exit_output(&mut stdout_task, &mut stderr_task).await?;
            } else {
                join_exit_output(&mut stdout_task, &mut stderr_task).await?;
            }
            detach_live_sink(&live_sink);
            finish_exited(&intent, status, &stdout_capture, &stderr_capture)
        }
        Settle::StillRunning => {
            detach_live_sink(&live_sink);
            if context.cancellation_token().is_cancelled() {
                stdout_task.abort();
                stderr_task.abort();
                #[cfg(windows)]
                if let Some(job) = windows_job.as_ref() {
                    job.terminate();
                }
                kill_child_group(&mut child);
                let _ = child.wait().await;
                return Err(ProcessRunnerError::Cancelled);
            }
            let (stdout_bytes, stdout_truncated) = stdout_capture.snapshot();
            let (stderr_bytes, stderr_truncated) = stderr_capture.snapshot();
            if let Some(message) = crate::confinement::setup_failure(&stderr_bytes) {
                stdout_task.abort();
                stderr_task.abort();
                #[cfg(windows)]
                if let Some(job) = windows_job.as_ref() {
                    job.terminate();
                }
                kill_child_group(&mut child);
                let _ = child.wait().await;
                return Err(ProcessRunnerError::infrastructure(message));
            }
            let pid = child.id();
            let command = display_command(&intent);
            let cwd = intent.cwd().map(str::to_owned);
            let mut output = ProcessRunnerOutput::from_bytes(
                &intent,
                ProcessExitStatus::Running,
                stdout_bytes,
                stdout_truncated,
                stderr_bytes,
                stderr_truncated,
            )
            .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))?;
            let Some(pid) = pid else {
                stdout_task.abort();
                stderr_task.abort();
                #[cfg(windows)]
                if let Some(job) = windows_job.as_ref() {
                    job.terminate();
                }
                kill_child_group(&mut child);
                let _ = child.wait().await;
                return Err(ProcessRunnerError::infrastructure(
                    "background process started without a pid",
                ));
            };
            let id = supervisor.mint_id(explicit_background);
            let display_name = intent
                .display_name()
                .map(str::to_owned)
                .or_else(|| fallback_display_name(&command));
            output = output
                .with_running_pid(pid)
                .with_background_process_id(id.clone());
            if let Some(secs) = wait_timeout_secs {
                output = output.with_wait_timed_out_secs(secs);
            }
            supervisor.retain(RetainedProcess {
                id,
                display_name,
                command,
                cwd,
                pid,
                life: Life::Running,
                child,
                #[cfg(windows)]
                windows_job,
                readers: vec![stdout_task, stderr_task],
                stdout: stdout_capture,
                stderr: stderr_capture,
            });
            tracing::debug!(
                pid,
                wait_timeout_secs,
                "retained still-running process in background supervisor"
            );
            Ok(output)
        }
    }
}

fn detach_live_sink(slot: &Option<Arc<Mutex<Option<ProcessOutputSink>>>>) {
    if let Some(slot) = slot {
        let mut guard = slot.lock().unwrap_or_else(|error| error.into_inner());
        *guard = None;
    }
}

async fn join_exit_output(
    stdout_task: &mut JoinHandle<Result<(), ProcessRunnerError>>,
    stderr_task: &mut JoinHandle<Result<(), ProcessRunnerError>>,
) -> Result<(), ProcessRunnerError> {
    let stdout_result = (&mut *stdout_task).await.map_err(|source| {
        ProcessRunnerError::infrastructure(format!("process stdout reader task failed: {source}"))
    });
    if let Err(error) = stdout_result {
        stderr_task.abort();
        return Err(error);
    }
    if let Err(error) = stdout_result.expect("checked above") {
        stderr_task.abort();
        return Err(error);
    }
    (&mut *stderr_task).await.map_err(|source| {
        ProcessRunnerError::infrastructure(format!("process stderr reader task failed: {source}"))
    })??;
    Ok(())
}

enum Settle {
    Cancelled,
    Exited(std::io::Result<std::process::ExitStatus>),
    StillRunning,
}

async fn collect_exit_output(
    stdout_task: &mut JoinHandle<Result<(), ProcessRunnerError>>,
    stderr_task: &mut JoinHandle<Result<(), ProcessRunnerError>>,
) -> Result<(), ProcessRunnerError> {
    let readers = async {
        (&mut *stdout_task).await.map_err(|source| {
            ProcessRunnerError::infrastructure(format!(
                "process stdout reader task failed: {source}"
            ))
        })??;
        (&mut *stderr_task).await.map_err(|source| {
            ProcessRunnerError::infrastructure(format!(
                "process stderr reader task failed: {source}"
            ))
        })??;
        Ok(())
    };
    let result = tokio::select! {
        biased;
        result = readers => result,
        () = tokio::time::sleep(BACKGROUND_EXIT_OUTPUT_WAIT) => {
            stdout_task.abort();
            stderr_task.abort();
            Ok(())
        }
    };
    if result.is_err() {
        stdout_task.abort();
        stderr_task.abort();
    }
    result
}

fn finish_exited(
    intent: &ProcessActionIntent,
    status: std::process::ExitStatus,
    stdout_capture: &Capture,
    stderr_capture: &Capture,
) -> Result<ProcessRunnerOutput, ProcessRunnerError> {
    let (stdout, stdout_truncated) = stdout_capture.snapshot();
    let (stderr, stderr_truncated) = stderr_capture.snapshot();
    if let Some(message) = crate::confinement::setup_failure(&stderr) {
        return Err(ProcessRunnerError::infrastructure(message));
    }
    let status = status
        .code()
        .map(ProcessExitStatus::Exited)
        .unwrap_or(ProcessExitStatus::DomainFailed);
    ProcessRunnerOutput::from_bytes(
        intent,
        status,
        stdout,
        stdout_truncated,
        stderr,
        stderr_truncated,
    )
    .map_err(|source| ProcessRunnerError::infrastructure(source.to_string()))
}

impl BackgroundProcessControl for BackgroundSupervisor {
    fn list(&self) -> Vec<BackgroundProcessSummary> {
        let mut processes = lock_processes(self);
        processes.iter_mut().map(summary_of).collect()
    }

    fn read(
        &self,
        id: &BackgroundProcessId,
    ) -> Result<BackgroundProcessOutput, BackgroundProcessLookupError> {
        let mut processes = lock_processes(self);
        let Some(process) = processes.iter_mut().find(|process| &process.id == id) else {
            return Err(missing(id, &processes));
        };
        let (stdout, stdout_truncated) = process.stdout.snapshot();
        let (stderr, stderr_truncated) = process.stderr.snapshot();
        Ok(BackgroundProcessOutput::new(
            summary_of(process),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
            stdout_truncated,
            stderr_truncated,
        ))
    }

    fn stop(
        &self,
        id: &BackgroundProcessId,
    ) -> Result<BackgroundProcessStop, BackgroundProcessLookupError> {
        let mut processes = lock_processes(self);
        let Some(process) = processes.iter_mut().find(|process| &process.id == id) else {
            return Err(missing(id, &processes));
        };
        refresh(process);
        if !matches!(process.life, Life::Running) {
            return Ok(BackgroundProcessStop::new(summary_of(process), false));
        }
        process.life = Life::Killed;
        kill_tree(Some(process.pid));
        let _ = process.child.start_kill();
        reap_child(&mut process.child);
        for reader in &process.readers {
            reader.abort();
        }
        Ok(BackgroundProcessStop::new(summary_of(process), true))
    }
}

fn lock_processes(supervisor: &BackgroundSupervisor) -> MutexGuard<'_, Vec<RetainedProcess>> {
    supervisor
        .inner
        .processes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn missing(
    id: &BackgroundProcessId,
    processes: &[RetainedProcess],
) -> BackgroundProcessLookupError {
    BackgroundProcessLookupError::new(
        id.clone(),
        processes.iter().map(|process| process.id.clone()).collect(),
    )
}

fn summary_of(process: &mut RetainedProcess) -> BackgroundProcessSummary {
    refresh(process);
    BackgroundProcessSummary::new(
        process.id.clone(),
        process.command.clone(),
        process.cwd.clone(),
        match process.life {
            Life::Running => BackgroundProcessState::Running { pid: process.pid },
            Life::Exited(code) => BackgroundProcessState::Exited { code },
            Life::Killed => BackgroundProcessState::Killed,
        },
    )
    .with_display_name(process.display_name.clone())
}

fn refresh(process: &mut RetainedProcess) {
    if !matches!(process.life, Life::Running) {
        return;
    }
    match process.child.try_wait() {
        Ok(Some(status)) => {
            process.life = Life::Exited(status.code().unwrap_or(-1));
        }
        Ok(None) => {}
        Err(_) => {
            process.life = Life::Exited(-1);
        }
    }
}

const DISPLAY_COMMAND_LIMIT: usize = 240;
const FALLBACK_DISPLAY_NAME_LIMIT: usize = 24;

fn fallback_display_name(command: &str) -> Option<String> {
    let first_segment = command
        .split([';', '\n', '|'])
        .next()
        .unwrap_or(command)
        .split("&&")
        .next()
        .unwrap_or(command)
        .trim();
    let tokens: Vec<&str> = first_segment.split_whitespace().take(2).collect();
    if tokens.is_empty() {
        return None;
    }
    let candidate = tokens.join(" ");
    if candidate.chars().count() <= FALLBACK_DISPLAY_NAME_LIMIT {
        Some(candidate)
    } else {
        let mut truncated: String = candidate
            .chars()
            .take(FALLBACK_DISPLAY_NAME_LIMIT.saturating_sub(1))
            .collect();
        truncated.push('…');
        Some(truncated)
    }
}

fn display_command(intent: &ProcessActionIntent) -> String {
    let raw = unwrap_shell_display_command(intent.argv());
    if raw.chars().count() <= DISPLAY_COMMAND_LIMIT {
        raw
    } else {
        let mut truncated = raw.chars().take(DISPLAY_COMMAND_LIMIT).collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

fn unwrap_shell_display_command(argv: &[String]) -> String {
    match argv {
        [_, no_profile, non_interactive, flag, script]
            if no_profile.eq_ignore_ascii_case("-NoProfile")
                && non_interactive.eq_ignore_ascii_case("-NonInteractive")
                && flag.eq_ignore_ascii_case("-Command") =>
        {
            script.clone()
        }
        [_, no_profile, flag, script]
            if no_profile.eq_ignore_ascii_case("-NoProfile")
                && flag.eq_ignore_ascii_case("-Command") =>
        {
            script.clone()
        }
        [_, flag, script]
            if matches!(flag.as_str(), "-lc" | "-c") || flag.eq_ignore_ascii_case("/C") =>
        {
            script.clone()
        }
        _ => argv.join(" "),
    }
}

fn stop_retained(process: &mut RetainedProcess) {
    for reader in &process.readers {
        reader.abort();
    }
    #[cfg(windows)]
    if let Some(job) = process.windows_job.as_ref() {
        job.terminate();
    }
    reap_child(&mut process.child);
}

pub(super) fn kill_child_group(child: &mut Child) {
    kill_tree(child.id());
    let _ = child.start_kill();
}

/// Reaps a killed child so its pid does not remain a zombie.
///
/// Drop cannot await. `SIGKILL` is bounded by a short poll of `try_wait`.
fn reap_child(child: &mut Child) {
    kill_child_group(child);
    for _ in 0..20 {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn kill_tree(pid: Option<u32>) {
    #[cfg(unix)]
    {
        let Some(pid) = pid else {
            return;
        };
        let Some(pid) = rustix::process::Pid::from_raw(pid as rustix::process::RawPid) else {
            return;
        };
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

#[derive(Debug, Default)]
struct Capture {
    bytes: Mutex<Vec<u8>>,
    truncated: AtomicBool,
}

impl Capture {
    fn snapshot(&self) -> (Vec<u8>, bool) {
        let bytes = self
            .bytes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        (bytes, self.truncated.load(Ordering::Relaxed))
    }
}

async fn drain_capture<R>(
    mut reader: R,
    limit: usize,
    capture: Arc<Capture>,
    live_sink: Option<Arc<Mutex<Option<ProcessOutputSink>>>>,
    stream: ToolOutputStream,
) -> Result<(), ProcessRunnerError>
where
    R: AsyncRead + Unpin,
{
    let mut scratch = [0_u8; 8192];
    loop {
        let count = reader.read(&mut scratch).await.map_err(|source| {
            ProcessRunnerError::infrastructure(format!("failed to read process output: {source}"))
        })?;
        if count == 0 {
            break;
        }
        if let Some(slot) = &live_sink {
            let guard = slot.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(sink) = guard.as_ref() {
                sink.emit(stream, &scratch[..count]);
            }
        }
        let mut bytes = capture
            .bytes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if bytes.len() < limit {
            let remaining = limit - bytes.len();
            let kept = count.min(remaining);
            bytes.extend_from_slice(&scratch[..kept]);
            if kept < count {
                capture.truncated.store(true, Ordering::Relaxed);
            }
        } else {
            capture.truncated.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[cfg(test)]
mod display_tests {
    use super::unwrap_shell_display_command;

    #[test]
    fn unwrap_shell_display_command_handles_unix_and_windows_wrappers() {
        assert_eq!(
            unwrap_shell_display_command(&[
                "bash".to_owned(),
                "-lc".to_owned(),
                "cargo test".to_owned(),
            ]),
            "cargo test"
        );
        assert_eq!(
            unwrap_shell_display_command(&[
                "pwsh".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                "cargo test".to_owned(),
            ]),
            "cargo test"
        );
        assert_eq!(
            unwrap_shell_display_command(&[
                "powershell.exe".to_owned(),
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                "Get-ChildItem".to_owned(),
            ]),
            "Get-ChildItem"
        );
        assert_eq!(
            unwrap_shell_display_command(&[
                "cmd.exe".to_owned(),
                "/C".to_owned(),
                "dir".to_owned(),
            ]),
            "dir"
        );
    }
}
