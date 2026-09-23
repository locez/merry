use super::{RuntimeInner, diagnostic_from_text};
use crate::{
    events::RuntimeJournalEventBatch,
    session::{ModelTurnId, ModelTurnStatus, SessionState},
    tool_input_validation::ToolInputValidationError,
};
use merry_core::{
    CompactionUsageWindow, ErrorInfo, ModelUsage, PendingToolCall, RuntimeJournalEvent,
    RuntimeJournalPayload, ToolCallResultStatus, UsageContextWindow,
};
use tokio::sync::watch;
use tokio::sync::{mpsc, mpsc::Permit};
use tokio_util::sync::CancellationToken;

pub(super) async fn send_assistant_text_output_completed_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    turn_id: ModelTurnId,
    text: String,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let events = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session
            .record_assistant_text_output(turn_id, text)
            .and_then(|event| {
                session.close_model_response(turn_id, false)?;
                let completed = session.record_step_completed();
                Ok((event, completed))
            })
    };

    let Ok((artifact_event, completed_event)) = events else {
        drop(permit);
        abort_model_turn_before_terminal_event(inner, turn_id).await;
        let diagnostic = diagnostic_from_text(
            "assistant_output_artifact",
            "assistant output artifact or model turn could not be recorded",
        );
        return send_failed_event(inner, sender, token, diagnostic).await;
    };
    inner
        .emit_journal_batch_after_savepoint(
            permit,
            RuntimeJournalEventBatch::pair(artifact_event, completed_event),
        )
        .await;
    true
}

pub(super) async fn send_assistant_text_output_delta_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    delta: String,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_transient_event(RuntimeJournalPayload::AssistantOutputDelta { delta })
    };

    inner.emit_journal_batch(permit, event.clone().into());
    true
}

pub(super) async fn send_model_tool_call_response_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    turn_id: ModelTurnId,
    commentary: Option<String>,
    calls: Vec<PendingToolCall>,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let events = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_model_tool_call_response(turn_id, commentary, calls)
    };

    match events {
        Ok((commentary_event, tool_event)) => {
            let bridge_calls = match &tool_event.payload {
                RuntimeJournalPayload::ToolCallPending { call } => {
                    if inner
                        .tool_registry
                        .registered_tool(call.name())
                        .is_some_and(|tool| tool.runner() == crate::ToolRunner::Bridge)
                    {
                        vec![call.clone()]
                    } else {
                        Vec::new()
                    }
                }
                RuntimeJournalPayload::ToolCallBatchPending { batch } => batch
                    .calls()
                    .iter()
                    .filter(|call| {
                        inner
                            .tool_registry
                            .registered_tool(call.name())
                            .is_some_and(|tool| tool.runner() == crate::ToolRunner::Bridge)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            let batch = match commentary_event {
                Some(commentary_event) => {
                    RuntimeJournalEventBatch::pair(commentary_event, tool_event)
                }
                None => tool_event.into(),
            };
            inner.emit_journal_batch(permit, batch);

            if token.is_cancelled() {
                return false;
            }

            for call in bridge_calls {
                if let Some(Err(error)) = inner.tool_registry.validate_tool_input(&call) {
                    if !send_bridge_tool_input_validation_failure_events(
                        inner, sender, token, &call, error,
                    )
                    .await
                    {
                        return false;
                    }
                } else if !send_bridge_tool_call_requested_event(inner, sender, token, call).await {
                    return false;
                }
            }
            true
        }
        Err(diagnostic) => {
            drop(permit);
            abort_model_turn_before_terminal_event(inner, turn_id).await;
            send_failed_event(inner, sender, token, diagnostic).await
        }
    }
}

async fn abort_model_turn_before_terminal_event(inner: &RuntimeInner, turn_id: ModelTurnId) {
    let mut session = inner.session.lock().await;
    let result = if session.model_turn_status(turn_id) == Some(ModelTurnStatus::InProgress) {
        session.abort_model_turn(turn_id)
    } else {
        Ok(())
    };
    if let Err(error) = result {
        tracing::error!(
            category = "model_turn_abort",
            model_turn_id = turn_id.as_u64(),
            error = %error,
            "failed to abort model turn before terminal journal event"
        );
    }
}

pub(super) async fn send_model_usage_updated_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    model_usage: ModelUsage,
    context: Option<UsageContextWindow>,
    compaction: Option<CompactionUsageWindow>,
    observation: Option<crate::token_estimate::RequestTokenObservation>,
) -> Result<bool, ErrorInfo> {
    if token.is_cancelled() {
        return Ok(false);
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return Ok(false);
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return Ok(false);
        }
        let event = session.record_model_usage(model_usage, context, compaction)?;
        if let Some(observation) = observation {
            session.calibrate_request_tokens(observation, model_usage);
        }
        event
    };

    inner.emit_journal_batch(permit, event.clone().into());
    Ok(true)
}

/// Whether contention may discard an intermediate sample or must preserve a boundary.
pub(super) enum RateEventDelivery {
    Replaceable,
    Boundary,
}

/// Sends a boundary or best-effort sample through the latest-only rate channel;
/// false means cancellation or receiver closure. Intermediate replaceable
/// samples never wait behind the semantic event queue.
pub(super) async fn send_model_output_rate_event(
    inner: &RuntimeInner,
    rate_sender: &watch::Sender<Option<RuntimeJournalEvent>>,
    token: &CancellationToken,
    rate: Option<merry_core::ModelOutputRate>,
    delivery: RateEventDelivery,
) -> bool {
    if token.is_cancelled() {
        return false;
    }
    if rate_sender.is_closed() {
        return matches!(delivery, RateEventDelivery::Replaceable);
    }
    let event = {
        let mut session = match delivery {
            RateEventDelivery::Replaceable => {
                let Ok(session) = inner.session.try_lock() else {
                    return true;
                };
                session
            }
            RateEventDelivery::Boundary => inner.session.lock().await,
        };
        if token.is_cancelled() {
            return false;
        }
        session.record_transient_event(RuntimeJournalPayload::ModelOutputRateUpdated { rate })
    };
    rate_sender.send_replace(Some(event));
    true
}

pub(super) async fn send_compaction_started_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    rate_sender: &watch::Sender<Option<RuntimeJournalEvent>>,
    token: &CancellationToken,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let (event, reset) = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        let reset = session
            .record_transient_event(RuntimeJournalPayload::ModelOutputRateUpdated { rate: None });
        (session.record_compaction_started(), reset)
    };

    inner.emit_journal_batch(permit, event.into());
    rate_sender.send_replace(Some(reset));
    true
}

pub(super) async fn send_compaction_completed_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    checkpoint_id: String,
    covered_history_item_count: usize,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_compaction_completed(checkpoint_id, covered_history_item_count)
    };

    inner.emit_journal_batch(permit, event.into());
    true
}

async fn send_bridge_tool_call_requested_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    call: PendingToolCall,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.record_bridge_tool_call_requested(call)
    };

    inner.emit_journal_batch(permit, event.into());
    true
}

async fn send_bridge_tool_input_validation_failure_events(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    call: &PendingToolCall,
    error: ToolInputValidationError,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let result = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        session.submit_tool_execution_outcome(
            call.id(),
            ToolCallResultStatus::Failed,
            error.content_for_call(call),
            Some(error.diagnostic()),
            None,
        )
    };

    match result {
        Ok(events) => {
            let Some(batch) = RuntimeJournalEventBatch::from_events(events) else {
                drop(permit);
                let diagnostic = diagnostic_from_text(
                    "tool_input_validation_events",
                    "validated bridge tool outcome produced no journal events",
                );
                return send_failed_event(inner, sender, token, diagnostic).await;
            };
            inner
                .emit_journal_batch_after_savepoint(permit, batch)
                .await;
            true
        }
        Err(error) => {
            drop(permit);
            let diagnostic =
                diagnostic_from_text("tool_input_validation_result", error.to_string());
            send_failed_event(inner, sender, token, diagnostic).await
        }
    }
}

pub(super) async fn send_failed_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    diagnostic: ErrorInfo,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    send_normal_event(inner, sender, token, |session| {
        Some(session.record_failed(diagnostic))
    })
    .await
}

pub(super) fn trace_provider_step_failed(diagnostic: &ErrorInfo) {
    tracing::debug!(
        category = "failed",
        diagnostic_code = diagnostic.code(),
        "runtime provider step failed"
    );
}

pub(super) fn trace_provider_step_cancelled() {
    tracing::debug!(
        category = "cancelled",
        diagnostic_code = "cancelled",
        "runtime provider step cancelled"
    );
}

pub(super) async fn send_normal_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
    make_event: impl FnOnce(&mut SessionState) -> Option<RuntimeJournalEvent>,
) -> bool {
    if token.is_cancelled() {
        return false;
    }

    let Some(permit) = reserve_normal_event_slot(sender, token).await else {
        return false;
    };

    let event = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return false;
        }
        make_event(&mut session)
    };

    if let Some(event) = event {
        inner.emit_journal_batch(permit, event.into());
    }

    true
}

pub(super) async fn send_step_started_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    rate_sender: &watch::Sender<Option<RuntimeJournalEvent>>,
    token: &CancellationToken,
) -> Option<RuntimeJournalEvent> {
    if token.is_cancelled() {
        return None;
    }

    let permit = reserve_normal_event_slot(sender, token).await?;
    let (event, reset) = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            return None;
        }
        let event = session.record_step_started();
        let reset =
            if inner
                .model_configs
                .contains_role(crate::RuntimeModelRole::Primary)
            {
                Some(session.record_transient_event(
                    RuntimeJournalPayload::ModelOutputRateUpdated { rate: None },
                ))
            } else {
                None
            };
        (event, reset)
    };
    inner.emit_journal_batch(permit, event.clone().into());
    if let Some(reset) = reset {
        rate_sender.send_replace(Some(reset));
    }
    Some(event)
}

pub(super) async fn reserve_normal_event_slot<'a>(
    sender: &'a mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
) -> Option<Permit<'a, RuntimeJournalEventBatch>> {
    if token.is_cancelled() || sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = token.cancelled() => None,
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}

pub(super) async fn send_cancelled_if_requested(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    token: &CancellationToken,
) -> bool {
    if !token.is_cancelled() {
        return false;
    }

    send_cancelled_event(inner, sender).await
}

pub(super) async fn send_cancelled_event(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
) -> bool {
    let Some(permit) = reserve_cancelled_event_slot(sender).await else {
        return false;
    };

    if sender.is_closed() {
        return false;
    }

    let diagnostic = ErrorInfo::new("cancelled", "runtime step cancelled")
        .expect("static cancellation diagnostic is valid");
    let event = {
        let mut session = inner.session.lock().await;
        session.record_cancelled(diagnostic)
    };
    inner.emit_journal_batch(permit, event.into());
    true
}

async fn reserve_cancelled_event_slot<'a>(
    sender: &'a mpsc::Sender<RuntimeJournalEventBatch>,
) -> Option<Permit<'a, RuntimeJournalEventBatch>> {
    reserve_event_slot_ignoring_cancellation(sender).await
}

async fn reserve_event_slot_ignoring_cancellation<'a>(
    sender: &'a mpsc::Sender<RuntimeJournalEventBatch>,
) -> Option<Permit<'a, RuntimeJournalEventBatch>> {
    if sender.is_closed() {
        return None;
    }

    tokio::select! {
        biased;
        () = sender.closed() => None,
        permit = sender.reserve() => permit.ok(),
    }
}
