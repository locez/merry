//! Consumes one model turn, reducing streamed output and committing its terminal result.

use super::super::{
    RuntimeInner, diagnostic_from_text,
    journal_emission::{
        RateEventDelivery, send_assistant_text_output_completed_events,
        send_assistant_text_output_delta_event, send_model_output_rate_event,
        send_model_tool_call_response_events, send_model_usage_updated_event,
    },
    model_output::{
        DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT, diagnostic_from_model_error,
        is_cancelled_model_error, pending_tool_call_from_model, pending_tool_calls_from_outputs,
        record_streamed_tool_call, tool_call_commentary_text,
    },
    model_turn_lifecycle::{cancel_model_turn, fail_model_turn},
    output_rate::OutputRateTracker,
    provider_request::StepUsageContextSnapshot,
    provider_stream::wait_for_model_stream_item,
};
use crate::{
    events::{RuntimeJournalEventBatch, RuntimeRateUpdateSender},
    session::ModelTurnId,
    token_estimate::RequestTokenObservation,
};
use merry_core::PendingToolCall;
use merry_llm::{FinishReason, ModelEvent, ModelEventStream, ModelOutput, ModelRetryEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) struct ModelStreamRun {
    pub(super) stream: ModelEventStream,
    pub(super) retry_event_receiver: mpsc::Receiver<ModelRetryEvent>,
    pub(super) turn_id: ModelTurnId,
    pub(super) usage_context_snapshot: StepUsageContextSnapshot,
    pub(super) request_token_observation: RequestTokenObservation,
    pub(super) final_output_contract: Option<crate::FinalOutputContract>,
}

pub(super) async fn consume_model_stream(
    inner: &RuntimeInner,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    rate_sender: &RuntimeRateUpdateSender,
    token: &CancellationToken,
    run: ModelStreamRun,
) {
    let ModelStreamRun {
        mut stream,
        mut retry_event_receiver,
        turn_id,
        usage_context_snapshot,
        request_token_observation,
        final_output_contract,
    } = run;
    let mut commentary_text = String::new();
    let mut output_rate = OutputRateTracker::default();
    let mut streamed_tool_calls: Vec<PendingToolCall> = Vec::new();

    loop {
        let item = wait_for_model_stream_item(
            inner,
            sender,
            token,
            &mut stream,
            &mut retry_event_receiver,
        )
        .await;

        let item = match item {
            Some(item) => item,
            None => {
                cancel_model_turn(inner, sender, turn_id).await;
                return;
            }
        };

        match item {
            Some(Ok(ModelEvent::OutputProgress { progress })) => {
                let rate = output_rate.observe(progress);
                let delivery = if rate.is_some() {
                    RateEventDelivery::Replaceable
                } else {
                    RateEventDelivery::Boundary
                };
                if !send_model_output_rate_event(inner, rate_sender, token, rate, delivery).await {
                    cancel_model_turn(inner, sender, turn_id).await;
                    return;
                }
            }
            Some(Ok(ModelEvent::Started)) => {
                tracing::debug!(category = "started", "runtime model stream event received");
            }
            Some(Ok(ModelEvent::OutputTextDelta { delta })) => {
                if !delta.is_empty() {
                    tracing::trace!(
                        category = "output_text_delta_nonempty",
                        "runtime model stream event received"
                    );
                    commentary_text.push_str(&delta);
                    if !send_assistant_text_output_delta_event(inner, sender, token, delta).await {
                        cancel_model_turn(inner, sender, turn_id).await;
                        return;
                    }
                }
            }
            Some(Ok(ModelEvent::Completed { response })) => {
                if let Some(rate) = output_rate.rate(response.usage())
                    && !send_model_output_rate_event(
                        inner,
                        rate_sender,
                        token,
                        Some(rate),
                        RateEventDelivery::Boundary,
                    )
                    .await
                {
                    cancel_model_turn(inner, sender, turn_id).await;
                    return;
                }
                tracing::debug!(
                    category = "completed",
                    finish_reason = ?response.finish_reason(),
                    "runtime model stream event received"
                );
                if let Some(model_usage) = response.usage() {
                    match send_model_usage_updated_event(
                        inner,
                        sender,
                        token,
                        model_usage,
                        usage_context_snapshot.context,
                        usage_context_snapshot.compaction,
                        (response.finish_reason() != FinishReason::Cancelled)
                            .then_some(request_token_observation),
                    )
                    .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            cancel_model_turn(inner, sender, turn_id).await;
                            return;
                        }
                        Err(diagnostic) => {
                            fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                            return;
                        }
                    }
                }
                match response.finish_reason() {
                    FinishReason::Stop => {
                        if !streamed_tool_calls.is_empty() {
                            let diagnostic = diagnostic_from_text(
                                DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                                "model requested a tool call before completing with text output",
                            );
                            fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                            return;
                        }

                        let [ModelOutput::Text { text }] = response.outputs() else {
                            let diagnostic = diagnostic_from_text(
                                "model_output_unsupported",
                                "model stop output must contain exactly one text item",
                            );
                            fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                            return;
                        };

                        if !send_assistant_text_output_completed_events(
                            inner,
                            sender,
                            token,
                            turn_id,
                            text.clone(),
                        )
                        .await
                        {
                            cancel_model_turn(inner, sender, turn_id).await;
                        }
                        return;
                    }
                    FinishReason::ToolCalls => {
                        match pending_tool_calls_from_outputs(
                            response.outputs(),
                            &streamed_tool_calls,
                        ) {
                            Ok(calls) => {
                                if calls.len() > 1
                                    && final_output_contract.as_ref().is_some_and(|contract| {
                                        calls.iter().any(|call| call.name() == contract.tool_name())
                                    })
                                {
                                    let diagnostic = diagnostic_from_text(
                                        "final_output_tool_batch_mixed",
                                        "final-output tool calls must be the only call in their model batch",
                                    );
                                    fail_model_turn(inner, sender, token, turn_id, diagnostic)
                                        .await;
                                    return;
                                }
                                let commentary =
                                    tool_call_commentary_text(response.outputs(), &commentary_text);
                                let sent = send_model_tool_call_response_events(
                                    inner, sender, token, turn_id, commentary, calls,
                                )
                                .await;
                                if !sent {
                                    cancel_model_turn(inner, sender, turn_id).await;
                                }
                            }
                            Err(diagnostic) => {
                                fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                            }
                        }
                        return;
                    }
                    FinishReason::Length => {
                        let diagnostic = diagnostic_from_text(
                            "model_length",
                            "model output stopped because it reached a length limit",
                        );
                        fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                        return;
                    }
                    FinishReason::Blocked => {
                        let diagnostic = diagnostic_from_text(
                            "model_blocked",
                            "model output was blocked by provider safety or content policy",
                        );
                        fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                        return;
                    }
                    FinishReason::Cancelled => {
                        cancel_model_turn(inner, sender, turn_id).await;
                        return;
                    }
                    FinishReason::Error => {
                        let diagnostic = diagnostic_from_text(
                            "model_finish_error",
                            "model output stopped because the provider reported a finish error",
                        );
                        fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                        return;
                    }
                }
            }
            Some(Ok(ModelEvent::ToolCallRequested { call })) => {
                tracing::debug!(
                    category = "tool_call_requested",
                    "runtime model stream event received"
                );
                match pending_tool_call_from_model(&call)
                    .and_then(|call| record_streamed_tool_call(&mut streamed_tool_calls, call))
                {
                    Ok(()) => {}
                    Err(diagnostic) => {
                        fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                        return;
                    }
                }
            }
            Some(Err(error)) => {
                let error_kind = error.kind();
                tracing::debug!(
                    category = "provider_error",
                    error_kind = ?error_kind,
                    "runtime model stream event received"
                );
                if is_cancelled_model_error(&error) {
                    cancel_model_turn(inner, sender, turn_id).await;
                    return;
                }

                let diagnostic = diagnostic_from_model_error(error);
                fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                return;
            }
            None => {
                tracing::debug!(category = "eof", "runtime model stream ended");
                let diagnostic = diagnostic_from_text(
                    "model_stream_eof",
                    "model stream ended before completion",
                );
                fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
                return;
            }
        }
    }
}
