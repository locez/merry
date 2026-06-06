use super::*;

pub(super) async fn run_provider_step(
    inner: &Arc<RuntimeInner>,
    sender: &mpsc::Sender<RuntimeEvent>,
    token: &CancellationToken,
    input: StepInput,
    generation_config: GenerationConfig,
    final_output_contract: Option<crate::FinalOutputContract>,
    provider_config: ModelProviderConfig,
) {
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

    let activated_memories = match activation_result {
        Ok(memories) => memories,
        Err(error) => {
            let diagnostic = diagnostic_from_text("memory_activation", error.to_string());
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };
    tracing::debug!(
        category = "activated_memory_count",
        count = activated_memories.len(),
        "runtime memories activated"
    );

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
        let append_only_body = match session.append_only_body_snapshot() {
            Ok(body) => body,
            Err(error) => {
                session.replace_activated_memories(Vec::new());
                inner.memory_projection_epoch.fetch_add(1, Ordering::AcqRel);
                let diagnostic = diagnostic_from_text(
                    "append_only_body_artifact",
                    format!("append-only body artifact could not be read: {error}"),
                );
                drop(session);
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        };
        let continuations = match session.uncheckpointed_tool_continuation_snapshots() {
            Ok(continuations) => continuations,
            Err(error) => {
                session.replace_activated_memories(Vec::new());
                inner.memory_projection_epoch.fetch_add(1, Ordering::AcqRel);
                let diagnostic = diagnostic_from_text(
                    "tool_continuation_artifact",
                    format!("tool continuation artifact could not be read: {error}"),
                );
                drop(session);
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        };
        (
            StepRequestInputs::from_session(&session, append_only_body, continuations),
            activation_epoch,
        )
    };
    let mut projection_guard =
        ActivationProjectionGuard::new(Arc::clone(inner), token.clone(), activation_epoch);
    let mut tool_specs = inner.tool_registry.tool_specs();
    if let Some(contract) = &final_output_contract {
        tool_specs.push(contract.tool_spec().clone());
    }
    tracing::debug!(
        category = "continuations_and_tools",
        continuation_count = request_inputs.continuations.len(),
        tool_spec_count = tool_specs.len(),
        "runtime provider request inputs counted"
    );

    let mut request = match compile_step_request_from_inputs(
        &input,
        provider_config.model(),
        &request_inputs,
        tool_specs.clone(),
        generation_config.clone(),
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

    let provider = provider_config.provider();
    let mut request_budget = request_context_budget(provider.capabilities(), &request);
    if let Err(error) = &request_budget {
        trace_provider_request_budget_unavailable(
            inner.session_id.as_str(),
            provider.name().as_str(),
            &request,
            error,
        );
    }
    if matches!(
        request_budget.as_ref().map(|budget| budget.decision),
        Ok(CheckpointDecision::RequireCheckpoint)
    ) {
        match compact_context_for_hard_watermark(inner, token).await {
            Ok(Some(_outcome)) => {
                let refreshed = {
                    let session = inner.session.lock().await;
                    match step_request_inputs_from_session(&session) {
                        Ok(inputs) => inputs,
                        Err(error) => {
                            clear_current_activated_memories(inner).await;
                            let diagnostic = diagnostic_from_text(
                                "auto_compaction_projection",
                                error.to_string(),
                            );
                            trace_provider_step_failed(&diagnostic);
                            let _ = send_failed_event(inner, sender, token, diagnostic).await;
                            return;
                        }
                    }
                };
                request_inputs = refreshed;
                request = match compile_step_request_from_inputs(
                    &input,
                    provider_config.model(),
                    &request_inputs,
                    tool_specs.clone(),
                    generation_config.clone(),
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
                request_budget = request_context_budget(provider.capabilities(), &request);
                if let Err(error) = &request_budget {
                    trace_provider_request_budget_unavailable(
                        inner.session_id.as_str(),
                        provider.name().as_str(),
                        &request,
                        error,
                    );
                }
            }
            Ok(None) => {}
            Err(error) => {
                clear_current_activated_memories(inner).await;
                if token.is_cancelled() {
                    trace_provider_step_cancelled();
                    let _ = send_cancelled_event(inner, sender).await;
                    return;
                }
                let diagnostic = diagnostic_from_text("auto_compaction", error.to_string());
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        }
    }
    let sent_continuation_count = request_inputs.continuations.len();

    if input.should_record_user_history() {
        let mut session = inner.session.lock().await;
        if token.is_cancelled() {
            drop(session);
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
            return;
        }
        session.record_user_message_body(input.text());
    }

    let stream_context = ModelStreamContext::new(token.clone());
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
            trace_provider_step_cancelled();
            let _ = send_cancelled_event(inner, sender).await;
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
                trace_provider_step_cancelled();
                let _ = send_cancelled_event(inner, sender).await;
                return;
            }

            let diagnostic = diagnostic_from_model_error(error);
            trace_provider_step_failed(&diagnostic);
            let _ = send_failed_event(inner, sender, token, diagnostic).await;
            return;
        }
    };
    projection_guard.disarm();

    let mut commentary_text = String::new();
    let mut streamed_tool_call: Option<PendingToolCall> = None;

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
                trace_provider_step_cancelled();
                let _ = send_cancelled_event(inner, sender).await;
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
                }
            }
            Some(Ok(ModelEvent::Completed { response })) => {
                tracing::debug!(
                    category = "completed",
                    finish_reason = ?response.finish_reason(),
                    "runtime model stream event received"
                );
                match response.finish_reason() {
                    FinishReason::Stop => {
                        if streamed_tool_call.is_some() {
                            let diagnostic = diagnostic_from_text(
                                DIAGNOSTIC_MODEL_TOOL_CALL_MIXED_OUTPUT,
                                "model requested a tool call before completing with text output",
                            );
                            trace_provider_step_failed(&diagnostic);
                            let _ = send_failed_event(inner, sender, token, diagnostic).await;
                            return;
                        }

                        let [ModelOutput::Text { text }] = response.outputs() else {
                            let diagnostic = diagnostic_from_text(
                                "model_output_unsupported",
                                "model stop output must contain exactly one text item",
                            );
                            trace_provider_step_failed(&diagnostic);
                            let _ = send_failed_event(inner, sender, token, diagnostic).await;
                            return;
                        };

                        if !send_assistant_text_output_completed_events(
                            inner,
                            sender,
                            token,
                            text.clone(),
                        )
                        .await
                        {
                            let _ = send_cancelled_if_requested(inner, sender, token).await;
                        }
                        return;
                    }
                    FinishReason::ToolCalls => {
                        match pending_tool_call_from_outputs(
                            response.outputs(),
                            streamed_tool_call.as_ref(),
                        ) {
                            Ok(call) => {
                                if let Some(commentary) =
                                    tool_call_commentary_text(response.outputs(), &commentary_text)
                                    && !send_assistant_text_output_recorded_event(
                                        inner, sender, token, commentary,
                                    )
                                    .await
                                {
                                    let _ = send_cancelled_if_requested(inner, sender, token).await;
                                    return;
                                }
                                if !send_tool_call_pending_event(inner, sender, token, call).await {
                                    let _ = send_cancelled_if_requested(inner, sender, token).await;
                                }
                            }
                            Err(diagnostic) => {
                                trace_provider_step_failed(&diagnostic);
                                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                            }
                        }
                        return;
                    }
                    FinishReason::Length => {
                        let diagnostic = diagnostic_from_text(
                            "model_length",
                            "model output stopped because it reached a length limit",
                        );
                        trace_provider_step_failed(&diagnostic);
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
                        return;
                    }
                    FinishReason::Cancelled => {
                        trace_provider_step_cancelled();
                        let _ = send_cancelled_event(inner, sender).await;
                        return;
                    }
                    FinishReason::Error => {
                        let diagnostic = diagnostic_from_text(
                            "model_finish_error",
                            "model output stopped because the provider reported a finish error",
                        );
                        trace_provider_step_failed(&diagnostic);
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
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
                    .and_then(|call| record_streamed_tool_call(&mut streamed_tool_call, call))
                {
                    Ok(call) => {
                        streamed_tool_call = Some(call);
                    }
                    Err(diagnostic) => {
                        trace_provider_step_failed(&diagnostic);
                        let _ = send_failed_event(inner, sender, token, diagnostic).await;
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
                    trace_provider_step_cancelled();
                    let _ = send_cancelled_event(inner, sender).await;
                    return;
                }

                let diagnostic = diagnostic_from_model_error(error);
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
            None => {
                tracing::debug!(category = "eof", "runtime model stream ended");
                let diagnostic = diagnostic_from_text(
                    "model_stream_eof",
                    "model stream ended before completion",
                );
                trace_provider_step_failed(&diagnostic);
                let _ = send_failed_event(inner, sender, token, diagnostic).await;
                return;
            }
        }
    }
}
