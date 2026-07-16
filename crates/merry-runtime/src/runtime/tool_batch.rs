use super::{RuntimeInner, persist_resume_safe_savepoint_if_configured, tool_execution};
use crate::{RuntimeError, ToolConcurrency, ToolExecutionContext};
use futures_util::{StreamExt, stream};
use merry_core::{PendingToolCall, RuntimeJournalEvent};
use std::sync::Arc;

pub(crate) struct ToolBatchExecution {
    events: Vec<RuntimeJournalEvent>,
    error: Option<RuntimeError>,
}

impl ToolBatchExecution {
    pub(crate) fn into_parts(self) -> (Vec<RuntimeJournalEvent>, Option<RuntimeError>) {
        (self.events, self.error)
    }
}

pub(super) async fn execute_tool_call_batch_with_active_permit(
    inner: &Arc<RuntimeInner>,
    calls: Vec<PendingToolCall>,
    context: ToolExecutionContext,
) -> ToolBatchExecution {
    let execution = {
        let _batch_scope = inner.begin_tool_batch();
        execute_tool_call_batch_inner(inner, calls, context).await
    };

    // Individual tool paths update in-memory state as their work completes,
    // but a resume-safe session snapshot is only valid after the whole batch
    // has resolved. Infrastructure failures leave pending calls for the
    // interactive cleanup path, so it owns the next safe savepoint.
    if execution.error.is_none() {
        persist_resume_safe_savepoint_if_configured(inner).await;
    }
    execution
}

async fn execute_tool_call_batch_inner(
    inner: &Arc<RuntimeInner>,
    calls: Vec<PendingToolCall>,
    context: ToolExecutionContext,
) -> ToolBatchExecution {
    let mut events = Vec::new();
    let mut cursor = 0;

    while cursor < calls.len() {
        if tool_concurrency(inner, &calls[cursor]) == ToolConcurrency::ParallelSafe {
            let start = cursor;
            while cursor < calls.len()
                && tool_concurrency(inner, &calls[cursor]) == ToolConcurrency::ParallelSafe
            {
                cursor += 1;
            }

            let mut results = stream::iter(calls[start..cursor].iter().cloned().enumerate())
                .map(|(index, call)| {
                    let inner = Arc::clone(inner);
                    let context = context.clone();
                    async move {
                        let result = tool_execution::execute_tool_call_with_active_permit(
                            &inner,
                            call.id(),
                            context,
                        )
                        .await;
                        (index, result)
                    }
                })
                .buffer_unordered(inner.max_parallel_tool_calls.get())
                .collect::<Vec<_>>()
                .await;
            results.sort_by_key(|(index, _)| *index);

            let mut first_error = None;
            for (_, result) in results {
                match result {
                    Ok(mut call_events) => events.append(&mut call_events),
                    Err(error) if first_error.is_none() => first_error = Some(error),
                    Err(_) => {}
                }
            }
            if let Some(error) = first_error {
                events.sort_by_key(|event| event.sequence);
                return ToolBatchExecution {
                    events,
                    error: Some(error),
                };
            }
        } else {
            let result = tool_execution::execute_tool_call_with_active_permit(
                inner,
                calls[cursor].id(),
                context.clone(),
            )
            .await;
            cursor += 1;
            match result {
                Ok(mut call_events) => events.append(&mut call_events),
                Err(error) => {
                    events.sort_by_key(|event| event.sequence);
                    return ToolBatchExecution {
                        events,
                        error: Some(error),
                    };
                }
            }
        }
    }

    events.sort_by_key(|event| event.sequence);
    ToolBatchExecution {
        events,
        error: None,
    }
}

fn tool_concurrency(inner: &RuntimeInner, call: &PendingToolCall) -> ToolConcurrency {
    inner
        .tool_registry
        .registered_tool(call.name())
        .map_or(ToolConcurrency::Exclusive, |tool| tool.concurrency())
}
