use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use merry_core::PendingToolCall;
use merry_runtime::{
    ToolExecutionContext, ToolExecutionError, ToolExecutionOutcome, ToolExecutor,
    ToolExecutorFuture,
};
use regex::Regex;
use serde::Serialize;

use crate::{
    WORKSPACE_SEARCH_TEXT_TOOL,
    config::WorkspaceToolLimits,
    errors::{
        BlockingToolError, DomainError, ERROR_INVALID_ARGUMENTS, ERROR_NOT_SEARCHABLE,
        ERROR_PATH_DENIED, ERROR_PATH_NOT_FOUND, ERROR_READ_FAILED, WorkspaceGuidance,
        blocking_tool_error_into_execution, failed_outcome, workspace_search_success_guidance,
    },
    path::{
        is_hidden_name, join_display_path, open_file_for_read, resolve_existing_path,
        truncate_utf8_line, validate_relative_path_or_root,
    },
    schema::{SearchTextArgs, parse_search_text_args},
    state::WorkspaceToolState,
    trace::{
        WorkspaceTraceFinish, WorkspaceTracePath, WorkspaceTraceTarget, invalid_arguments_outcome,
        trace_workspace_tool_finish, trace_workspace_tool_start,
    },
};

#[derive(Debug)]
pub(crate) struct SearchTextExecutor {
    pub(crate) state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for SearchTextExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let args = match parse_search_text_args(&call) {
                Ok(args) => args,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        WORKSPACE_SEARCH_TEXT_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_path = args.path.as_deref().map(WorkspaceTracePath::new);
            let trace_query_bytes = args.query.len();
            trace_workspace_tool_start(
                WORKSPACE_SEARCH_TEXT_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Search {
                    path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                    query_bytes: trace_query_bytes,
                },
            );

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let worker_token = token.clone();
            let handle = tokio::task::spawn_blocking(move || {
                let is_cancelled = || worker_token.is_cancelled();
                search_text_blocking_checked(&state, args, &is_cancelled)
            });

            tokio::select! {
                biased;
                () = token.cancelled() => {
                    trace_workspace_tool_finish(
                        WORKSPACE_SEARCH_TEXT_TOOL,
                        call.id().as_str(),
                        WorkspaceTraceTarget::Search {
                            path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                            query_bytes: trace_query_bytes,
                        },
                        WorkspaceTraceFinish::Cancelled,
                    );
                    Err(ToolExecutionError::Cancelled)
                }
                joined = handle => match joined {
                    Ok(Ok(outcome)) => {
                        if token.is_cancelled() {
                            trace_workspace_tool_finish(
                                WORKSPACE_SEARCH_TEXT_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Search {
                                    path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                                    query_bytes: trace_query_bytes,
                                },
                                WorkspaceTraceFinish::Cancelled,
                            );
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            trace_workspace_tool_finish(
                                WORKSPACE_SEARCH_TEXT_TOOL,
                                call.id().as_str(),
                                WorkspaceTraceTarget::Search {
                                    path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                                    query_bytes: trace_query_bytes,
                                },
                                WorkspaceTraceFinish::Outcome(&outcome),
                            );
                            Ok(outcome)
                        }
                    }
                    Ok(Err(error)) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_SEARCH_TEXT_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Search {
                                path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                                query_bytes: trace_query_bytes,
                            },
                            WorkspaceTraceFinish::from_error(&error),
                        );
                        Err(error)
                    }
                    Err(error) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_SEARCH_TEXT_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Search {
                                path: trace_path.as_ref().map(WorkspaceTracePath::as_str),
                                query_bytes: trace_query_bytes,
                            },
                            WorkspaceTraceFinish::Error("workspace_infrastructure_error"),
                        );
                        Err(ToolExecutionError::infrastructure(format!(
                            "workspace search task failed to join: {error}"
                        )))
                    }
                },
            }
        })
    }
}

#[derive(Debug, Serialize)]
struct SearchTextSuccess<'a> {
    ok: bool,
    tool: &'static str,
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    matches: Vec<SearchMatch>,
    searched_files: usize,
    skipped: SearchSkipCounts,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance: Option<WorkspaceGuidance>,
}

#[derive(Debug, Serialize)]
struct SearchMatch {
    path: String,
    line_number: usize,
    line: String,
    truncated: bool,
}

#[derive(Debug, Default, Serialize)]
struct SearchSkipCounts {
    hidden: usize,
    symlink: usize,
    non_utf8: usize,
    too_large: usize,
    read_failed: usize,
}

#[cfg(test)]
pub(crate) fn search_text_blocking(
    state: &WorkspaceToolState,
    args: SearchTextArgs,
) -> ToolExecutionOutcome {
    search_text_blocking_checked(state, args, &|| false)
        .expect("uncancelled workspace search should not return cancellation")
}

pub(crate) fn search_text_blocking_checked(
    state: &WorkspaceToolState,
    args: SearchTextArgs,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ToolExecutionOutcome, ToolExecutionError> {
    if is_cancelled() {
        return Err(ToolExecutionError::Cancelled);
    }

    if args.query.is_empty() {
        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace search query must not be empty",
            None::<String>,
        ));
    }
    if args.query.len() > state.limits.max_search_query_bytes {
        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace search query exceeds the configured byte limit",
            None::<String>,
        ));
    }
    if args.query.chars().any(char::is_control) {
        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace search query must be a single line without control characters",
            None::<String>,
        ));
    }

    let max_matches = args
        .max_matches
        .unwrap_or(state.limits.max_search_matches)
        .min(state.limits.max_search_matches);
    if max_matches == 0 {
        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_INVALID_ARGUMENTS,
            "workspace search max_matches must be greater than zero",
            None::<String>,
        ));
    }

    let regex = match Regex::new(&args.query) {
        Ok(regex) => regex,
        Err(error) => {
            let error = error
                .to_string()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            return Ok(failed_outcome(
                WORKSPACE_SEARCH_TEXT_TOOL,
                ERROR_INVALID_ARGUMENTS,
                format!("workspace search query is not a valid regular expression: {error}"),
                None::<String>,
            ));
        }
    };
    let mut search = SearchRun::new(regex, max_matches, &state.limits, state.allow_hidden);

    if let Some(path) = args.path {
        let relative = match validate_relative_path_or_root(&path, state.allow_hidden) {
            Ok(relative) => relative,
            Err(error) => {
                return Ok(failed_outcome(
                    WORKSPACE_SEARCH_TEXT_TOOL,
                    error.code,
                    error.message,
                    error.path,
                ));
            }
        };

        let display = relative.display.clone();
        for root in &state.roots {
            if is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            match resolve_existing_path(root, &relative) {
                Ok(Some(resolved)) => {
                    return match search_resolved_path(
                        &mut search,
                        &resolved.path,
                        &display,
                        is_cancelled,
                    ) {
                        Ok(()) => Ok(search_success(&args.query, Some(display), search)),
                        Err(BlockingToolError::Domain(error)) => Ok(failed_outcome(
                            WORKSPACE_SEARCH_TEXT_TOOL,
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
                        WORKSPACE_SEARCH_TEXT_TOOL,
                        error.code,
                        error.message,
                        Some(relative.display),
                    ));
                }
            }
        }

        return Ok(failed_outcome(
            WORKSPACE_SEARCH_TEXT_TOOL,
            ERROR_PATH_NOT_FOUND,
            "workspace search path was not found",
            Some(relative.display),
        ));
    }

    for root in &state.roots {
        if search.is_done() {
            break;
        }
        if is_cancelled() {
            return Err(ToolExecutionError::Cancelled);
        }
        search_directory(&mut search, root, ".", is_cancelled)
            .map_err(blocking_tool_error_into_execution)?;
    }

    Ok(search_success(&args.query, None, search))
}

#[derive(Debug)]
struct SearchRun<'a> {
    regex: Regex,
    max_matches: usize,
    limits: &'a WorkspaceToolLimits,
    allow_hidden: bool,
    matches: Vec<SearchMatch>,
    searched_files: usize,
    scanned_bytes: usize,
    inspected_entries: usize,
    skipped: SearchSkipCounts,
    truncated: bool,
}

impl<'a> SearchRun<'a> {
    fn new(
        regex: Regex,
        max_matches: usize,
        limits: &'a WorkspaceToolLimits,
        allow_hidden: bool,
    ) -> Self {
        Self {
            regex,
            max_matches,
            limits,
            allow_hidden,
            matches: Vec::new(),
            searched_files: 0,
            scanned_bytes: 0,
            inspected_entries: 0,
            skipped: SearchSkipCounts::default(),
            truncated: false,
        }
    }

    fn is_done(&self) -> bool {
        self.truncated || self.matches.len() >= self.max_matches
    }

    fn has_search_budget(&self) -> bool {
        self.scanned_bytes < self.limits.max_search_bytes
    }

    fn remaining_search_bytes(&self) -> usize {
        self.limits
            .max_search_bytes
            .saturating_sub(self.scanned_bytes)
    }

    fn record_scanned_bytes(&mut self, bytes: usize) {
        self.scanned_bytes = self.scanned_bytes.saturating_add(bytes);
    }

    fn try_inspect_entry(&mut self) -> bool {
        if self.inspected_entries >= self.limits.max_search_entries {
            self.truncated = true;
            return false;
        }

        self.inspected_entries += 1;
        true
    }
}

fn search_success(
    query: &str,
    path: Option<String>,
    search: SearchRun<'_>,
) -> ToolExecutionOutcome {
    let guidance = workspace_search_success_guidance(search.truncated, search.skipped.too_large);
    let payload = SearchTextSuccess {
        ok: true,
        tool: WORKSPACE_SEARCH_TEXT_TOOL,
        query,
        path: path.as_deref(),
        matches: search.matches,
        searched_files: search.searched_files,
        skipped: search.skipped,
        truncated: search.truncated,
        guidance,
    };
    ToolExecutionOutcome::succeeded_json(
        serde_json::to_string(&payload).expect("workspace search success envelope serializes"),
    )
}

fn search_resolved_path(
    search: &mut SearchRun<'_>,
    path: &Path,
    display: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return Err(DomainError::new(
                ERROR_READ_FAILED,
                "could not inspect workspace search path",
            )
            .into());
        }
    };

    if metadata.is_file() {
        search_file(search, path, display, is_cancelled)?;
        Ok(())
    } else if metadata.is_dir() {
        search_directory(search, path, display, is_cancelled)?;
        Ok(())
    } else {
        Err(DomainError::new(
            ERROR_NOT_SEARCHABLE,
            "workspace search path is not a regular file or directory",
        )
        .into())
    }
}

fn search_directory(
    search: &mut SearchRun<'_>,
    path: &Path,
    display_prefix: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let read_dir = match fs::read_dir(path) {
        Ok(read_dir) => read_dir,
        Err(_) => {
            search.skipped.read_failed += 1;
            return Ok(());
        }
    };

    let mut children = Vec::new();
    for entry in read_dir {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        if search.is_done() {
            break;
        }
        if !search.try_inspect_entry() {
            break;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                search.skipped.read_failed += 1;
                continue;
            }
        };

        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            search.skipped.non_utf8 += 1;
            continue;
        };
        if !search.allow_hidden && is_hidden_name(&name) {
            search.skipped.hidden += 1;
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                search.skipped.read_failed += 1;
                continue;
            }
        };

        if file_type.is_symlink() {
            search.skipped.symlink += 1;
            continue;
        }

        let display = join_display_path(display_prefix, &name);
        if file_type.is_dir() || file_type.is_file() {
            children.push(SearchChild {
                path: entry.path(),
                display,
                is_dir: file_type.is_dir(),
            });
        }
    }

    children.sort_by(|left, right| left.display.cmp(&right.display));
    for child in children {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        if search.is_done() {
            break;
        }
        if !search.has_search_budget() {
            search.truncated = true;
            break;
        }

        if child.is_dir {
            search_directory(search, &child.path, &child.display, is_cancelled)?;
        } else {
            search_file(search, &child.path, &child.display, is_cancelled)?;
        }
    }

    Ok(())
}

#[derive(Debug)]
struct SearchChild {
    path: PathBuf,
    display: String,
    is_dir: bool,
}

fn search_file(
    search: &mut SearchRun<'_>,
    path: &Path,
    display: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    if search.searched_files >= search.limits.max_search_files {
        search.truncated = true;
        return Ok(());
    }
    if !search.has_search_budget() {
        search.truncated = true;
        return Ok(());
    }

    let mut file = match open_file_for_read(path) {
        Ok(file) => file,
        Err(error) => {
            if error.code == ERROR_PATH_DENIED {
                search.skipped.symlink += 1;
            } else {
                search.skipped.read_failed += 1;
            }
            return Ok(());
        }
    };

    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => {
            search.skipped.read_failed += 1;
            return Ok(());
        }
    };
    if !metadata.is_file() {
        return Ok(());
    }

    if metadata.len() > search.limits.max_read_bytes as u64 {
        search.searched_files += 1;
        search.skipped.too_large += 1;
        return Ok(());
    }
    let file_size = match usize::try_from(metadata.len()) {
        Ok(file_size) => file_size,
        Err(_) => {
            search.searched_files += 1;
            search.skipped.too_large += 1;
            return Ok(());
        }
    };
    if file_size > search.remaining_search_bytes() {
        search.truncated = true;
        return Ok(());
    }

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let mut bytes = Vec::new();
    if Read::by_ref(&mut file)
        .take(metadata.len())
        .read_to_end(&mut bytes)
        .is_err()
    {
        search.skipped.read_failed += 1;
        return Ok(());
    }

    search.searched_files += 1;
    search.record_scanned_bytes(bytes.len());

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(_) => {
            search.skipped.non_utf8 += 1;
            return Ok(());
        }
    };

    for (line_index, line) in content.lines().enumerate() {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }

        if search.regex.is_match(line) {
            let (line, truncated) = truncate_utf8_line(line, search.limits.max_search_line_bytes);
            search.matches.push(SearchMatch {
                path: display.to_owned(),
                line_number: line_index + 1,
                line,
                truncated,
            });
            if search.matches.len() >= search.max_matches {
                search.truncated = true;
                return Ok(());
            }
        }
    }

    Ok(())
}
