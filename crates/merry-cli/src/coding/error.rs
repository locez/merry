use merry::profiles::{CodingAgentProfileBuildError, ProjectRulesLoadError};
use merry_core::CoreError;
use merry_process::ProcessBackendError;
use merry_runtime::{AgentLoopConfigError, RuntimeError, SkillError};
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

    #[error("invalid action process backend: {source}")]
    ProcessBackend {
        #[from]
        source: ProcessBackendError,
    },

    #[error("failed to load project rules: {source}")]
    ProjectRules {
        #[from]
        source: ProjectRulesLoadError,
    },

    #[error("invalid shared coding-agent profile: {source}")]
    CodingAgentProfile {
        #[from]
        source: CodingAgentProfileBuildError,
    },

    #[error("failed to load skill catalog: {source}")]
    SkillCatalog {
        #[from]
        source: SkillError,
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
