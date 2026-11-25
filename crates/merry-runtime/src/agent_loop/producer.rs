//! Asynchronous event producer for the runtime-owned agent loop.

use super::hook_step::{
    AfterStepHookAction, BeforeStepHookAction, StepHookContext, finish_hook_result,
    handle_stream_wave_outcome, publish_stream_events, run_after_model_step_hook,
    run_before_model_step_hook,
};
use super::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopError, AgentLoopResult, AgentLoopStatus,
    AgentRunMessage, PendingLoopToolCall, PendingLoopToolWave, StepOutcome,
    agent_loop_cancelled_diagnostic, agent_loop_stream_error, agent_loop_stream_error_with_source,
    can_retry_structured_output, classify_step_events, continuation_step_input,
    execute_stream_runtime_batch, final_assistant_output_from_step, next_agent_run_batch_id,
    publish_journal_event, receive_and_publish_bridge_tool_results, record_final_output_tool_call,
    structured_output_failure_result, take_subagent_notification, take_subagent_notification_input,
    validate_final_output,
};
use crate::{
    Runtime, StepContext, StepInput,
    bridge::BridgeToolResultCommand,
    events::{ActiveStepPermit, RuntimeEventProjector},
    hooks::bridge::{HookDecisionCommand, StreamHookBridge},
};
use futures_util::StreamExt;
use std::sync::{Arc, atomic::AtomicU64};
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
    hook_receiver: mpsc::Receiver<HookDecisionCommand>,
    hook_resolution_epoch: Arc<AtomicU64>,
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
    pub(crate) hook_receiver: mpsc::Receiver<HookDecisionCommand>,
    pub(crate) hook_resolution_epoch: Arc<AtomicU64>,
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
            hook_receiver,
            hook_resolution_epoch,
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
            hook_receiver,
            hook_resolution_epoch,
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
        mut hook_receiver,
        hook_resolution_epoch,
    } = producer;
    let mut next_input = Some(input);
    let mut deferred_user_input = None;
    let mut events = Vec::new();
    let mut projector = RuntimeEventProjector::new();
    let mut subagent_event_receiver = runtime.subscribe_subagent_events();
    let mut model_turns_run = 0;
    let mut bridge_batch_sequence = 0_u64;
    let mut hook_sequence = 0_u64;
    let mut structured_output_retries: usize = 0;

    loop {
        drain_subagent_stream_events(
            &runtime,
            &mut subagent_event_receiver,
            &mut projector,
            &sender,
            &mut events,
            model_turns_run,
        )
        .await?;
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
        let Some(mut input) = next_input.take() else {
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

        let before_ctx = StepHookContext {
            runtime: &runtime,
            model_turn: model_turns_run + 1,
            max_model_turns: config.max_model_turns(),
            loop_token: &loop_token,
            loop_permit: &loop_permit,
        };
        let mut hook_bridge = StreamHookBridge {
            sender: &sender,
            receiver: &mut hook_receiver,
            resolution_epoch: &hook_resolution_epoch,
            sequence: &mut hook_sequence,
        };
        let step_generation_config = match run_before_model_step_hook(
            &before_ctx,
            &mut input,
            generation_config.clone(),
            Some(&mut hook_bridge),
        )
        .await
        {
            Ok(BeforeStepHookAction::Proceed(gen_cfg)) => gen_cfg,
            Ok(BeforeStepHookAction::Finish {
                status,
                final_output,
            }) => {
                return Ok(finish_hook_result(
                    &runtime,
                    status,
                    events,
                    model_turns_run,
                    final_output,
                )
                .await);
            }
            Err(source) => {
                return Err(agent_loop_stream_error_with_source(
                    &runtime,
                    model_turns_run,
                    &events,
                    source,
                ));
            }
        };

        let mut step_context =
            StepContext::new(loop_token.clone()).with_generation_config(step_generation_config);
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
        loop {
            tokio::select! {
                event = stream.next() => {
                    let Some(event) = event else {
                        break;
                    };
                    if !event.payload.is_transient() {
                        step_events.push(event.clone());
                    }
                    publish_journal_event(&runtime, &mut projector, &sender, &mut events, event)
                        .await
                        .map_err(|source| {
                            agent_loop_stream_error_with_source(&runtime, model_turns_run, &events, source)
                        })?;
                }
                subagent_event = subagent_event_receiver.recv() => {
                    if let Ok(event) = subagent_event {
                        publish_journal_event(&runtime, &mut projector, &sender, &mut events, event)
                            .await
                            .map_err(|source| {
                                agent_loop_stream_error_with_source(&runtime, model_turns_run, &events, source)
                            })?;
                    }
                }
            }
        }
        drain_subagent_stream_events(
            &runtime,
            &mut subagent_event_receiver,
            &mut projector,
            &sender,
            &mut events,
            model_turns_run,
        )
        .await?;

        let step_final_output = final_assistant_output_from_step(&runtime, &step_events).await;
        let outcome = classify_step_events(&step_events, config.final_output_contract());

        let after_ctx = StepHookContext {
            runtime: &runtime,
            model_turn: model_turns_run,
            max_model_turns: config.max_model_turns(),
            loop_token: &loop_token,
            loop_permit: &loop_permit,
        };
        let mut hook_bridge = StreamHookBridge {
            sender: &sender,
            receiver: &mut hook_receiver,
            resolution_epoch: &hook_resolution_epoch,
            sequence: &mut hook_sequence,
        };
        match run_after_model_step_hook(
            &after_ctx,
            &outcome,
            &step_final_output,
            Some(&mut hook_bridge),
        )
        .await
        {
            Ok(AfterStepHookAction::Accept) => {}
            Ok(AfterStepHookAction::ContinueWithInput {
                settled_events,
                input,
            }) => {
                publish_stream_events(
                    &runtime,
                    &mut projector,
                    &sender,
                    &mut events,
                    settled_events,
                    model_turns_run,
                )
                .await?;
                next_input = Some(input);
                continue;
            }
            Ok(AfterStepHookAction::Finish {
                settled_events,
                status,
                final_output,
            }) => {
                publish_stream_events(
                    &runtime,
                    &mut projector,
                    &sender,
                    &mut events,
                    settled_events,
                    model_turns_run,
                )
                .await?;
                return Ok(finish_hook_result(
                    &runtime,
                    status,
                    events,
                    model_turns_run,
                    final_output,
                )
                .await);
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

        match outcome {
            StepOutcome::Completed => {
                loop {
                    if let Some(notification_input) =
                        take_subagent_notification_input(&runtime).await
                    {
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
                        break;
                    }
                    if let Some(deferred_user_input) = deferred_user_input.take() {
                        next_input = Some(deferred_user_input);
                        break;
                    }
                    if !runtime.has_active_subagents().await {
                        break;
                    }
                    let Some(notify) = runtime.subagent_completion_notify() else {
                        break;
                    };
                    let notified = notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if runtime.has_subagent_completion_notifications().await
                        || !runtime.has_active_subagents().await
                    {
                        continue;
                    }
                    tokio::select! {
                        biased;
                        () = loop_token.cancelled() => {
                            drain_subagent_stream_events(
                                &runtime,
                                &mut subagent_event_receiver,
                                &mut projector,
                                &sender,
                                &mut events,
                                model_turns_run,
                            )
                            .await?;
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
                        subagent_event = subagent_event_receiver.recv() => {
                            if let Ok(event) = subagent_event {
                                publish_journal_event(&runtime, &mut projector, &sender, &mut events, event)
                                    .await
                                    .map_err(|source| {
                                        agent_loop_stream_error_with_source(&runtime, model_turns_run, &events, source)
                                    })?;
                            }
                        }
                        () = notified.as_mut() => {}
                    }
                }
                drain_subagent_stream_events(
                    &runtime,
                    &mut subagent_event_receiver,
                    &mut projector,
                    &sender,
                    &mut events,
                    model_turns_run,
                )
                .await?;
                if next_input.is_some() {
                    continue;
                }
                runtime.schedule_memory_maintenance();
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
                    let mut hook_bridge = StreamHookBridge {
                        sender: &sender,
                        receiver: &mut hook_receiver,
                        resolution_epoch: &hook_resolution_epoch,
                        sequence: &mut hook_sequence,
                    };
                    let wave_outcome = match wave {
                        PendingLoopToolWave::Runtime(calls) => {
                            execute_stream_runtime_batch(
                                &runtime,
                                calls,
                                model_turns_run,
                                &loop_token,
                                &loop_permit,
                                &mut projector,
                                &sender,
                                &mut events,
                                Some(&mut hook_bridge),
                            )
                            .await
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
                            receive_and_publish_bridge_tool_results(
                                &runtime,
                                calls,
                                batch_id,
                                model_turns_run,
                                &bridge_resolution_epoch,
                                &loop_token,
                                &loop_permit,
                                &mut bridge_receiver,
                                &mut projector,
                                &sender,
                                &mut events,
                                Some(&mut hook_bridge),
                            )
                            .await
                        }
                    };
                    if let Some(early) = handle_stream_wave_outcome(
                        &runtime,
                        &loop_permit,
                        &mut projector,
                        &sender,
                        &mut events,
                        model_turns_run,
                        wave_outcome,
                    )
                    .await
                    {
                        return early;
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

                    let mut hook_bridge = StreamHookBridge {
                        sender: &sender,
                        receiver: &mut hook_receiver,
                        resolution_epoch: &hook_resolution_epoch,
                        sequence: &mut hook_sequence,
                    };
                    let wave_outcome = match call {
                        PendingLoopToolCall::Runtime(call) => {
                            execute_stream_runtime_batch(
                                &runtime,
                                vec![call],
                                model_turns_run,
                                &loop_token,
                                &loop_permit,
                                &mut projector,
                                &sender,
                                &mut events,
                                Some(&mut hook_bridge),
                            )
                            .await
                        }
                        PendingLoopToolCall::Bridge(call) => {
                            let batch_id = next_agent_run_batch_id(&mut bridge_batch_sequence)
                                .map_err(|source| {
                                    agent_loop_stream_error_with_source(
                                        &runtime,
                                        model_turns_run,
                                        &events,
                                        source,
                                    )
                                })?;
                            receive_and_publish_bridge_tool_results(
                                &runtime,
                                vec![call],
                                batch_id,
                                model_turns_run,
                                &bridge_resolution_epoch,
                                &loop_token,
                                &loop_permit,
                                &mut bridge_receiver,
                                &mut projector,
                                &sender,
                                &mut events,
                                Some(&mut hook_bridge),
                            )
                            .await
                        }
                        PendingLoopToolCall::FinalOutput(_) => {
                            unreachable!("final-output call is handled before continuation budget")
                        }
                    };

                    if let Some(early) = handle_stream_wave_outcome(
                        &runtime,
                        &loop_permit,
                        &mut projector,
                        &sender,
                        &mut events,
                        model_turns_run,
                        wave_outcome,
                    )
                    .await
                    {
                        return early;
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

async fn drain_subagent_stream_events(
    runtime: &Runtime,
    receiver: &mut tokio::sync::broadcast::Receiver<merry_core::RuntimeJournalEvent>,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<merry_core::RuntimeJournalEvent>,
    model_turns_run: usize,
) -> Result<(), AgentLoopError> {
    loop {
        match receiver.try_recv() {
            Ok(event) => {
                publish_journal_event(runtime, projector, sender, events, event)
                    .await
                    .map_err(|source| {
                        agent_loop_stream_error_with_source(
                            runtime,
                            model_turns_run,
                            events,
                            source,
                        )
                    })?;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => return Ok(()),
        }
    }
}
