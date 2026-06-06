use std::{fmt, io};

#[derive(Debug)]
pub(crate) enum CliError {
    BrokenPipe,
    DebugUsage(String),
    DebugOpenAiUsage(String),
    ShellUsage(String),
    Unexpected(String),
}

pub(crate) fn unexpected(err: impl fmt::Display) -> CliError {
    CliError::Unexpected(err.to_string())
}

pub(crate) fn usage_error(err: impl fmt::Display) -> CliError {
    CliError::DebugUsage(err.to_string())
}

pub(crate) fn debug_openai_usage_error(err: impl fmt::Display) -> CliError {
    CliError::DebugOpenAiUsage(err.to_string())
}

pub(crate) fn shell_usage_error(err: impl fmt::Display) -> CliError {
    CliError::ShellUsage(err.to_string())
}

pub(crate) fn stdout_error(err: io::Error) -> CliError {
    if err.kind() == io::ErrorKind::BrokenPipe {
        CliError::BrokenPipe
    } else {
        CliError::Unexpected(err.to_string())
    }
}
