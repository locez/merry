use std::{io::Read, path::Path, sync::Arc};

use merry_core::PendingToolCall;
use merry_runtime::{
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use serde::Serialize;

use crate::{
    WORKSPACE_READ_FILE_TOOL,
    config::WorkspaceToolLimits,
    errors::{
        DomainError, ERROR_FILE_NOT_FOUND, ERROR_FILE_TOO_LARGE, ERROR_NOT_FILE, ERROR_NOT_UTF8,
        ERROR_READ_FAILED, failed_outcome,
    },
    path::{
        ValidatedRelativePath, open_file_for_read, resolve_existing_path, validate_relative_path,
    },
    schema::parse_read_file_args,
    state::WorkspaceToolState,
    trace::{
        WorkspaceTraceFinish, WorkspaceTracePath, WorkspaceTraceTarget, invalid_arguments_outcome,
        trace_workspace_tool_finish, trace_workspace_tool_start,
    },
};

#[derive(Debug)]
pub(crate) struct ReadFileExecutor {
    pub(crate) state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for ReadFileExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let path = match parse_read_file_args(&call) {
                Ok(args) => args.path,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        WORKSPACE_READ_FILE_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_path = WorkspaceTracePath::new(path.as_str());
            trace_workspace_tool_start(
                WORKSPACE_READ_FILE_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Path(trace_path.as_ref()),
            );

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let handle = tokio::task::spawn_blocking(move || read_file_blocking(&state, path));

            tokio::select! {
                biased;
                () = token.cancelled() => {
                    trace_workspace_tool_finish(
                        WORKSPACE_READ_FILE_TOOL,
                        call.id().as_str(),
                        WorkspaceTraceTarget::Path(trace_path.as_ref()),
                        WorkspaceTraceFinish::Cancelled,
                    );
                    Err(ToolExecutionError::Cancelled)
                }
                joined = handle => match joined {
                    Ok(outcome) => {
                        if token.is_cancelled() {
                            trace_workspace_tool_finish(
                                WORKSPACE_READ_FILE_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                                WorkspaceTraceFinish::Cancelled,
                            );
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            trace_workspace_tool_finish(
                                WORKSPACE_READ_FILE_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                                WorkspaceTraceFinish::Outcome(&outcome),
                            );
                            Ok(outcome)
                        }
                    }
                    Err(error) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_READ_FILE_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Path(trace_path.as_ref()),
                            WorkspaceTraceFinish::Error("workspace_infrastructure_error"),
                        );
                        Err(ToolExecutionError::infrastructure(format!(
                            "workspace read task failed to join: {error}"
                        )))
                    }
                },
            }
        })
    }
}

#[derive(Debug, Serialize)]
struct ReadFileSuccess<'a> {
    ok: bool,
    tool: &'static str,
    path: &'a str,
    bytes: usize,
    truncated: bool,
    content: &'a str,
}

pub(crate) fn read_file_blocking(state: &WorkspaceToolState, path: String) -> ToolExecutionOutcome {
    let relative = match validate_relative_path(&path, state.allow_hidden) {
        Ok(relative) => relative,
        Err(error) => {
            return failed_outcome(
                WORKSPACE_READ_FILE_TOOL,
                error.code,
                error.message,
                error.path,
            );
        }
    };

    for root in state.read_roots() {
        match resolve_existing_path(root, &relative) {
            Ok(Some(resolved)) => {
                return match read_resolved_file(&relative, &resolved.path, &state.limits) {
                    Ok(success) => success,
                    Err(error) => failed_outcome(
                        WORKSPACE_READ_FILE_TOOL,
                        error.code,
                        error.message,
                        Some(relative.display),
                    ),
                };
            }
            Ok(None) => {}
            Err(error) => {
                return failed_outcome(
                    WORKSPACE_READ_FILE_TOOL,
                    error.code,
                    error.message,
                    Some(relative.display),
                );
            }
        }
    }

    failed_outcome(
        WORKSPACE_READ_FILE_TOOL,
        ERROR_FILE_NOT_FOUND,
        "workspace file was not found",
        Some(relative.display),
    )
}

fn read_resolved_file(
    relative: &ValidatedRelativePath,
    path: &Path,
    limits: &WorkspaceToolLimits,
) -> Result<ToolExecutionOutcome, DomainError> {
    let mut file = open_file_for_read(path)?;
    let metadata = file.metadata().map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;

    if !metadata.is_file() {
        return Err(DomainError::new(
            ERROR_NOT_FILE,
            "workspace path is not a regular file",
        ));
    }

    if metadata.len() > limits.max_read_bytes as u64 {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        ));
    }

    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limits.max_read_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;

    if bytes.len() > limits.max_read_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        ));
    }

    let content = String::from_utf8(bytes)
        .map_err(|_| DomainError::new(ERROR_NOT_UTF8, "workspace file is not valid UTF-8"))?;

    let payload = ReadFileSuccess {
        ok: true,
        tool: WORKSPACE_READ_FILE_TOOL,
        path: &relative.display,
        bytes: content.len(),
        truncated: false,
        content: &content,
    };
    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace read success envelope serializes"),
    ))
}
