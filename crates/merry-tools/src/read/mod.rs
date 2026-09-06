use std::{
    io::{BufReader, Read},
    path::Path,
    sync::Arc,
};

use merry_core::PendingToolCall;
use merry_runtime::{
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use serde::Serialize;

use crate::{
    READ_TEXT_TOOL,
    config::WorkspaceToolLimits,
    errors::{
        BlockingToolError, DomainError, ERROR_FILE_NOT_FOUND, ERROR_FILE_TOO_LARGE,
        ERROR_INVALID_ARGUMENTS, ERROR_NOT_FILE, ERROR_READ_FAILED, failed_outcome,
    },
    path::{
        ValidatedRelativePath, open_file_for_read, resolve_existing_path, validate_relative_path,
    },
    state::WorkspaceToolState,
    trace::{
        WorkspaceTraceFinish, WorkspaceTracePath, WorkspaceTraceTarget, invalid_arguments_outcome,
        trace_workspace_tool_finish, trace_workspace_tool_start,
    },
};

mod bounded;
mod input;
pub(crate) use input::{ReadTextInput, spec};

#[derive(Debug)]
pub(crate) struct ReadTextExecutor {
    pub(crate) state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for ReadTextExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }
            let args = match input::parse(&call) {
                Ok(args) => args,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        READ_TEXT_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_path = WorkspaceTracePath::new(args.path.as_str());
            trace_workspace_tool_start(
                READ_TEXT_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Path(trace_path.as_ref()),
            );
            let state = Arc::clone(&self.state);
            let worker_token = context.cancellation_token().child_token();
            let _cancel_on_drop = worker_token.clone().drop_guard();
            let result = match tokio::task::spawn_blocking(move || {
                read_text_blocking_checked(&state, args, &|| worker_token.is_cancelled())
            })
            .await
            {
                Ok(result) => result,
                Err(error) => Err(ToolExecutionError::infrastructure(format!(
                    "read_text task failed to join: {error}"
                ))),
            };
            let finish = match &result {
                Ok(outcome) => WorkspaceTraceFinish::Outcome(outcome),
                Err(error) => WorkspaceTraceFinish::from_error(error),
            };
            trace_workspace_tool_finish(
                READ_TEXT_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                finish,
            );
            result
        })
    }
}

#[derive(Debug, Serialize)]
struct ReadTextSuccess<'a> {
    ok: bool,
    tool: &'static str,
    path: &'a str,
    start_line: usize,
    end_line: Option<usize>,
    lines: usize,
    bytes: usize,
    truncated: bool,
    content: &'a str,
}

#[cfg(test)]
pub(crate) fn read_text_blocking(
    state: &WorkspaceToolState,
    args: ReadTextInput,
) -> ToolExecutionOutcome {
    read_text_blocking_checked(state, args, &|| false)
        .expect("uncancelled text read should return an outcome")
}

fn read_text_blocking_checked(
    state: &WorkspaceToolState,
    args: ReadTextInput,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }
    let relative = match validate_relative_path(&args.path, state.allow_hidden) {
        Ok(relative) => relative,
        Err(error) => {
            return Ok(failed_outcome(
                READ_TEXT_TOOL,
                error.code,
                error.message,
                error.path,
            ));
        }
    };
    let start_line = args.start_line.unwrap_or(1);
    let max_lines = args.max_lines.unwrap_or(state.limits.max_read_lines);
    if start_line == 0 || max_lines == 0 || max_lines > state.limits.max_read_lines {
        return Ok(failed_outcome(
            READ_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "read_text line range is outside the configured limit",
            Some(relative.display),
        ));
    }
    for root in state.read_roots() {
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }
        match resolve_existing_path(root, &relative) {
            Ok(Some(resolved)) => {
                return match read_resolved_text(
                    &relative,
                    &resolved.path,
                    start_line,
                    max_lines,
                    &state.limits,
                    is_cancelled,
                ) {
                    Ok(outcome) => Ok(outcome),
                    Err(BlockingToolError::Cancelled) => Err(ToolExecutionError::Cancelled),
                    Err(BlockingToolError::Domain(error)) => Ok(failed_outcome(
                        READ_TEXT_TOOL,
                        error.code,
                        error.message,
                        Some(relative.display),
                    )),
                };
            }
            Ok(None) => {}
            Err(error) => {
                return Ok(failed_outcome(
                    READ_TEXT_TOOL,
                    error.code,
                    error.message,
                    Some(relative.display),
                ));
            }
        }
    }
    Ok(failed_outcome(
        READ_TEXT_TOOL,
        ERROR_FILE_NOT_FOUND,
        "workspace file was not found",
        Some(relative.display),
    ))
}

fn read_resolved_text(
    relative: &ValidatedRelativePath,
    path: &Path,
    start_line: usize,
    max_lines: usize,
    limits: &WorkspaceToolLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    let file = open_file_for_read(path)?;
    let metadata = file.metadata().map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;
    if !metadata.is_file() {
        return Err(
            DomainError::new(ERROR_NOT_FILE, "workspace path is not a regular file").into(),
        );
    }
    let file_bytes = metadata.len();
    let byte_limit = u64::try_from(limits.max_read_bytes)
        .map_err(|_| DomainError::new(ERROR_FILE_TOO_LARGE, "workspace read limit is too large"))?;
    let mut reader = BufReader::new(file.take(byte_limit.saturating_add(1)));
    let mut content = String::new();
    let mut scanned_bytes = 0usize;
    let mut selected_lines = 0usize;
    let mut current_line = 0usize;
    let mut truncated = false;
    loop {
        let line = bounded::read_line(
            &mut reader,
            limits.max_read_bytes - scanned_bytes,
            is_cancelled,
        )?;
        if line.is_empty() {
            break;
        }
        scanned_bytes += line.len();
        current_line = current_line.saturating_add(1);
        if current_line < start_line {
            continue;
        }
        content.push_str(&line);
        selected_lines += 1;
        if selected_lines == max_lines {
            truncated = (scanned_bytes as u64) < file_bytes;
            break;
        }
    }
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }
    let payload = ReadTextSuccess {
        ok: true,
        tool: READ_TEXT_TOOL,
        path: &relative.display,
        start_line,
        end_line: (selected_lines > 0).then(|| start_line.saturating_add(selected_lines - 1)),
        lines: selected_lines,
        bytes: content.len(),
        truncated,
        content: &content,
    };
    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("read_text success envelope serializes"),
    ))
}
