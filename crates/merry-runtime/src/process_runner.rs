//! Tokio-backed process runner adapter.
//!
//! This module is the concrete OS process adapter for Merry's runtime-owned
//! [`crate::ProcessRunner`] boundary. It does not decide whether a process is
//! admitted; callers must still opt in through runtime permission profiles.

use crate::{
    PathAccess, PathAccessRule, PermissionRequest, PermissionedProcessRunnerFactory,
    ProcessActionIntent, ProcessExitStatus, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture, ProcessRunnerOutput,
};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::io::{AsyncRead, AsyncReadExt};

const BWRAP_PROGRAM: &str = "bwrap";
const ACTION_SANDBOX_HOME: &str = "/home/merry";
const ACTION_SANDBOX_TMPDIR: &str = "/tmp";
const ACTION_SANDBOX_ETC_READ_ONLY_FILE_PATHS: &[&str] = &[
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
];
const ACTION_SANDBOX_ETC_READ_ONLY_DIR_PATHS: &[&str] = &[
    "/etc/ld.so.conf.d",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/etc/pki",
];

/// Runtime-owned process runner backed by [`tokio::process::Command`].
///
/// The runner executes the exact validated argv supplied by
/// [`ProcessActionIntent`], inherits the current process environment, closes
/// stdin, captures stdout/stderr up to the intent limits, and cooperatively
/// cancels by killing the child process. Permission profiles and sandbox
/// constraints are enforced by the runtime construction path that selects this
/// runner, not by this type.
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

/// Runtime-owned process runner that executes each process inside bubblewrap.
///
/// This is Merry's per-action sandbox backend for Linux. It is intentionally
/// separate from the CLI outer sandbox: the outer sandbox protects the host
/// from the Merry process, while this runner protects each process action from
/// the runtime profile.
#[derive(Debug, Clone)]
pub struct BwrapProcessRunner {
    cwd_root: PathBuf,
    network_allowed: bool,
    path_rules: Vec<PathAccessRule>,
    bwrap_program: PathBuf,
}

impl BwrapProcessRunner {
    /// Creates a per-action bubblewrap runner rooted at a workspace path.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: root.into(),
            network_allowed: false,
            path_rules: Vec::new(),
            bwrap_program: PathBuf::from(BWRAP_PROGRAM),
        }
    }

    /// Allows network access for child process actions.
    #[must_use]
    pub fn allow_network(mut self) -> Self {
        self.network_allowed = true;
        self
    }

    /// Installs trusted path rules for child process actions.
    #[must_use]
    pub fn with_path_rules(mut self, rules: impl IntoIterator<Item = PathAccessRule>) -> Self {
        self.path_rules = rules.into_iter().collect();
        self
    }

    #[cfg(test)]
    fn with_bwrap_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.bwrap_program = program.into();
        self
    }
}

/// Builds a per-action bubblewrap runner from an approved permission request.
///
/// Filesystem access stays governed by the configured workspace/root path
/// rules. The first materialized permission capability is network: approved
/// `network=true` requests run without `--unshare-net`; other requests remain
/// network-isolated.
#[derive(Debug, Clone)]
pub struct BwrapPermissionedProcessRunnerFactory {
    cwd_root: PathBuf,
    base_network_allowed: bool,
    path_rules: Vec<PathAccessRule>,
    bwrap_program: PathBuf,
}

impl BwrapPermissionedProcessRunnerFactory {
    /// Creates a bubblewrap permissioned runner factory rooted at a workspace path.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: root.into(),
            base_network_allowed: false,
            path_rules: Vec::new(),
            bwrap_program: PathBuf::from(BWRAP_PROGRAM),
        }
    }

    /// Allows network capability in the base profile before per-request grants.
    #[must_use]
    pub fn allow_base_network(mut self) -> Self {
        self.base_network_allowed = true;
        self
    }

    /// Installs trusted path rules shared by each materialized runner.
    #[must_use]
    pub fn with_path_rules(mut self, rules: impl IntoIterator<Item = PathAccessRule>) -> Self {
        self.path_rules = rules.into_iter().collect();
        self
    }

    #[cfg(test)]
    fn with_bwrap_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.bwrap_program = program.into();
        self
    }

    fn build_runner(&self, request: &PermissionRequest) -> BwrapProcessRunner {
        let mut runner = BwrapProcessRunner::new_at_workspace_root(self.cwd_root.clone())
            .with_path_rules(self.path_rules.clone());
        if self.base_network_allowed || request.requests_network() {
            runner = runner.allow_network();
        }
        runner.bwrap_program = self.bwrap_program.clone();
        runner
    }
}

impl PermissionedProcessRunnerFactory for BwrapPermissionedProcessRunnerFactory {
    fn runner_for(&self, request: &PermissionRequest) -> Arc<dyn ProcessRunner> {
        Arc::new(self.build_runner(request))
    }
}

impl ProcessRunner for BwrapProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        let plan = bwrap_process_plan(
            &intent,
            &self.cwd_root,
            self.network_allowed,
            &self.path_rules,
            &self.bwrap_program,
        );
        Box::pin(async move { run_process_plan(plan, intent, context).await })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BwrapProcessPlan {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
}

fn bwrap_process_plan(
    intent: &ProcessActionIntent,
    cwd_root: &Path,
    network_allowed: bool,
    path_rules: &[PathAccessRule],
    bwrap_program: &Path,
) -> BwrapProcessPlan {
    let cwd = process_current_dir(Some(cwd_root), intent);
    let mut args = vec![
        os("--unshare-user"),
        os("--unshare-ipc"),
        os("--unshare-pid"),
        os("--unshare-uts"),
        os("--unshare-cgroup-try"),
        os("--die-with-parent"),
        os("--new-session"),
        os("--proc"),
        os("/proc"),
        os("--dev"),
        os("/dev"),
        os("--perms"),
        os("01777"),
        os("--tmpfs"),
        os(ACTION_SANDBOX_TMPDIR),
        os("--tmpfs"),
        os("/home"),
        os("--perms"),
        os("0700"),
        os("--dir"),
        os(ACTION_SANDBOX_HOME),
        os("--ro-bind"),
        os("/usr"),
        os("/usr"),
        os("--ro-bind-try"),
        os("/bin"),
        os("/bin"),
        os("--ro-bind-try"),
        os("/lib"),
        os("/lib"),
        os("--ro-bind-try"),
        os("/lib64"),
        os("/lib64"),
        os("--ro-bind-try"),
        os("/opt"),
        os("/opt"),
    ];
    for path in ACTION_SANDBOX_ETC_READ_ONLY_FILE_PATHS {
        if Path::new(path).exists() {
            append_bwrap_file_bind_args(&mut args, Path::new(path), Path::new(path));
        }
    }
    for path in ACTION_SANDBOX_ETC_READ_ONLY_DIR_PATHS {
        if Path::new(path).exists() {
            append_bwrap_dir_bind_args(&mut args, Path::new(path), Path::new(path));
        }
    }
    if !network_allowed {
        args.push(os("--unshare-net"));
    }
    append_bwrap_path_rule(&mut args, cwd_root, PathAccess::ReadWrite);
    for rule in path_rules {
        append_bwrap_path_rule(&mut args, rule.path(), rule.access());
    }
    args.extend([
        os("--chdir"),
        cwd.as_os_str().to_owned(),
        os("--clearenv"),
        os("--setenv"),
        os("PATH"),
        os("/usr/local/bin:/usr/bin:/bin"),
        os("--setenv"),
        os("HOME"),
        os(ACTION_SANDBOX_HOME),
        os("--setenv"),
        os("TMPDIR"),
        os(ACTION_SANDBOX_TMPDIR),
        os("--setenv"),
        os("PWD"),
        cwd.as_os_str().to_owned(),
        os("--"),
    ]);
    args.extend(intent.argv().iter().map(OsString::from));

    BwrapProcessPlan {
        program: bwrap_program.as_os_str().to_owned(),
        args,
        cwd,
    }
}

fn append_bwrap_file_bind_args(args: &mut Vec<OsString>, source: &Path, destination: &Path) {
    append_bwrap_mount_parent_args(args, destination);
    args.extend([
        os("--ro-bind"),
        source.as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ]);
}

fn append_bwrap_dir_bind_args(args: &mut Vec<OsString>, source: &Path, destination: &Path) {
    append_bwrap_mount_parent_args(args, destination);
    args.extend([
        os("--ro-bind"),
        source.as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ]);
}

fn append_bwrap_path_rule(args: &mut Vec<OsString>, path: &Path, access: PathAccess) {
    append_bwrap_mount_parent_args(args, path);
    match access {
        PathAccess::ReadOnly => args.extend([
            os("--ro-bind-try"),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::ReadWrite => args.extend([
            os("--bind-try"),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::Deny => {
            args.extend([os("--tmpfs"), path.as_os_str().to_owned()]);
        }
    }
}

fn append_bwrap_mount_parent_args(args: &mut Vec<OsString>, destination: &Path) {
    let Some(parent) = destination.parent() else {
        return;
    };
    let mut parents = parent
        .ancestors()
        .take_while(|path| *path != Path::new("/"))
        .collect::<Vec<_>>();
    parents.reverse();

    for parent in parents {
        args.extend([os("--dir"), parent.as_os_str().to_owned()]);
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
    let program = program.clone();
    let program_for_error = program.clone();
    let args = args.to_vec();

    if context.cancellation_token().is_cancelled() {
        return Err(ProcessRunnerError::Cancelled);
    }

    let mut command = tokio::process::Command::new(&program);
    command
        .args(&args)
        .current_dir(process_current_dir(cwd_root, &intent))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    run_spawned_process(command, intent, context, program_for_error).await
}

async fn run_spawned_process(
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

fn os(value: &str) -> OsString {
    OsString::from(value)
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
    use super::{BwrapProcessRunner, TokioProcessRunner, bwrap_process_plan, process_current_dir};
    use crate::{
        PathAccess, PathAccessRule, PathAccessRuleSource, PermissionRequest, PermissionedAction,
        ProcessActionIntent, ProcessEnvPolicy, ProcessExitStatus, ProcessRunner,
        ProcessRunnerContext,
    };
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use serde_json::json;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use tokio_util::sync::CancellationToken;

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

    fn permission_request(arguments: serde_json::Value) -> PermissionRequest {
        let call = PendingToolCall::new(
            ToolCallId::new("call-permission").expect("valid call id"),
            ToolName::new("request_permissions").expect("valid tool name"),
            ToolCallArguments::try_from(arguments).expect("valid tool arguments"),
        );
        crate::permission::permission_request_from_call(&call, Vec::new())
            .expect("permission request should parse")
    }

    fn request_process_intent(request: &PermissionRequest) -> &ProcessActionIntent {
        let PermissionedAction::Process(intent) = request.action();
        intent
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

    #[test]
    fn bwrap_process_plan_denies_network_by_default() {
        let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
            .with_bwrap_program("/custom/bin/bwrap");
        let plan = bwrap_process_plan(
            &intent(Some("crates")),
            &runner.cwd_root,
            runner.network_allowed,
            &runner.path_rules,
            &runner.bwrap_program,
        );
        let args = os_args(&plan.args);

        assert_eq!(plan.program, OsString::from("/custom/bin/bwrap"));
        assert!(args.iter().any(|arg| arg == "--unshare-net"));
        assert!(contains_sequence(
            &args,
            &["--bind-try", "/workspace/merry", "/workspace/merry"]
        ));
        if Path::new("/etc/ld.so.cache").exists() {
            assert!(contains_sequence(
                &args,
                &["--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache"]
            ));
        }
        assert!(contains_sequence(
            &args,
            &["--chdir", "/workspace/merry/crates"]
        ));
        assert!(contains_sequence(&args, &["--", "pwd"]));
    }

    #[test]
    fn bwrap_process_plan_does_not_expose_outer_graphical_endpoints() {
        let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry")
            .with_bwrap_program("/custom/bin/bwrap");
        let plan = bwrap_process_plan(
            &intent(None),
            &runner.cwd_root,
            runner.network_allowed,
            &runner.path_rules,
            &runner.bwrap_program,
        );
        let args = os_args(&plan.args);

        for forbidden in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "XAUTHORITY",
            "/tmp/.X11-unix",
            "/run/merry-wayland",
            "/run/merry-x11",
        ] {
            assert!(
                !args.iter().any(|arg| arg.contains(forbidden)),
                "inner action sandbox leaked {forbidden}: {args:?}"
            );
        }
    }

    #[test]
    fn bwrap_process_plan_allows_network_when_configured() {
        let runner = BwrapProcessRunner::new_at_workspace_root("/workspace/merry").allow_network();
        let plan = bwrap_process_plan(
            &intent(None),
            &runner.cwd_root,
            runner.network_allowed,
            &runner.path_rules,
            &runner.bwrap_program,
        );
        let args = os_args(&plan.args);

        assert!(!args.iter().any(|arg| arg == "--unshare-net"));
    }

    #[test]
    fn bwrap_process_plan_applies_path_rules() {
        let runner =
            BwrapProcessRunner::new_at_workspace_root("/workspace/merry").with_path_rules([
                PathAccessRule::new(
                    PathBuf::from("/var/log"),
                    PathAccess::ReadOnly,
                    PathAccessRuleSource::TrustedGlobalConfig,
                ),
                PathAccessRule::new(
                    PathBuf::from("/cache"),
                    PathAccess::ReadWrite,
                    PathAccessRuleSource::TrustedGlobalConfig,
                ),
                PathAccessRule::new(
                    PathBuf::from("/home/merry/.ssh"),
                    PathAccess::Deny,
                    PathAccessRuleSource::TrustedGlobalConfig,
                ),
            ]);
        let plan = bwrap_process_plan(
            &intent(None),
            &runner.cwd_root,
            runner.network_allowed,
            &runner.path_rules,
            &runner.bwrap_program,
        );
        let args = os_args(&plan.args);

        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/var/log", "/var/log"]
        ));
        assert!(contains_sequence(
            &args,
            &["--bind-try", "/cache", "/cache"]
        ));
        assert!(contains_sequence(&args, &["--tmpfs", "/home/merry/.ssh"]));
    }

    #[test]
    fn bwrap_permissioned_factory_allows_network_only_when_requested() {
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_bwrap_program("/custom/bin/bwrap");
        let request_without_network = permission_request(json!({
            "requested": {
                "paths": [{ "path": "/workspace/merry", "access": "rw" }]
            },
            "for_action": { "kind": "process", "argv": ["cargo", "test"], "cwd": "." }
        }));
        let request_with_network = permission_request(json!({
            "requested": { "network": true },
            "for_action": { "kind": "process", "argv": ["cargo", "test"], "cwd": "." }
        }));

        let runner_without_network = factory.build_runner(&request_without_network);
        let plan_without_network = bwrap_process_plan(
            request_process_intent(&request_without_network),
            &runner_without_network.cwd_root,
            runner_without_network.network_allowed,
            &runner_without_network.path_rules,
            &runner_without_network.bwrap_program,
        );
        let runner_with_network = factory.build_runner(&request_with_network);
        let plan_with_network = bwrap_process_plan(
            request_process_intent(&request_with_network),
            &runner_with_network.cwd_root,
            runner_with_network.network_allowed,
            &runner_with_network.path_rules,
            &runner_with_network.bwrap_program,
        );

        assert!(os_args(&plan_without_network.args).contains(&"--unshare-net".to_owned()));
        assert!(!os_args(&plan_with_network.args).contains(&"--unshare-net".to_owned()));
    }

    #[test]
    fn bwrap_permissioned_factory_preserves_trusted_path_rules() {
        let factory =
            super::BwrapPermissionedProcessRunnerFactory::new_at_workspace_root("/workspace/merry")
                .with_path_rules([PathAccessRule::new(
                    PathBuf::from("/var/log"),
                    PathAccess::ReadOnly,
                    PathAccessRuleSource::TrustedGlobalConfig,
                )]);
        let request = permission_request(json!({
            "requested": { "network": true },
            "for_action": { "kind": "process", "argv": ["cargo", "test"], "cwd": "." }
        }));

        let runner = factory.build_runner(&request);
        let plan = bwrap_process_plan(
            request_process_intent(&request),
            &runner.cwd_root,
            runner.network_allowed,
            &runner.path_rules,
            &runner.bwrap_program,
        );
        let args = os_args(&plan.args);

        assert!(contains_sequence(
            &args,
            &["--ro-bind-try", "/var/log", "/var/log"]
        ));
    }

    #[tokio::test]
    async fn tokio_process_runner_inherits_current_process_environment() {
        let Ok(path) = std::env::var("PATH") else {
            return;
        };
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let intent = ProcessActionIntent::new(
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf '%s\n%s' \"${PATH-}\" \"${HOME-}\"".to_owned(),
            ],
            None,
            ProcessEnvPolicy::empty(),
            None,
            64 * 1024,
            1024,
        )
        .expect("process intent should be valid");

        let output = TokioProcessRunner::new()
            .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
            .await
            .expect("process should run");

        assert_eq!(output.status(), ProcessExitStatus::Exited(0));
        assert_eq!(output.stdout_text(), format!("{path}\n{home}"));
    }

    fn os_args(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn contains_sequence(args: &[String], expected: &[&str]) -> bool {
        args.windows(expected.len()).any(|window| {
            window
                .iter()
                .map(String::as_str)
                .eq(expected.iter().copied())
        })
    }
}
