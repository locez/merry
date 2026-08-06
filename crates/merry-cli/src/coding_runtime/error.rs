use merry_core::CoreError;
use merry_runtime::{
    AgentLoopConfigError, ContextError, ProcessRunnerError, RuntimeError, RuntimeProfileError,
    SkillError,
};
use merry_tool_workspace::{WorkspaceCodingLoopProfileError, WorkspaceToolConfigError};
use std::{io, path::PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum CodingRuntimeError {
    #[error("invalid coding agent loop config: {source}")]
    AgentLoopConfig {
        #[from]
        source: AgentLoopConfigError,
    },

    #[error("core protocol error while building coding runtime: {source}")]
    Core {
        #[from]
        source: CoreError,
    },

    #[error("invalid action process sandbox: {source}")]
    ProcessRunner {
        #[from]
        source: ProcessRunnerError,
    },

    #[error("failed to read project rules at {path}: {source}")]
    ProjectRulesRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("project rules path is denied at {path}: {reason}")]
    ProjectRulesPathDenied { path: PathBuf, reason: &'static str },

    #[error("project rules path is not a regular file: {path}")]
    ProjectRulesNotRegularFile { path: PathBuf },

    #[error("project rules at {path} exceed the {max_bytes}-byte limit")]
    ProjectRulesTooLarge { path: PathBuf, max_bytes: usize },

    #[error("invalid project rules at {path}: {source}")]
    ProjectRulesInvalid {
        path: PathBuf,
        #[source]
        source: Box<ContextError>,
    },

    #[error("failed to load skill catalog: {source}")]
    SkillCatalog {
        #[from]
        source: SkillError,
    },

    #[error("invalid workspace tools config: {source}")]
    WorkspaceTools {
        #[from]
        source: WorkspaceToolConfigError,
    },

    #[error("invalid workspace coding profile: {source}")]
    WorkspaceProfile {
        #[from]
        source: WorkspaceCodingLoopProfileError,
    },

    #[error("failed to apply runtime profile: {source}")]
    RuntimeProfile {
        #[from]
        source: RuntimeProfileError,
    },

    #[error("failed to apply runtime profile to builder: {source}")]
    RuntimeProfileApply { source: RuntimeError },

    #[error("failed to build runtime: {source}")]
    RuntimeBuild { source: RuntimeError },
}

impl From<CodingRuntimeError> for crate::cli_error::CliError {
    fn from(error: CodingRuntimeError) -> Self {
        crate::cli_error::CliError::Unexpected(error.to_string())
    }
}
