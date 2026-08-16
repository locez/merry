use std::{fs, path::Path, sync::Arc};

use merry_core::PendingToolCall;
use merry_runtime::{
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use serde::Serialize;

use crate::{
    WORKSPACE_LIST_DIR_TOOL,
    errors::{
        BlockingToolError, DomainError, ERROR_NOT_DIRECTORY, ERROR_PATH_NOT_FOUND,
        ERROR_READ_FAILED, GUIDANCE_LIST_TRUNCATED, WorkspaceGuidance, failed_outcome,
    },
    path::{
        ValidatedRelativePath, is_hidden_name, join_display_path, resolve_existing_path,
        validate_relative_path_or_root,
    },
    schema::parse_list_dir_args,
    state::WorkspaceToolState,
    trace::{
        WorkspaceTraceFinish, WorkspaceTracePath, WorkspaceTraceTarget, invalid_arguments_outcome,
        trace_workspace_tool_finish, trace_workspace_tool_start,
    },
};

#[derive(Debug)]
pub(crate) struct ListDirExecutor {
    pub(crate) state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for ListDirExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let path = match parse_list_dir_args(&call) {
                Ok(args) => args.path,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        WORKSPACE_LIST_DIR_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_path = WorkspaceTracePath::new(path.as_str());
            trace_workspace_tool_start(
                WORKSPACE_LIST_DIR_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Path(trace_path.as_ref()),
            );

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let worker_token = token.clone();
            let handle = tokio::task::spawn_blocking(move || {
                let is_cancelled = || worker_token.is_cancelled();
                list_dir_blocking_checked(&state, path, &is_cancelled)
            });

            tokio::select! {
                biased;
                () = token.cancelled() => {
                    trace_workspace_tool_finish(
                        WORKSPACE_LIST_DIR_TOOL,
                        call.id().as_str(),
                        WorkspaceTraceTarget::Path(trace_path.as_ref()),
                        WorkspaceTraceFinish::Cancelled,
                    );
                    Err(ToolExecutionError::Cancelled)
                }
                joined = handle => match joined {
                    Ok(Ok(outcome)) => {
                        if token.is_cancelled() {
                            trace_workspace_tool_finish(
                                WORKSPACE_LIST_DIR_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                                WorkspaceTraceFinish::Cancelled,
                            );
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            trace_workspace_tool_finish(
                                WORKSPACE_LIST_DIR_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Path(trace_path.as_ref()),
                                WorkspaceTraceFinish::Outcome(&outcome),
                            );
                            Ok(outcome)
                        }
                    }
                    Ok(Err(error)) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_LIST_DIR_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Path(trace_path.as_ref()),
                            WorkspaceTraceFinish::from_error(&error),
                        );
                        Err(error)
                    }
                    Err(error) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_LIST_DIR_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Path(trace_path.as_ref()),
                            WorkspaceTraceFinish::Error("workspace_infrastructure_error"),
                        );
                        Err(ToolExecutionError::infrastructure(format!(
                            "workspace list task failed to join: {error}"
                        )))
                    }
                },
            }
        })
    }
}

#[derive(Debug, Serialize)]
struct ListDirSuccess<'a> {
    ok: bool,
    tool: &'static str,
    path: &'a str,
    entries: Vec<ListDirEntry>,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance: Option<WorkspaceGuidance>,
}

#[derive(Debug, Serialize)]
struct ListDirEntry {
    name: String,
    path: String,
    kind: EntryKind,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[cfg(test)]
pub(crate) fn list_dir_blocking(state: &WorkspaceToolState, path: String) -> ToolExecutionOutcome {
    list_dir_blocking_checked(state, path, &|| false)
        .expect("uncancelled workspace list should not return cancellation")
}

pub(crate) fn list_dir_blocking_checked(
    state: &WorkspaceToolState,
    path: String,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    let relative = match validate_relative_path_or_root(&path, state.allow_hidden) {
        Ok(relative) => relative,
        Err(error) => {
            return Ok(failed_outcome(
                WORKSPACE_LIST_DIR_TOOL,
                error.code,
                error.message,
                error.path,
            ));
        }
    };

    for root in state.read_roots() {
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }

        match resolve_existing_path(root, &relative) {
            Ok(Some(resolved)) => {
                return match list_resolved_dir(&relative, &resolved.path, state, is_cancelled) {
                    Ok(success) => Ok(success),
                    Err(BlockingToolError::Domain(error)) => Ok(failed_outcome(
                        WORKSPACE_LIST_DIR_TOOL,
                        error.code,
                        error.message,
                        Some(relative.display),
                    )),
                    Err(BlockingToolError::Cancelled) => Err(ToolExecutionError::Cancelled),
                };
            }
            Ok(None) => {}
            Err(error) => {
                return Ok(failed_outcome(
                    WORKSPACE_LIST_DIR_TOOL,
                    error.code,
                    error.message,
                    Some(relative.display),
                ));
            }
        }
    }

    Ok(failed_outcome(
        WORKSPACE_LIST_DIR_TOOL,
        ERROR_PATH_NOT_FOUND,
        "workspace directory was not found",
        Some(relative.display),
    ))
}

fn list_resolved_dir(
    relative: &ValidatedRelativePath,
    path: &Path,
    state: &WorkspaceToolState,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, BlockingToolError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace directory metadata",
        )
    })?;
    if !metadata.is_dir() {
        return Err(
            DomainError::new(ERROR_NOT_DIRECTORY, "workspace path is not a directory").into(),
        );
    }

    let mut entries = Vec::new();
    let read_dir = fs::read_dir(path)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace directory"))?;
    for entry in read_dir {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }

        let entry = entry.map_err(|_| {
            DomainError::new(
                ERROR_READ_FAILED,
                "could not read workspace directory entry",
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !state.allow_hidden && is_hidden_name(&name) {
            continue;
        }

        let file_type = entry.file_type().map_err(|_| {
            DomainError::new(
                ERROR_READ_FAILED,
                "could not inspect workspace directory entry",
            )
        })?;
        let entry_path = join_display_path(&relative.display, &name);
        push_bounded_list_entry(
            &mut entries,
            ListDirEntry {
                name,
                path: entry_path,
                kind: entry_kind(file_type),
            },
            state.limits.max_list_entries,
        );
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let truncated = entries.len() > state.limits.max_list_entries;
    entries.truncate(state.limits.max_list_entries);

    let payload = ListDirSuccess {
        ok: true,
        tool: WORKSPACE_LIST_DIR_TOOL,
        path: &relative.display,
        entries,
        truncated,
        guidance: truncated.then_some(WorkspaceGuidance {
            kind: "workspace_list_truncated",
            message: GUIDANCE_LIST_TRUNCATED,
        }),
    };
    Ok(ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace list success envelope serializes"),
    ))
}

fn push_bounded_list_entry(
    entries: &mut Vec<ListDirEntry>,
    entry: ListDirEntry,
    max_entries: usize,
) {
    let retention_limit = max_entries.saturating_add(1);
    if entries.len() < retention_limit {
        entries.push(entry);
        return;
    }

    let Some(current_last) = entries
        .iter()
        .max_by(|left, right| left.name.cmp(&right.name))
    else {
        entries.push(entry);
        return;
    };

    if entry.name >= current_last.name {
        return;
    }

    let current_last_name = current_last.name.clone();
    if let Some(index) = entries
        .iter()
        .position(|candidate| candidate.name == current_last_name)
    {
        entries[index] = entry;
    }
}

fn entry_kind(file_type: fs::FileType) -> EntryKind {
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}
