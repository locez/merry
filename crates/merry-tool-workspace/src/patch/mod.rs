use std::sync::Arc;

use merry_core::PendingToolCall;
use merry_runtime::{
    ToolActionPreflight, ToolActionProposalFuture, ToolExecutionContext, ToolExecutionError,
    ToolExecutor, ToolExecutorFuture,
};

use crate::{
    WORKSPACE_PATCH_TOOL,
    errors::{ERROR_INVALID_ARGUMENTS, failed_outcome},
    schema::parse_workspace_patch_args,
    state::WorkspaceToolState,
    trace::{
        WorkspaceTraceFinish, WorkspaceTraceTarget, invalid_arguments_outcome,
        trace_workspace_tool_finish, trace_workspace_tool_start,
    },
};

mod apply;
mod parse;
mod plan;
mod types;

#[cfg(test)]
pub(crate) use plan::workspace_patch_blocking;
pub(crate) use plan::{propose_workspace_patch_blocking_checked, workspace_patch_blocking_checked};
#[cfg(test)]
pub(crate) use types::stable_content_fingerprint;

#[derive(Debug)]
pub(crate) struct WorkspacePatchExecutor {
    pub(crate) state: Arc<WorkspaceToolState>,
}

impl ToolExecutor for WorkspacePatchExecutor {
    fn propose<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolActionProposalFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let args = match parse_workspace_patch_args(&call) {
                Ok(args) => args,
                Err(message) => {
                    return Ok(ToolActionPreflight::Outcome(failed_outcome(
                        WORKSPACE_PATCH_TOOL,
                        ERROR_INVALID_ARGUMENTS,
                        message,
                        None::<String>,
                    )));
                }
            };

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let worker_token = token.clone();
            let handle = tokio::task::spawn_blocking(move || {
                let is_cancelled = || worker_token.is_cancelled();
                propose_workspace_patch_blocking_checked(&state, args, &call, &is_cancelled)
            });

            tokio::select! {
                biased;
                () = token.cancelled() => Err(ToolExecutionError::Cancelled),
                joined = handle => match joined {
                    Ok(Ok(proposal)) => {
                        if token.is_cancelled() {
                            Err(ToolExecutionError::Cancelled)
                        } else {
                            Ok(proposal)
                        }
                    }
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(ToolExecutionError::infrastructure(format!(
                        "workspace patch proposal task failed to join: {error}"
                    ))),
                },
            }
        })
    }

    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            if context.cancellation_token().is_cancelled() {
                return Err(ToolExecutionError::Cancelled);
            }

            let args = match parse_workspace_patch_args(&call) {
                Ok(args) => args,
                Err(message) => {
                    return Ok(invalid_arguments_outcome(
                        WORKSPACE_PATCH_TOOL,
                        call.id().as_str(),
                        message,
                    ));
                }
            };
            let trace_patch_bytes = args.patch.len();
            trace_workspace_tool_start(
                WORKSPACE_PATCH_TOOL,
                call.id().as_str(),
                WorkspaceTraceTarget::Patch {
                    patch_bytes: trace_patch_bytes,
                },
            );

            let state = Arc::clone(&self.state);
            let token = context.cancellation_token().clone();
            let worker_token = token.clone();
            let approved_proposal = context.approved_workspace_patch().cloned();
            let handle = tokio::task::spawn_blocking(move || {
                let is_cancelled = || worker_token.is_cancelled();
                workspace_patch_blocking_checked(
                    &state,
                    args,
                    approved_proposal.as_ref(),
                    &is_cancelled,
                )
            });

            tokio::select! {
                biased;
                joined = handle => match joined {
                    Ok(Ok(outcome)) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_PATCH_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Patch {
                                patch_bytes: trace_patch_bytes,
                            },
                            WorkspaceTraceFinish::Outcome(&outcome),
                        );
                        Ok(outcome)
                    }
                    Ok(Err(error)) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_PATCH_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Patch {
                                patch_bytes: trace_patch_bytes,
                            },
                            WorkspaceTraceFinish::from_error(&error),
                        );
                        Err(error)
                    }
                    Err(error) => {
                        trace_workspace_tool_finish(
                            WORKSPACE_PATCH_TOOL,
                            call.id().as_str(),
                            WorkspaceTraceTarget::Patch {
                                patch_bytes: trace_patch_bytes,
                            },
                            WorkspaceTraceFinish::Error("workspace_infrastructure_error"),
                        );
                        Err(ToolExecutionError::infrastructure(format!(
                            "workspace patch task failed to join: {error}"
                        )))
                    }
                },
            }
        })
    }
}
