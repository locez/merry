//! Direct host-process execution backed by Tokio.

use super::{
    process_current_dir, run_spawned_process, validate_environment_name, validate_os_string,
};
use merry_runtime::{
    ProcessActionIntent, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
    ProcessRunnerFuture,
};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Host-process runner backed by [`tokio::process::Command`].
///
/// The runner executes the exact validated argv supplied by
/// [`ProcessActionIntent`], inherits the current process environment, closes
/// stdin, captures bounded stdout/stderr through the shared process IO path,
/// and cooperatively cancels by killing the child process. Permission profiles
/// and sandbox constraints are selected by the owning backend, not by this
/// executor.
#[derive(Debug, Default, Clone)]
pub struct TokioProcessRunner {
    cwd_root: Option<PathBuf>,
    environment_overrides: Vec<(OsString, OsString)>,
}

impl TokioProcessRunner {
    /// Creates a Tokio-backed process runner.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cwd_root: None,
            environment_overrides: Vec::new(),
        }
    }

    /// Creates a Tokio-backed process runner whose process cwd values are
    /// resolved under a stable workspace root.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: Some(root.into()),
            environment_overrides: Vec::new(),
        }
    }

    /// Validates and applies environment assignments while preserving all
    /// other variables inherited from the Merry process.
    pub fn with_environment_overrides(
        mut self,
        overrides: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, ProcessRunnerError> {
        let mut names = BTreeSet::new();
        let mut validated = Vec::new();
        for (name, value) in overrides {
            validate_environment_name(&name)?;
            validate_os_string(&value, "host process environment value")?;
            if !names.insert(name.clone()) {
                return Err(ProcessRunnerError::infrastructure(
                    "host process environment contains a duplicate variable",
                ));
            }
            validated.push((name, value));
        }
        self.environment_overrides = validated;
        Ok(self)
    }
}

impl ProcessRunner for TokioProcessRunner {
    fn run<'a>(
        &'a self,
        intent: ProcessActionIntent,
        context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        let cwd_root = self.cwd_root.clone();
        let environment_overrides = self.environment_overrides.clone();
        Box::pin(async move {
            run_tokio_process(intent, context, cwd_root.as_deref(), &environment_overrides).await
        })
    }
}

async fn run_tokio_process(
    intent: ProcessActionIntent,
    context: ProcessRunnerContext,
    cwd_root: Option<&Path>,
    environment_overrides: &[(OsString, OsString)],
) -> Result<merry_runtime::ProcessRunnerOutput, ProcessRunnerError> {
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
        .envs(
            environment_overrides
                .iter()
                .map(|(name, value)| (name, value)),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    run_spawned_process(command, intent, context, program_for_error).await
}
