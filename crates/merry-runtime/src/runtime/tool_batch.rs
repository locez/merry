use super::{RuntimeInner, tool_execution};
use crate::{RuntimeError, ToolConcurrency, ToolExecutionContext};
use futures_util::{StreamExt, stream};
use merry_core::{PendingToolCall, RuntimeJournalEvent};
use std::sync::Arc;
use tokio::sync::mpsc;

pub(crate) struct ToolBatchExecution {
    events: Vec<RuntimeJournalEvent>,
    error: Option<RuntimeError>,
}

impl ToolBatchExecution {
    pub(crate) fn events(&self) -> &[RuntimeJournalEvent] {
        &self.events
    }

    pub(crate) fn is_successful(&self) -> bool {
        self.error.is_none()
    }

    pub(crate) fn into_parts(self) -> (Vec<RuntimeJournalEvent>, Option<RuntimeError>) {
        (self.events, self.error)
    }
}

pub(super) async fn execute_tool_call_batch_with_active_permit(
    inner: &Arc<RuntimeInner>,
    calls: Vec<PendingToolCall>,
    context: ToolExecutionContext,
) -> ToolBatchExecution {
    {
        let _batch_scope = inner.tools.begin_batch();
        execute_tool_call_batch_inner(inner, calls, context, None).await
    }
}

pub(super) async fn execute_tool_call_batch_streaming_with_active_permit(
    inner: &Arc<RuntimeInner>,
    calls: Vec<PendingToolCall>,
    context: ToolExecutionContext,
    event_tx: mpsc::Sender<Vec<RuntimeJournalEvent>>,
) -> Option<RuntimeError> {
    let execution = {
        let _batch_scope = inner.tools.begin_batch();
        execute_tool_call_batch_inner(inner, calls, context, Some(event_tx)).await
    };
    execution.error
}

async fn execute_tool_call_batch_inner(
    inner: &Arc<RuntimeInner>,
    calls: Vec<PendingToolCall>,
    context: ToolExecutionContext,
    event_tx: Option<mpsc::Sender<Vec<RuntimeJournalEvent>>>,
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

            let mut running = stream::iter(calls[start..cursor].iter().cloned().enumerate())
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
                .buffer_unordered(inner.tools.max_parallel_calls());

            let mut first_error: Option<(usize, RuntimeError)> = None;
            while let Some((index, result)) = running.next().await {
                match result {
                    Ok(mut call_events) => {
                        if let Some(tx) = &event_tx {
                            inner
                                .project_journal_events_with_tool_outputs(&call_events)
                                .await;
                            let _ = tx.send(call_events).await;
                        } else {
                            events.append(&mut call_events);
                        }
                    }
                    Err(error) => {
                        let replace = first_error
                            .as_ref()
                            .is_none_or(|(best_index, _)| index < *best_index);
                        if replace {
                            first_error = Some((index, error));
                        }
                    }
                }
            }
            if let Some((_, error)) = first_error {
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
                Ok(mut call_events) => {
                    if let Some(tx) = &event_tx {
                        inner
                            .project_journal_events_with_tool_outputs(&call_events)
                            .await;
                        let _ = tx.send(call_events).await;
                    } else {
                        events.append(&mut call_events);
                    }
                }
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
        .tools
        .registry
        .registered_tool(call.name())
        .map_or(ToolConcurrency::Exclusive, |tool| tool.concurrency())
}
