//! Pending-tool classification and bridge/runtime tool-wave execution.

use super::{AgentLoopBlockedReason, AgentRunMessage, publish_journal_event};
use crate::{
    FinalOutputContract, Runtime, RuntimeError, ToolExecutionContext,
    bridge::{
        BridgeToolResultCommand, receive_bridge_tool_result, resolve_bridge_tool_result_command,
    },
    events::{ActiveStepPermit, RuntimeEventProjector},
};
use merry_core::{
    ErrorInfo, PendingToolCall, PendingToolCallBatch, RuntimeJournalEvent, RuntimeJournalPayload,
    ToolCallBatchId, ToolCallId,
};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;

pub(crate) enum StepOutcome {
    Completed,
    Failed(ErrorInfo),
    Cancelled(ErrorInfo),
    ToolResultRecorded,
    Pending(PendingLoopToolCall),
    PendingBatch(Vec<PendingLoopToolCall>),
    Blocked(AgentLoopBlockedReason),
}

#[derive(Clone)]
pub(crate) enum PendingLoopToolCall {
    Runtime(PendingToolCall),
    Bridge(PendingToolCall),
    FinalOutput(PendingToolCall),
}

pub(crate) enum PendingLoopToolWave {
    Runtime(Vec<PendingToolCall>),
    Bridge(Vec<PendingToolCall>),
}

pub(crate) fn classify_step_events(
    events: &[RuntimeJournalEvent],
    final_output_contract: Option<&FinalOutputContract>,
) -> StepOutcome {
    let bridge_call_ids = events
        .iter()
        .filter_map(|event| match &event.payload {
            RuntimeJournalPayload::BridgeToolCallRequested { call } => Some(call.id().clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();

    let resolved_call_ids = events
        .iter()
        .filter_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result.call_id().clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let resolved_tool_result_recorded = !resolved_call_ids.is_empty();

    let mut pending = events
        .iter()
        .flat_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallPending { call }
                if !resolved_call_ids.contains(call.id()) =>
            {
                vec![call.clone()]
            }
            RuntimeJournalPayload::ToolCallBatchPending { batch } => batch
                .calls()
                .iter()
                .filter(|call| !resolved_call_ids.contains(call.id()))
                .cloned()
                .collect(),
            _ => Vec::new(),
        })
        .collect::<Vec<_>>();

    if let Some(diagnostic) = events.iter().rev().find_map(|event| match &event.payload {
        RuntimeJournalPayload::Failed { diagnostic } => Some(diagnostic.clone()),
        _ => None,
    }) {
        return StepOutcome::Failed(diagnostic);
    }

    if let Some(diagnostic) = events.iter().rev().find_map(|event| match &event.payload {
        RuntimeJournalPayload::Cancelled { diagnostic } => Some(diagnostic.clone()),
        _ => None,
    }) {
        return StepOutcome::Cancelled(diagnostic);
    }

    let completed = events
        .iter()
        .any(|event| matches!(event.payload, RuntimeJournalPayload::StepCompleted));

    if completed {
        if pending.is_empty() {
            if final_output_contract.is_some() {
                return StepOutcome::Blocked(AgentLoopBlockedReason::FinalOutputToolNotCalled);
            }
            return StepOutcome::Completed;
        }

        return StepOutcome::Blocked(AgentLoopBlockedReason::StepCompletedWithPendingToolCall {
            pending_count: pending.len(),
        });
    }

    match pending.len() {
        0 if resolved_tool_result_recorded => StepOutcome::ToolResultRecorded,
        0 => StepOutcome::Blocked(AgentLoopBlockedReason::StepEndedWithoutTerminalEvent),
        1 => {
            let call = pending.pop().expect("one pending call is present");
            StepOutcome::Pending(classify_pending_tool_call(
                call,
                &bridge_call_ids,
                final_output_contract,
            ))
        }
        _ => StepOutcome::PendingBatch(
            pending
                .into_iter()
                .map(|call| {
                    classify_pending_tool_call(call, &bridge_call_ids, final_output_contract)
                })
                .collect(),
        ),
    }
}

pub(crate) fn classify_pending_tool_call(
    call: PendingToolCall,
    bridge_call_ids: &BTreeSet<ToolCallId>,
    final_output_contract: Option<&FinalOutputContract>,
) -> PendingLoopToolCall {
    if final_output_contract.is_some_and(|contract| call.name() == contract.tool_name()) {
        PendingLoopToolCall::FinalOutput(call)
    } else if bridge_call_ids.contains(call.id()) {
        PendingLoopToolCall::Bridge(call)
    } else {
        PendingLoopToolCall::Runtime(call)
    }
}

pub(crate) async fn execute_stream_runtime_batch(
    runtime: &Runtime,
    calls: Vec<PendingToolCall>,
    token: &tokio_util::sync::CancellationToken,
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
) -> Result<Option<RuntimeError>, RuntimeError> {
    if calls.is_empty() {
        return Ok(None);
    }

    let execution = runtime
        .execute_tool_call_batch_with_active_permit(
            calls,
            ToolExecutionContext::new(token.clone()),
            loop_permit,
        )
        .await;
    let (execution_events, error) = execution.into_parts();
    for event in execution_events {
        publish_journal_event(runtime, projector, sender, events, event).await?;
    }
    Ok(error)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn receive_and_publish_bridge_tool_results(
    runtime: &Runtime,
    calls: Vec<PendingToolCall>,
    batch_id: ToolCallBatchId,
    bridge_resolution_epoch: &AtomicU64,
    token: &tokio_util::sync::CancellationToken,
    loop_permit: &ActiveStepPermit,
    receiver: &mut mpsc::Receiver<BridgeToolResultCommand>,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
) -> Result<(), RuntimeError> {
    let batch = PendingToolCallBatch::new(batch_id, calls).map_err(RuntimeError::from)?;
    let request = AgentRunMessage::ToolInvocations {
        batch: batch.clone(),
    };
    sender
        .send(request)
        .await
        .map_err(|_| RuntimeError::AgentRunClosed {
            session_id: runtime.session_id().clone(),
            message: "bridge invocation receiver closed before the request was delivered",
        })?;

    loop {
        let command = match receive_bridge_tool_result(receiver, token).await {
            Some(command) => command,
            None if token.is_cancelled() => {
                let Some(first_call) = batch.calls().first() else {
                    return Err(RuntimeError::BridgeToolResultBatchEmpty {
                        session_id: runtime.session_id().clone(),
                    });
                };
                let settlement = settle_cancelled_bridge_tool_calls(
                    runtime,
                    batch.calls(),
                    loop_permit,
                    projector,
                    sender,
                    events,
                )
                .await;
                bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                settlement?;
                return Err(RuntimeError::ToolExecutionCancelled {
                    session_id: runtime.session_id().clone(),
                    call_id: first_call.id().clone(),
                });
            }
            None => {
                return Err(RuntimeError::AgentRunClosed {
                    session_id: runtime.session_id().clone(),
                    message: "bridge tool result channel closed before invocations were resolved",
                });
            }
        };
        let (ack_sender, result) =
            resolve_bridge_tool_result_command(runtime, &batch, command, loop_permit).await;
        match result {
            Ok(result_events) => {
                bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                let _ = ack_sender.send(Ok(()));
                for event in result_events {
                    publish_journal_event(runtime, projector, sender, events, event).await?;
                }
                return Ok(());
            }
            Err(error) if error.is_retryable_bridge_tool_result() => {
                let _ = ack_sender.send(Err(error));
            }
            Err(error) => {
                let message = error.to_string();
                let _ = ack_sender.send(Err(RuntimeError::BridgeToolResultRejected {
                    session_id: runtime.session_id().clone(),
                    message: message.clone(),
                }));
                settle_failed_bridge_tool_calls(
                    runtime,
                    batch.calls(),
                    loop_permit,
                    projector,
                    sender,
                    events,
                    &message,
                )
                .await?;
                return Ok(());
            }
        }
    }
}

pub(crate) async fn settle_cancelled_bridge_tool_calls(
    runtime: &Runtime,
    calls: &[PendingToolCall],
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
) -> Result<(), RuntimeError> {
    let pending_ids = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .map(|call| call.id().clone())
        .collect::<BTreeSet<_>>();

    for call in calls.iter().filter(|call| pending_ids.contains(call.id())) {
        let failure_events = runtime
            .submit_tool_abandoned_failure_with_active_permit(
                call.id(),
                "agent run cancelled before the bridge tool result was submitted",
                loop_permit,
            )
            .await?;
        for event in failure_events {
            publish_journal_event(runtime, projector, sender, events, event).await?;
        }
    }

    Ok(())
}

pub(crate) async fn settle_failed_bridge_tool_calls(
    runtime: &Runtime,
    calls: &[PendingToolCall],
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    message: &str,
) -> Result<(), RuntimeError> {
    let pending_ids = runtime
        .pending_tool_calls()
        .await
        .into_iter()
        .map(|call| call.id().clone())
        .collect::<BTreeSet<_>>();

    for call in calls.iter().filter(|call| pending_ids.contains(call.id())) {
        let failure_events = runtime
            .submit_tool_execution_failure_with_active_permit(call.id(), message, loop_permit)
            .await?;
        for event in failure_events {
            publish_journal_event(runtime, projector, sender, events, event).await?;
        }
    }

    Ok(())
}

pub(crate) fn next_agent_run_batch_id(sequence: &mut u64) -> Result<ToolCallBatchId, RuntimeError> {
    let batch_id =
        ToolCallBatchId::new(&format!("agent-run-batch-{sequence}")).map_err(RuntimeError::from)?;
    *sequence = (*sequence).saturating_add(1);
    Ok(batch_id)
}
