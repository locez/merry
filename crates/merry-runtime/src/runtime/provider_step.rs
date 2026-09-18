use super::auto_compaction::{
    CompactionProgress, HardWatermarkCompaction, HardWatermarkOutcome,
    reduce_context_at_hard_watermark,
};
use super::journal_emission::{
    send_assistant_text_output_completed_events, send_assistant_text_output_delta_event,
    send_cancelled_event, send_compaction_completed_event, send_failed_event,
    send_model_tool_call_response_events, send_model_usage_updated_event,
    trace_provider_step_cancelled, trace_provider_step_failed,
};
use super::memory_activation::{
    ActivationProjectionGuard, clear_current_activated_memories,
    memory_activation_seed_from_step_input,
};
use super::model_output::{
    DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT, diagnostic_from_model_error, is_cancelled_model_error,
    pending_tool_call_from_model, pending_tool_calls_from_outputs, record_streamed_tool_call,
    tool_call_commentary_text,
};
use super::model_turn_lifecycle::{InProgressModelTurnGuard, cancel_model_turn, fail_model_turn};
use super::provider_request::{
    compile_step_request_from_inputs, request_context_budget, step_request_compile_diagnostic,
    step_request_inputs_from_session, step_usage_context_snapshot, trace_provider_request,
    trace_provider_request_budget_unavailable,
};
use super::provider_stream::{
    stream_model_with_retry_policy, wait_for_model_stream_item, wait_for_retrying_stream_setup,
};

use super::{DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED, RuntimeInner, diagnostic_from_text};

use crate::{
    CheckpointDecision,
    events::{ActiveStepPermit, RuntimeJournalEventBatch},
    memory::MemoryActivationContext,
    model_config::ModelProviderConfig,
    plan::unix_time_ms,
    step::StepInput,
};
use merry_core::PendingToolCall;
use merry_llm::{FinishReason, GenerationConfig, ModelEvent, ModelOutput, ModelStreamContext};
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn has_unresolved_pending_tool_calls(inner: &RuntimeInner) -> bool {
    let session = inner.session.lock().await;
    session.has_pending_tool_calls()
}

pub(super) struct ProviderStepControl<'a> {
    token: &'a CancellationToken,
    active_permit: &'a ActiveStepPermit,
    step_sequence: u64,
}

impl<'a> ProviderStepControl<'a> {
    pub(super) const fn new(
        token: &'a CancellationToken,
        active_permit: &'a ActiveStepPermit,
        step_sequence: u64,
    ) -> Self {
        Self {
            token,
            active_permit,
            step_sequence,
        }
    }
}

pub(super) async fn run_provider_step(
    inner: &Arc<RuntimeInner>,
    sender: &mpsc::Sender<RuntimeJournalEventBatch>,
    control: ProviderStepControl<'_>,
    input: StepInput,
    generation_config: GenerationConfig,
    final_output_contract: Option<crate::FinalOutputContract>,
    provider_config: ModelProviderConfig,
) {
    let ProviderStepControl {
        token,
        active_permit,
        step_sequence,
    } = control;
    if has_unresolved_pending_tool_calls(inner).await {
        tracing::debug!(
            category = "unresolved_pending_tool_gate",
            diagnostic_code = DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
            "runtime provider step gated by unresolved pending tool call"
        );
        let diagnostic = diagnostic_from_text(
            DIAGNOSTIC_TOOL_CALL_RESULT_REQUIRED,
            "a pending tool call must be resolved before the next provider step",
        );
        trace_provider_step_failed(&diagnostic);
        let _ = send_failed_event(inner, sender, token, diagnostic).await;
        return;
    }

    clear_current_activated_memories(inner).await;

    if token.is_cancelled() {
        trace_provider_step_cancelled();
        let _ = send_cancelled_event(inner, sender).await;
        return;
    }

    let seed = match memory_activation_seed_from_step_input(&input) {
        Ok(seed) => seed,
        Err(error) => {
            let diagnostic = diagnostic_from_text("memory_activation", error.to_string());
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let activated_memories = if let Some(seed) = seed {
        let candidates = {
            let session = inner.session.lock().await;
            session.memory_store().candidate_snapshot()
        };
        tracing::debug!(
            category = "memory_candidate_count",
            count = candidates.len(),
            "runtime memory candidates collected"
        );
        if token.is_cancelled() {
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }

        let activation_context = MemoryActivationContext::new(token.clone());
        let activation_result = tokio::select! {
            biased;
            () = token.cancelled() => {
                trace_provider_step_cancelled();
                let _ = send_cancelled_event(inner, sender).await;
                return;
            }
            result = inner
                .memory_activation_source
                .activate(seed, candidates, activation_context) => result,
        };
        if token.is_cancelled() {
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }

        match activation_result {
            Ok(memories) => memories,
            Err(error) => {
                let diagnostic = diagnostic_from_text("memory_activation", error.to_string());
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        }
    } else {
        Vec::new()
    };
    tracing::debug!(
        category = "activated_memory_count",
        count = activated_memories.len(),
        "runtime memories activated"
    );

    let plan_subagent_control = if let Some(control) = inner.plan_subagent_control.as_ref() {
        if let Err(error) = control.review_progress_at_boundary(unix_time_ms()).await {
            let diagnostic = diagnostic_from_text("plan_progress_review", error.to_string());
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
        if let Err(error) = control.deliver_directives(unix_time_ms()).await {
            let diagnostic = diagnostic_from_text("plan_directive_delivery", error.to_string());
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
        match control.snapshot().await {
            Ok(snapshot) => crate::plan::projection::plan_subagent_control_message(
                &snapshot,
                control.node_id(),
                control.attempt_id(),
                control.lease_id(),
            ),
            Err(error) => {
                let diagnostic = diagnostic_from_text("plan_subagent_context", error.to_string());
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        }
    } else {
        None
    };

    let (mut request_inputs, activation_epoch) = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            drop(session);
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        session.replace_activated_memories(activated_memories);
        let activation_epoch = inner
            .memory_projection_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let request_inputs = match step_request_inputs_from_session(
            &session,
            plan_subagent_control.clone(),
            inner.coordinator_plan_tools,
        ) {
            Ok(inputs) => inputs,
            Err(error) => {
                session.replace_activated_memories(Vec::new());
                inner.memory_projection_epoch.fetch_add(1, Ordering::AcqRel);
                let diagnostic = diagnostic_from_text(
                    "transcript_artifact",
                    format!("transcript artifact could not be read: {error}"),
                );
                drop(session);
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        };
        (request_inputs, activation_epoch)
    };
    let mut projection_guard =
        ActivationProjectionGuard::new(Arc::clone(inner), token.clone(), activation_epoch);
    let provider = provider_config.provider();
    let generation_config =
        match generation_config.resolve_parallel_tool_calls(provider.capabilities()) {
            Ok(generation_config) => generation_config,
            Err(error) => {
                clear_current_activated_memories(inner).await;
                let diagnostic = diagnostic_from_text(
                    "provider_parallel_tool_calls_unsupported",
                    error.to_string(),
                );
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        };
    let mut tool_specs = inner.visible_tool_specs();
    if let Some(contract) = &final_output_contract {
        tool_specs.push(contract.tool_spec().clone());
    }
    if !tool_specs.is_empty() && !provider.capabilities().supports_tool_calls() {
        clear_current_activated_memories(inner).await;
        let diagnostic = diagnostic_from_text(
            "provider_tool_calls_unsupported",
            format!(
                "provider {} does not support tool calls required by runtime tools",
                provider.name()
            ),
        );
        trace_provider_step_failed(&diagnostic);
        let _ = send_failed_event(inner, sender, token, diagnostic).await;
        return;
    }
    tracing::debug!(
        category = "transcript_and_tools",
        transcript_item_count = request_inputs.transcript.len(),
        tool_spec_count = tool_specs.len(),
        "runtime provider request inputs counted"
    );

    let mut request = match compile_step_request_from_inputs(
        &input,
        provider_config.model(),
        &request_inputs,
        tool_specs.clone(),
        generation_config.clone(),
        &inner.prompt_profile,
        inner.progress_commentary,
    ) {
        Ok(request) => {
            tracing::debug!(
                category = "model_request_compiled",
                "runtime model request compiled"
            );
            request
        }
        Err(error) => {
            clear_current_activated_memories(inner).await;
            let diagnostic = step_request_compile_diagnostic(&error);
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };

    let context_window_override = inner
        .context_window_tokens
        .read()
        .await
        .map(std::num::NonZeroU64::get);
    let mut request_budget =
        request_context_budget(provider.capabilities(), &request, context_window_override);
    // Read the compaction policy once for this step instead of re-locking per
    // compaction attempt.
    let (automatic_compaction_enabled, automatic_policy, compaction_reasoning_effort) = {
        let config = inner.automatic_compaction.read().await;
        (
            config.is_enabled(),
            config.policy(),
            config.reasoning_effort().cloned(),
        )
    };
    if let Err(error) = &request_budget {
        trace_provider_request_budget_unavailable(
            inner.session_id.as_str(),
            provider.name().as_str(),
            &request,
            error,
        );
        if automatic_compaction_enabled {
            clear_current_activated_memories(inner).await;
            let diagnostic = diagnostic_from_text(
                "auto_compaction",
                format!(
                    "cannot confirm request budget before automatic context reduction: {error}"
                ),
            );
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    }
    if automatic_compaction_enabled
        && let Ok(initial_budget) = &request_budget
        && matches!(
            initial_budget.decision,
            CheckpointDecision::RequireCheckpoint
        )
    {
        // Rolling compaction: each pass covers as much history as the compaction
        // window can host, and passes continue until the recompiled request lands
        // under the destination body target. Crossing the hard watermark starts the
        // work; finishing it requires the preferred tail, not merely dropping below
        // the trigger.
        let mut current_budget = *initial_budget;
        let mut progress = CompactionProgress::new(current_budget.dynamic_body_estimated_tokens);
        loop {
            let outcome = reduce_context_at_hard_watermark(
                inner,
                sender,
                token,
                active_permit,
                HardWatermarkCompaction {
                    policy: automatic_policy,
                    reasoning_effort: compaction_reasoning_effort.as_ref(),
                    request_budget: &current_budget,
                    input: &input,
                    request_inputs: &request_inputs,
                    tool_specs: tool_specs.clone(),
                    generation_config: generation_config.clone(),
                    primary_model: provider_config.model(),
                    request: &request,
                },
            )
            .await;
            let (replacement_outcome, target_dynamic_body_tokens) = match outcome {
                HardWatermarkOutcome::Continue {
                    replacement,
                    target_dynamic_body_tokens,
                } => (replacement, target_dynamic_body_tokens),
                HardWatermarkOutcome::Aborted => return,
            };
            let installed_replacement = replacement_outcome.is_some();

            let refreshed = {
                let session = inner.session.lock().await;
                step_request_inputs_from_session(
                    &session,
                    plan_subagent_control.clone(),
                    inner.coordinator_plan_tools,
                )
            };
            request_inputs = match refreshed {
                Ok(inputs) => inputs,
                Err(error) => {
                    clear_current_activated_memories(inner).await;
                    let diagnostic =
                        diagnostic_from_text("auto_compaction_projection", error.to_string());
                    trace_provider_step_failed(&diagnostic);
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
            };
            request = match compile_step_request_from_inputs(
                &input,
                provider_config.model(),
                &request_inputs,
                tool_specs.clone(),
                generation_config.clone(),
                &inner.prompt_profile,
                inner.progress_commentary,
            ) {
                Ok(request) => request,
                Err(error) => {
                    clear_current_activated_memories(inner).await;
                    let diagnostic = step_request_compile_diagnostic(&error);
                    trace_provider_step_failed(&diagnostic);
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
            };
            request_budget =
                request_context_budget(provider.capabilities(), &request, context_window_override);
            current_budget = match &request_budget {
                Ok(budget) => *budget,
                Err(error) => {
                    trace_provider_request_budget_unavailable(
                        inner.session_id.as_str(),
                        provider.name().as_str(),
                        &request,
                        error,
                    );
                    clear_current_activated_memories(inner).await;
                    let diagnostic = diagnostic_from_text(
                        "auto_compaction",
                        format!(
                            "cannot confirm request budget after automatic context reduction: {error}"
                        ),
                    );
                    trace_provider_step_failed(&diagnostic);
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
            };

            let compaction_event_sent = match replacement_outcome {
                Some(outcome) => {
                    send_compaction_completed_event(
                        inner,
                        sender,
                        token,
                        outcome.checkpoint_id().as_str().to_owned(),
                        outcome.covered_history_item_count(),
                    )
                    .await
                }
                None => true,
            };
            if !compaction_event_sent {
                return;
            }

            match progress.observe(
                current_budget.dynamic_body_estimated_tokens,
                target_dynamic_body_tokens,
                current_budget.budget.hard_water_tokens(),
                installed_replacement,
            ) {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => {
                    clear_current_activated_memories(inner).await;
                    let diagnostic = diagnostic_from_text("auto_compaction", error.to_string());
                    trace_provider_step_failed(&diagnostic);
                    let _ = send_failed_event(inner, sender, token, diagnostic).await;
                    return;
                }
            }
        }
    }
    inner
        .trajectory
        .observe_model_request(&request, step_sequence);
    let usage_context_snapshot =
        step_usage_context_snapshot(request_budget.as_ref().ok(), automatic_compaction_enabled);
    let sent_continuation_count = request.continuations().len();

    let turn_id = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            drop(session);
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        match session.begin_model_turn_at_sequence(step_sequence) {
            Ok(turn_id) => turn_id,
            Err(error) => {
                drop(session);
                let diagnostic = diagnostic_from_text("model_turn_begin", error.to_string());
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        }
    };
    let _model_turn_guard = InProgressModelTurnGuard::new(Arc::clone(inner), turn_id);

    let user_record_result = {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            let _ = session.abort_model_turn(turn_id);
            drop(session);
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        input
            .user_messages_for_history()
            .iter()
            .try_for_each(|message| session.record_user_message(turn_id, message))
    };
    if let Err(error) = user_record_result {
        let diagnostic = diagnostic_from_text("transcript_record", error.to_string());
        fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
        return;
    }

    let stream_context =
        ModelStreamContext::new(token.clone()).with_prompt_cache_key(inner.session_id.clone());
    let (retry_event_sender, mut retry_event_receiver) = mpsc::channel(8);
    trace_provider_request(
        inner.session_id.as_str(),
        provider.name().as_str(),
        &request,
        sent_continuation_count,
        request_budget.as_ref().ok(),
    );
    tracing::debug!(
        category = "provider_setup_start",
        "runtime provider stream setup started"
    );
    let stream_result = wait_for_retrying_stream_setup(
        inner,
        sender,
        token,
        stream_model_with_retry_policy(
            provider,
            provider_config.retry_policy(),
            request,
            stream_context,
            Some(retry_event_sender),
        ),
        &mut retry_event_receiver,
    )
    .await;

    let stream_result = match stream_result {
        Some(result) => result,
        None => {
            clear_current_activated_memories(inner).await;
            cancel_model_turn(inner, sender, turn_id).await;
            return;
        }
    };

    let mut stream = match stream_result {
        Ok(stream) => {
            tracing::debug!(
                category = "provider_setup_success",
                "runtime provider stream setup succeeded"
            );
            stream
        }
        Err(error) => {
            clear_current_activated_memories(inner).await;
            let error_kind = error.kind();
            tracing::debug!(
                category = "provider_setup_error",
                error_kind = ?error_kind,
                "runtime provider stream setup failed"
            );
            if is_cancelled_model_error(&error) {
                cancel_model_turn(inner, sender, turn_id).await;
                return;
            }

            let diagnostic = diagnostic_from_model_error(error);
            fail_model_turn(inner, sender, token, turn_id, diagnostic).await;
            return;
        }
    };
    projection_guard.disarm();

    let mut commentary_text = String::new();
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
