use merry::profiles::CodingRuntimeBuildError;
use merry_core::CoreError;
use merry_process::ProcessBackendError;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum CodingRuntimeError {
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

    #[error("invalid coding runtime composition: {source}")]
    CodingRuntime {
        #[from]
        source: CodingRuntimeBuildError,
    },
}

impl From<CodingRuntimeError> for crate::cli_error::CliError {
    fn from(error: CodingRuntimeError) -> Self {
        crate::cli_error::CliError::Unexpected(error.to_string())
    }
}
