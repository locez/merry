//! Pending-tool classification and bridge/runtime tool-wave execution.

use super::{AgentLoopBlockedReason, AgentRunMessage, publish_journal_event};
use crate::{
    FinalOutputContract, HookPoint, Runtime, RuntimeError, ToolExecutionContext,
    bridge::{
        BridgeToolResultCommand, receive_bridge_tool_result, validate_bridge_tool_result_command,
    },
    events::{ActiveStepPermit, RuntimeEventProjector},
    hooks::{
        bridge::{StreamHookBridge, has_after_tool_hook},
        wave::{
            HookWaveContext, PostBridgeWaveOutcome, PostRuntimeToolOutcome, PreToolWaveOutcome,
            run_post_bridge_tool_hooks, run_post_runtime_tool_hooks, run_pre_tool_wave_hooks,
        },
    },
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

/// Non-error early stop requested by a tool-level hook during a wave.
pub(crate) struct ToolWaveStop {
    pub(crate) hook_point: HookPoint,
    pub(crate) reason: String,
    pub(crate) final_output: Option<String>,
}

/// Outcome of executing one runtime or bridge tool wave with pre/post hooks.
pub(crate) enum ToolWaveOutcome {
    Continue,
    StoppedByHook(ToolWaveStop),
    RuntimeError(RuntimeError),
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_stream_runtime_batch(
    runtime: &Runtime,
    calls: Vec<PendingToolCall>,
    model_turn: usize,
    token: &tokio_util::sync::CancellationToken,
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    mut hook_bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<ToolWaveOutcome, RuntimeError> {
    if calls.is_empty() {
        return Ok(ToolWaveOutcome::Continue);
    }

    let wave = HookWaveContext {
        runtime,
        model_turn,
        token,
        loop_permit,
    };
    let allowed_calls =
        match run_pre_tool_wave_hooks(&wave, calls, false, hook_bridge.as_deref_mut()).await? {
            PreToolWaveOutcome::Proceed {
                pre_events,
                allowed_calls,
            } => {
                for event in pre_events {
                    publish_journal_event(runtime, projector, sender, events, event).await?;
                }
                allowed_calls
            }
            PreToolWaveOutcome::StoppedByHook {
                pre_events,
                reason,
                final_output,
            } => {
                for event in pre_events {
                    publish_journal_event(runtime, projector, sender, events, event).await?;
                }
                return Ok(ToolWaveOutcome::StoppedByHook(ToolWaveStop {
                    hook_point: HookPoint::BeforeToolCall,
                    reason,
                    final_output,
                }));
            }
        };

    if allowed_calls.is_empty() {
        return Ok(ToolWaveOutcome::Continue);
    }

    let bridge_enabled = hook_bridge.is_some();
    let needs_post_hooks = allowed_calls
        .iter()
        .any(|call| has_after_tool_hook(runtime, call.name(), bridge_enabled));

    if !needs_post_hooks {
        let (batch_events_tx, mut batch_events_rx) = mpsc::channel(64);
        let execution = runtime.execute_tool_call_batch_streaming_with_active_permit(
            allowed_calls,
            ToolExecutionContext::new(token.clone()),
            batch_events_tx,
            loop_permit,
        );
        tokio::pin!(execution);
        let mut batch_events_open = true;

        let error = loop {
            tokio::select! {
                error = &mut execution => break error,
                call_events = batch_events_rx.recv(), if batch_events_open => {
                    let Some(call_events) = call_events else {
                        batch_events_open = false;
                        continue;
                    };
                    for event in call_events {
                        publish_journal_event(runtime, projector, sender, events, event).await?;
                    }
                }
            }
        };
        while let Ok(call_events) = batch_events_rx.try_recv() {
            for event in call_events {
                publish_journal_event(runtime, projector, sender, events, event).await?;
            }
        }
        return Ok(match error {
            None => ToolWaveOutcome::Continue,
            Some(err) => ToolWaveOutcome::RuntimeError(err),
        });
    }

    let execution = runtime
        .execute_tool_call_batch_with_active_permit(
            allowed_calls.clone(),
            ToolExecutionContext::new(token.clone()),
            loop_permit,
        )
        .await;
    let (batch_events, error) = execution.into_parts();
    let post_outcome =
        run_post_runtime_tool_hooks(&wave, &allowed_calls, &batch_events, hook_bridge).await?;
    for event in batch_events {
        publish_journal_event(runtime, projector, sender, events, event).await?;
    }
    if let PostRuntimeToolOutcome::StoppedByHook {
        reason,
        final_output,
    } = post_outcome
    {
        return Ok(ToolWaveOutcome::StoppedByHook(ToolWaveStop {
            hook_point: HookPoint::AfterToolCall,
            reason,
            final_output,
        }));
    }
    Ok(match error {
        None => ToolWaveOutcome::Continue,
        Some(err) => ToolWaveOutcome::RuntimeError(err),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn receive_and_publish_bridge_tool_results(
    runtime: &Runtime,
    calls: Vec<PendingToolCall>,
    batch_id: ToolCallBatchId,
    model_turn: usize,
    bridge_resolution_epoch: &AtomicU64,
    token: &tokio_util::sync::CancellationToken,
    loop_permit: &ActiveStepPermit,
    receiver: &mut mpsc::Receiver<BridgeToolResultCommand>,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    mut hook_bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<ToolWaveOutcome, RuntimeError> {
    let wave = HookWaveContext {
        runtime,
        model_turn,
        token,
        loop_permit,
    };
    let allowed_calls =
        match run_pre_tool_wave_hooks(&wave, calls, true, hook_bridge.as_deref_mut()).await? {
            PreToolWaveOutcome::Proceed {
                pre_events,
                allowed_calls,
            } => {
                for event in pre_events {
                    publish_journal_event(runtime, projector, sender, events, event).await?;
                }
                allowed_calls
            }
            PreToolWaveOutcome::StoppedByHook {
                pre_events,
                reason,
                final_output,
            } => {
                for event in pre_events {
                    publish_journal_event(runtime, projector, sender, events, event).await?;
                }
                return Ok(ToolWaveOutcome::StoppedByHook(ToolWaveStop {
                    hook_point: HookPoint::BeforeToolCall,
                    reason,
                    final_output,
                }));
            }
        };

    if allowed_calls.is_empty() {
        return Ok(ToolWaveOutcome::Continue);
    }

    let batch = PendingToolCallBatch::new(batch_id, allowed_calls).map_err(RuntimeError::from)?;
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

    let validated_outcomes = loop {
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
            validate_bridge_tool_result_command(runtime, &batch, command, loop_permit).await;
        match result {
            Ok(outcomes) => {
                // Release the host batch lease BEFORE invoking any bridged `after_tool_call`
                // hooks so the host can return from `submit_tool_results` and read the next
                // stream message without deadlocking.
                bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                let _ = ack_sender.send(Ok(()));
                break outcomes;
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
                return Ok(ToolWaveOutcome::Continue);
            }
        }
    };

    match run_post_bridge_tool_hooks(
        runtime,
        batch.calls(),
        validated_outcomes,
        model_turn,
        token,
        hook_bridge,
    )
    .await?
    {
        PostBridgeWaveOutcome::Continue(final_outcomes) => {
            let result_events = runtime
                .submit_tool_execution_outcomes_with_active_permit(final_outcomes, loop_permit)
                .await?;
            for event in result_events {
                publish_journal_event(runtime, projector, sender, events, event).await?;
            }
            Ok(ToolWaveOutcome::Continue)
        }
        PostBridgeWaveOutcome::StoppedByHook {
            outcomes_to_commit,
            reason,
            final_output,
        } => {
            if !outcomes_to_commit.is_empty() {
                let result_events = runtime
                    .submit_tool_execution_outcomes_with_active_permit(
                        outcomes_to_commit,
                        loop_permit,
                    )
                    .await?;
                for event in result_events {
                    publish_journal_event(runtime, projector, sender, events, event).await?;
                }
            }
            Ok(ToolWaveOutcome::StoppedByHook(ToolWaveStop {
                hook_point: HookPoint::AfterToolCall,
                reason,
                final_output,
            }))
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
