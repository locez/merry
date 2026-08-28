//! Asynchronous event producer for the runtime-owned agent loop.

use super::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopError, AgentLoopResult, AgentLoopStatus,
    AgentRunMessage, PendingLoopToolCall, PendingLoopToolWave, StepOutcome,
    agent_loop_cancelled_diagnostic, agent_loop_stream_error, agent_loop_stream_error_with_source,
    can_retry_structured_output, classify_step_events, continuation_step_input,
    execute_stream_runtime_batch, final_assistant_output_from_step, next_agent_run_batch_id,
    publish_journal_event, receive_and_publish_bridge_tool_results, record_final_output_tool_call,
    settle_cancelled_bridge_tool_calls, settle_failed_bridge_tool_calls,
    structured_output_failure_result, take_subagent_notification, take_subagent_notification_input,
    tool_execution_cancelled_diagnostic, validate_final_output,
};
use crate::{
    Runtime, RuntimeError, StepContext, StepInput, ToolExecutionContext,
    bridge::{
        BridgeToolResultCommand, receive_bridge_tool_result, resolve_bridge_tool_result_command,
    },
    events::{ActiveStepPermit, RuntimeEventProjector},
};
use futures_util::StreamExt;
use merry_core::PendingToolCallBatch;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;

pub(crate) struct AgentLoopStreamProducer {
    runtime: Runtime,
    input: StepInput,
    loop_token: tokio_util::sync::CancellationToken,
    generation_config: merry_llm::GenerationConfig,
    config: AgentLoopConfig,
    loop_permit: ActiveStepPermit,
    sender: mpsc::Sender<AgentRunMessage>,
    bridge_receiver: mpsc::Receiver<BridgeToolResultCommand>,
    bridge_resolution_epoch: Arc<AtomicU64>,
}

pub(crate) struct AgentLoopStreamProducerInput {
    pub(crate) runtime: Runtime,
    pub(crate) input: StepInput,
    pub(crate) loop_token: tokio_util::sync::CancellationToken,
    pub(crate) generation_config: merry_llm::GenerationConfig,
    pub(crate) config: AgentLoopConfig,
    pub(crate) loop_permit: ActiveStepPermit,
    pub(crate) sender: mpsc::Sender<AgentRunMessage>,
    pub(crate) bridge_receiver: mpsc::Receiver<BridgeToolResultCommand>,
    pub(crate) bridge_resolution_epoch: Arc<AtomicU64>,
}

impl AgentLoopStreamProducer {
    pub(crate) fn new(input: AgentLoopStreamProducerInput) -> Self {
        let AgentLoopStreamProducerInput {
            runtime,
            input,
            loop_token,
            generation_config,
            config,
            loop_permit,
            sender,
            bridge_receiver,
            bridge_resolution_epoch,
        } = input;
        Self {
            runtime,
            input,
            loop_token,
            generation_config,
            config,
            loop_permit,
            sender,
            bridge_receiver,
            bridge_resolution_epoch,
        }
    }
}

pub(crate) async fn run_agent_loop_stream_producer(
    producer: AgentLoopStreamProducer,
) -> Result<AgentLoopResult, AgentLoopError> {
    let AgentLoopStreamProducer {
        runtime,
        input,
        loop_token,
        generation_config,
        config,
        loop_permit,
        sender,
        mut bridge_receiver,
        bridge_resolution_epoch,
    } = producer;
    let mut next_input = Some(input);
    let mut deferred_user_input = None;
    let mut events = Vec::new();
    let mut projector = RuntimeEventProjector::new();
    let mut model_turns_run = 0;
    let mut bridge_batch_sequence = 0_u64;
    let mut structured_output_retries: usize = 0;

    loop {
        if let Some(notification_input) = take_subagent_notification_input(&runtime).await {
            if next_input
                .as_ref()
                .is_some_and(|input| !input.user_messages().is_empty())
            {
                deferred_user_input = next_input.take();
            } else {
                let _ = next_input.take();
            }
            next_input = Some(notification_input);
        }
        let Some(input) = next_input.take() else {
            break;
        };
        if loop_token.is_cancelled() {
            let session_usage = runtime.usage().await;
            return Ok(AgentLoopResult::new(
                AgentLoopStatus::Cancelled {
                    diagnostic: agent_loop_cancelled_diagnostic(),
                },
                events,
                model_turns_run,
                None,
                session_usage,
            ));
        }

        let mut step_context =
            StepContext::new(loop_token.clone()).with_generation_config(generation_config.clone());
        if let Some(contract) = config.final_output_contract().cloned() {
            step_context = step_context.with_final_output_contract(contract);
        }
        let stream = runtime
            .step_with_active_permit(input, step_context, loop_permit.clone())
            .map_err(|source| {
                agent_loop_stream_error_with_source(&runtime, model_turns_run, &events, source)
            })?;
        model_turns_run += 1;

        let mut step_events = Vec::new();
        tokio::pin!(stream);
        while let Some(event) = stream.next().await {
            step_events.push(event.clone());
            publish_journal_event(&runtime, &mut projector, &sender, &mut events, event)
                .await
                .map_err(|source| {
                    agent_loop_stream_error_with_source(&runtime, model_turns_run, &events, source)
                })?;
        }

        let step_final_output = final_assistant_output_from_step(&runtime, &step_events).await;
        match classify_step_events(&step_events, config.final_output_contract()) {
            StepOutcome::Completed => {
                if let Some(notification_input) = take_subagent_notification_input(&runtime).await {
                    if model_turns_run >= config.max_model_turns() {
                        let session_usage = runtime.usage().await;
                        return Ok(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                    max_model_turns: config.max_model_turns(),
                                },
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }
                    next_input = Some(notification_input);
                    continue;
                }
                if let Some(deferred_user_input) = deferred_user_input.take() {
                    next_input = Some(deferred_user_input);
                    continue;
                }
                let session_usage = runtime.usage().await;
                return Ok(AgentLoopResult::new(
                    AgentLoopStatus::Completed,
                    events,
                    model_turns_run,
                    step_final_output,
                    session_usage,
                ));
            }
            StepOutcome::Failed(diagnostic) => {
                let session_usage = runtime.usage().await;
                return Ok(AgentLoopResult::new(
                    AgentLoopStatus::Failed { diagnostic },
                    events,
                    model_turns_run,
                    None,
                    session_usage,
                ));
            }
            StepOutcome::Cancelled(diagnostic) => {
                let session_usage = runtime.usage().await;
                return Ok(AgentLoopResult::new(
                    AgentLoopStatus::Cancelled { diagnostic },
                    events,
                    model_turns_run,
                    None,
                    session_usage,
                ));
            }
            StepOutcome::Blocked(reason) => {
                let session_usage = runtime.usage().await;
                return Ok(AgentLoopResult::new(
                    AgentLoopStatus::Blocked { reason },
                    events,
                    model_turns_run,
                    None,
                    session_usage,
                ));
            }
            StepOutcome::ToolResultRecorded => {
                if model_turns_run >= config.max_model_turns() {
                    let session_usage = runtime.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Blocked {
                            reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                max_model_turns: config.max_model_turns(),
                            },
                        },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }

                next_input = Some(continuation_step_input());
            }
            StepOutcome::PendingBatch(calls) => {
                if model_turns_run >= config.max_model_turns() {
                    let session_usage = runtime.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Blocked {
                            reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                max_model_turns: config.max_model_turns(),
                            },
                        },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }

                let mut waves = Vec::new();
                for call in calls {
                    match call {
                        PendingLoopToolCall::Runtime(call) => match waves.last_mut() {
                            Some(PendingLoopToolWave::Runtime(wave)) => wave.push(call),
                            _ => waves.push(PendingLoopToolWave::Runtime(vec![call])),
                        },
                        PendingLoopToolCall::Bridge(call) => match waves.last_mut() {
                            Some(PendingLoopToolWave::Bridge(wave)) => wave.push(call),
                            _ => waves.push(PendingLoopToolWave::Bridge(vec![call])),
                        },
                        PendingLoopToolCall::FinalOutput(_) => {
                            unreachable!("mixed final-output batches are rejected by provider step")
                        }
                    }
                }

                for wave in waves {
                    match wave {
                        PendingLoopToolWave::Runtime(calls) => {
                            if let Some(error) = execute_stream_runtime_batch(
                                &runtime,
                                calls,
                                &loop_token,
                                &loop_permit,
                                &mut projector,
                                &sender,
                                &mut events,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })? {
                                if let RuntimeError::ToolExecutionCancelled { call_id, .. } = error
                                {
                                    let session_usage = runtime.usage().await;
                                    return Ok(AgentLoopResult::new(
                                        AgentLoopStatus::Cancelled {
                                            diagnostic: tool_execution_cancelled_diagnostic(
                                                &call_id,
                                            ),
                                        },
                                        events,
                                        model_turns_run,
                                        None,
                                        session_usage,
                                    ));
                                }
                                return Err(agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    error,
                                ));
                            }
                        }
                        PendingLoopToolWave::Bridge(calls) => {
                            let batch_id = next_agent_run_batch_id(&mut bridge_batch_sequence)
                                .map_err(|source| {
                                    agent_loop_stream_error_with_source(
                                        &runtime,
                                        model_turns_run,
                                        &events,
                                        source,
                                    )
                                })?;
                            match receive_and_publish_bridge_tool_results(
                                &runtime,
                                calls,
                                batch_id,
                                &bridge_resolution_epoch,
                                &loop_token,
                                &loop_permit,
                                &mut bridge_receiver,
                                &mut projector,
                                &sender,
                                &mut events,
                            )
                            .await
                            {
                                Ok(()) => {}
                                Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
                                    let session_usage = runtime.usage().await;
                                    return Ok(AgentLoopResult::new(
                                        AgentLoopStatus::Cancelled {
                                            diagnostic: tool_execution_cancelled_diagnostic(
                                                &call_id,
                                            ),
                                        },
                                        events,
                                        model_turns_run,
                                        None,
                                        session_usage,
                                    ));
                                }
                                Err(source) => {
                                    return Err(agent_loop_stream_error_with_source(
                                        &runtime,
                                        model_turns_run,
                                        &events,
                                        source,
                                    ));
                                }
                            }
                        }
                    }
                }

                next_input = Some(continuation_step_input());
            }
            StepOutcome::Pending(call) => match call {
                PendingLoopToolCall::FinalOutput(call) => {
                    if let Some((notification_input, notification_text)) =
                        take_subagent_notification(&runtime).await
                    {
                        let failure_events = runtime
                            .submit_tool_execution_failure_with_active_permit(
                                call.id(),
                                &notification_text,
                                &loop_permit,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                        for event in failure_events {
                            publish_journal_event(
                                &runtime,
                                &mut projector,
                                &sender,
                                &mut events,
                                event,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                        }
                        if model_turns_run >= config.max_model_turns() {
                            let session_usage = runtime.usage().await;
                            return Ok(AgentLoopResult::new(
                                AgentLoopStatus::Blocked {
                                    reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                        max_model_turns: config.max_model_turns(),
                                    },
                                },
                                events,
                                model_turns_run,
                                None,
                                session_usage,
                            ));
                        }
                        next_input = Some(notification_input);
                        continue;
                    }
                    if let Some(Err(error)) = config
                        .final_output_contract()
                        .map(|contract| contract.validate_call(&call))
                    {
                        let error_message = error.message();
                        let failure_events = runtime
                            .submit_tool_input_validation_failure_with_active_permit(
                                &call,
                                error,
                                &loop_permit,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;

                        for event in failure_events {
                            publish_journal_event(
                                &runtime,
                                &mut projector,
                                &sender,
                                &mut events,
                                event,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                        }
                        structured_output_retries = structured_output_retries.saturating_add(1);
                        if can_retry_structured_output(
                            &config,
                            structured_output_retries,
                            model_turns_run,
                        ) {
                            next_input = Some(continuation_step_input());
                            continue;
                        }

                        return Ok(structured_output_failure_result(
                            &runtime,
                            events,
                            model_turns_run,
                            error_message,
                        )
                        .await);
                    }

                    if let Some(contract) = config.final_output_contract()
                        && let Err(error_message) = validate_final_output(contract, &call)
                    {
                        let failure_events = runtime
                            .submit_structured_output_failure_with_active_permit(
                                call.id(),
                                &error_message,
                                &loop_permit,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;

                        for event in failure_events {
                            publish_journal_event(
                                &runtime,
                                &mut projector,
                                &sender,
                                &mut events,
                                event,
                            )
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                        }
                        structured_output_retries = structured_output_retries.saturating_add(1);
                        if can_retry_structured_output(
                            &config,
                            structured_output_retries,
                            model_turns_run,
                        ) {
                            next_input = Some(continuation_step_input());
                            continue;
                        }

                        return Ok(structured_output_failure_result(
                            &runtime,
                            events,
                            model_turns_run,
                            error_message,
                        )
                        .await);
                    }

                    let (final_output, events_for_final_output) =
                        record_final_output_tool_call(&runtime, call)
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    source,
                                )
                            })?;
                    for event in events_for_final_output {
                        publish_journal_event(
                            &runtime,
                            &mut projector,
                            &sender,
                            &mut events,
                            event,
                        )
                        .await
                        .map_err(|source| {
                            agent_loop_stream_error_with_source(
                                &runtime,
                                model_turns_run,
                                &events,
                                source,
                            )
                        })?;
                    }
                    let session_usage = runtime.usage().await;
                    return Ok(AgentLoopResult::new_with_final_output_json(
                        AgentLoopStatus::Completed,
                        events,
                        model_turns_run,
                        None,
                        Some(final_output),
                        session_usage,
                    ));
                }
                call => {
                    if model_turns_run >= config.max_model_turns() {
                        let session_usage = runtime.usage().await;
                        return Ok(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                                    max_model_turns: config.max_model_turns(),
                                },
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }

                    let execution_events = match call {
                        PendingLoopToolCall::Runtime(call) => {
                            match runtime
                                .execute_tool_call_with_active_permit(
                                    call.id(),
                                    ToolExecutionContext::new(loop_token.clone()),
                                    &loop_permit,
                                )
                                .await
                            {
                                Ok(events) => events,
                                Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
                                    let session_usage = runtime.usage().await;
                                    return Ok(AgentLoopResult::new(
                                        AgentLoopStatus::Cancelled {
                                            diagnostic: tool_execution_cancelled_diagnostic(
                                                &call_id,
                                            ),
                                        },
                                        events,
                                        model_turns_run,
                                        None,
                                        session_usage,
                                    ));
                                }
                                Err(error) => {
                                    return Err(agent_loop_stream_error_with_source(
                                        &runtime,
                                        model_turns_run,
                                        &events,
                                        error,
                                    ));
                                }
                            }
                        }
                        PendingLoopToolCall::Bridge(call) => {
                            let batch = PendingToolCallBatch::new(
                                next_agent_run_batch_id(&mut bridge_batch_sequence).map_err(
                                    |source| {
                                        agent_loop_stream_error_with_source(
                                            &runtime,
                                            model_turns_run,
                                            &events,
                                            source,
                                        )
                                    },
                                )?,
                                vec![call.clone()],
                            )
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(
                                    &runtime,
                                    model_turns_run,
                                    &events,
                                    RuntimeError::from(source),
                                )
                            })?;
                            sender
                                .send(AgentRunMessage::ToolInvocations {
                                    batch: batch.clone(),
                                })
                                .await
                                .map_err(|_| {
                                    agent_loop_stream_error(
                                        runtime.session_id(),
                                        events.clone(),
                                        "bridge invocation receiver closed before the request was delivered",
                                    )
                                })?;
                            loop {
                                let command = match receive_bridge_tool_result(
                                    &mut bridge_receiver,
                                    &loop_token,
                                )
                                .await
                                {
                                    Some(command) => command,
                                    None if loop_token.is_cancelled() => {
                                        let settlement = settle_cancelled_bridge_tool_calls(
                                            &runtime,
                                            batch.calls(),
                                            &loop_permit,
                                            &mut projector,
                                            &sender,
                                            &mut events,
                                        )
                                        .await;
                                        bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                                        settlement.map_err(|source| {
                                            agent_loop_stream_error_with_source(
                                                &runtime,
                                                model_turns_run,
                                                &events,
                                                source,
                                            )
                                        })?;
                                        let session_usage = runtime.usage().await;
                                        return Ok(AgentLoopResult::new(
                                            AgentLoopStatus::Cancelled {
                                                diagnostic: agent_loop_cancelled_diagnostic(),
                                            },
                                            events,
                                            model_turns_run,
                                            None,
                                            session_usage,
                                        ));
                                    }
                                    None => {
                                        return Err(agent_loop_stream_error(
                                            runtime.session_id(),
                                            events,
                                            "bridge tool result channel closed before the call was resolved",
                                        ));
                                    }
                                };

                                let (ack_sender, result) = resolve_bridge_tool_result_command(
                                    &runtime,
                                    &batch,
                                    command,
                                    &loop_permit,
                                )
                                .await;
                                match result {
                                    Ok(events) => {
                                        bridge_resolution_epoch.fetch_add(1, Ordering::AcqRel);
                                        let _ = ack_sender.send(Ok(()));
                                        break events;
                                    }
                                    Err(error) if error.is_retryable_bridge_tool_result() => {
                                        let _ = ack_sender.send(Err(error));
                                    }
                                    Err(error) => {
                                        let message = error.to_string();
                                        let _ = ack_sender.send(Err(
                                            RuntimeError::BridgeToolResultRejected {
                                                session_id: runtime.session_id().clone(),
                                                message: message.clone(),
                                            },
                                        ));
                                        settle_failed_bridge_tool_calls(
                                            &runtime,
                                            batch.calls(),
                                            &loop_permit,
                                            &mut projector,
                                            &sender,
                                            &mut events,
                                            &message,
                                        )
                                        .await
                                        .map_err(
                                            |source| {
                                                agent_loop_stream_error_with_source(
                                                    &runtime,
                                                    model_turns_run,
                                                    &events,
                                                    source,
                                                )
                                            },
                                        )?;
                                        break Vec::new();
                                    }
                                }
                            }
                        }
                        PendingLoopToolCall::FinalOutput(_) => {
                            unreachable!("final-output call is handled before continuation budget")
                        }
                    };

                    for event in execution_events {
                        publish_journal_event(
                            &runtime,
                            &mut projector,
                            &sender,
                            &mut events,
                            event,
                        )
                        .await
                        .map_err(|source| {
                            agent_loop_stream_error_with_source(
                                &runtime,
                                model_turns_run,
                                &events,
                                source,
                            )
                        })?;
                    }

                    next_input = Some(continuation_step_input());
                }
            },
        }
    }

    Err(agent_loop_stream_error(
        runtime.session_id(),
        events,
        "agent loop producer ended without a terminal result",
    ))
}
