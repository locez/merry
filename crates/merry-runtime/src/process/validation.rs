use super::contracts::{
    MAX_PROCESS_ARG_BYTES, MAX_PROCESS_ARGV_ITEMS, MAX_PROCESS_CWD_BYTES,
    MAX_PROCESS_OUTPUT_LIMIT_BYTES, MAX_PROCESS_STDIN_TEXT_BYTES,
};
use std::path::{Component, Path};
use thiserror::Error;

/// Validation errors for provider-neutral process action values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProcessActionError {
    /// The argv vector was invalid.
    #[error("process action argv {reason}")]
    InvalidArgv {
        /// Validation failure detail.
        reason: &'static str,
    },

    /// One argv item was invalid.
    #[error("process action argv[{index}] {reason}")]
    InvalidArgument {
        /// Invalid argv index.
        index: usize,
        /// Validation failure detail.
        reason: &'static str,
    },

    /// The workspace-relative cwd was invalid.
    #[error("process action cwd {reason}")]
    InvalidCwd {
        /// Validation failure detail.
        reason: &'static str,
    },

    /// Inline stdin text was invalid.
    #[error("process action stdin_text {reason}")]
    InvalidStdinText {
        /// Validation failure detail.
        reason: &'static str,
    },

    /// An output capture limit was invalid.
    #[error("process action {field} {reason}")]
    InvalidOutputLimit {
        /// Invalid field name.
        field: &'static str,
        /// Validation failure detail.
        reason: &'static str,
    },

    /// Execute-time evidence was inconsistent with the validated intent.
    #[error("process execution evidence {field} {reason}")]
    InvalidExecutionEvidence {
        /// Invalid field name.
        field: &'static str,
        /// Validation failure detail.
        reason: &'static str,
    },
}

pub(super) fn validate_argv(argv: &[String]) -> Result<(), ProcessActionError> {
    if argv.is_empty() {
        return Err(ProcessActionError::InvalidArgv {
            reason: "must not be empty",
        });
    }
    if argv.len() > MAX_PROCESS_ARGV_ITEMS {
        return Err(ProcessActionError::InvalidArgv {
            reason: "contains too many arguments",
        });
    }
    for (index, argument) in argv.iter().enumerate() {
        if argument.is_empty() {
            return Err(ProcessActionError::InvalidArgument {
                index,
                reason: "must not be empty",
            });
        }
        if argument.len() > MAX_PROCESS_ARG_BYTES {
            return Err(ProcessActionError::InvalidArgument {
                index,
                reason: "exceeds the byte limit",
            });
        }
        if argument.chars().any(disallowed_argv_control_character) {
            return Err(ProcessActionError::InvalidArgument {
                index,
                reason: "must not contain control characters other than newline or tab",
            });
        }
    }

    Ok(())
}

fn disallowed_argv_control_character(character: char) -> bool {
    character.is_control() && !matches!(character, '\n' | '\t')
}

pub(super) fn validate_cwd(cwd: Option<String>) -> Result<Option<String>, ProcessActionError> {
    let Some(value) = cwd else {
        return Ok(None);
    };

    if value.trim().is_empty() {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must not be blank",
        });
    }
    if value.len() > MAX_PROCESS_CWD_BYTES {
        return Err(ProcessActionError::InvalidCwd {
            reason: "exceeds the byte limit",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must not contain control characters",
        });
    }
    if value.split('/').any(str::is_empty) {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must not contain empty path segments",
        });
    }
    if value != "."
        && value
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must not contain dot segments",
        });
    }

    let path = Path::new(&value);
    if path.is_absolute() {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must be relative",
        });
    }

    let mut saw_component = false;
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                if value.to_str().is_none() {
                    return Err(ProcessActionError::InvalidCwd {
                        reason: "components must be UTF-8",
                    });
                }
                saw_component = true;
            }
            Component::CurDir if value == "." => {}
            Component::CurDir | Component::ParentDir => {
                return Err(ProcessActionError::InvalidCwd {
                    reason: "must not contain dot segments",
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ProcessActionError::InvalidCwd {
                    reason: "must be relative",
                });
            }
        }
    }

    if !saw_component && value != "." {
        return Err(ProcessActionError::InvalidCwd {
            reason: "must name a workspace directory",
        });
    }

    Ok(Some(value))
}

pub(super) fn validate_stdin_text(stdin_text: Option<&str>) -> Result<(), ProcessActionError> {
    if stdin_text.is_some_and(|text| text.len() > MAX_PROCESS_STDIN_TEXT_BYTES) {
        return Err(ProcessActionError::InvalidStdinText {
            reason: "exceeds the byte limit",
        });
    }
    Ok(())
}

pub(super) fn validate_output_limit(
    field: &'static str,
    limit: usize,
) -> Result<(), ProcessActionError> {
    if limit == 0 {
        return Err(ProcessActionError::InvalidOutputLimit {
            field,
            reason: "must be greater than zero",
        });
    }
    if limit > MAX_PROCESS_OUTPUT_LIMIT_BYTES {
        return Err(ProcessActionError::InvalidOutputLimit {
            field,
            reason: "exceeds the byte limit",
        });
    }
    Ok(())
}

pub(super) fn validate_captured_bytes(
    field: &'static str,
    bytes: usize,
    limit: usize,
) -> Result<(), ProcessActionError> {
    if bytes > limit {
        return Err(ProcessActionError::InvalidExecutionEvidence {
            field,
            reason: "must not exceed the intent output limit",
        });
    }
    Ok(())
}

pub(super) fn summarize_intent(argv: &[String], cwd: Option<&str>) -> String {
    let executable = argv
        .first()
        .expect("process intent summary is built after argv validation");
    let cwd = cwd.unwrap_or(".");
    format!(
        "process argv[0]={executable}; argc={}; cwd={cwd}",
        argv.len()
    )
}
