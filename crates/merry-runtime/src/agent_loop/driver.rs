//! Synchronous and streaming entry points for the runtime agent loop.

use super::producer::{
    AgentLoopStreamProducer, AgentLoopStreamProducerInput, run_agent_loop_stream_producer,
};
use super::{
    AgentLoopBlockedReason, AgentLoopConfig, AgentLoopError, AgentLoopResult, AgentLoopStatus,
    AgentRun, PendingLoopToolCall, StepOutcome, blocked_reason_code, can_retry_structured_output,
    classify_step_events, collect_step_events, continuation_step_input,
    final_assistant_output_from_step, record_final_output_tool_call,
    structured_output_failure_result, take_subagent_notification, take_subagent_notification_input,
    tool_execution_cancelled_diagnostic, tool_resolution_artifact_id,
    tool_resolution_diagnostic_code, tool_resolution_is_policy_denied, tool_resolution_status,
    trace_loop_error, trace_loop_finish, validate_final_output,
};
use crate::{Runtime, RuntimeError, StepContext, StepInput, ToolExecutionContext};
use std::sync::{Arc, atomic::AtomicU64};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

impl Runtime {
    /// Runs a bounded runtime-owned agent loop.
    ///
    /// The loop starts with one [`Runtime::step`]. If the step completes, fails,
    /// or is cancelled, the corresponding status is returned with all observed
    /// events. If the step records pending tool calls and more step budget
    /// remains, the loop executes registered runtime tools, appends their
    /// events, and starts a continuation step without adding a new user
    /// message.
    ///
    /// A batch preserves model order around exclusive tools. Adjacent tools
    /// explicitly registered as parallel-safe may execute concurrently up to
    /// the runtime limit; all other tools execute serially. The loop does not
    /// introduce provider conversation state. It owns the runtime active-step
    /// permit for the full step -> tool execution -> continuation sequence.
    /// While the loop is running, cloned runtime handles reject concurrent
    /// direct mutation APIs with [`RuntimeError::StepAlreadyActive`].
    /// Cancellation and generation controls are reused from `context` for
    /// every step and tool execution.
    pub async fn run_agent_loop(
        &self,
        input: StepInput,
        context: StepContext,
        config: AgentLoopConfig,
    ) -> Result<AgentLoopResult, AgentLoopError> {
        let (loop_token, generation_config, context_contract) = context.into_parts();
        let config = config
            .merge_context_final_output_contract(context_contract)
            .map_err(|source| {
                AgentLoopError::new(Vec::new(), RuntimeError::AgentLoopConfig { source })
            })?;
        let loop_permit = self
            .acquire_active_step_permit()
            .map_err(|source| AgentLoopError::new(Vec::new(), source))?;
        let mut next_input = Some(input);
        let mut deferred_user_input = None;
        let mut events = Vec::new();
        let mut model_turns_run = 0;
        let mut structured_output_retries: usize = 0;

        tracing::info!(
            event = "runtime.loop.start",
            session_id = self.session_id().as_str(),
            max_model_turns = config.max_model_turns(),
            "runtime loop start"
        );

        loop {
            if let Some(notification_input) = take_subagent_notification_input(self).await {
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
            let input = next_input
                .take()
                .expect("agent loop always installs the next step input before continuing");
            let step_index = model_turns_run + 1;
            tracing::info!(
                event = "runtime.step.start",
                session_id = self.session_id().as_str(),
                step_index,
                "runtime loop step start"
            );
            let mut step_context = StepContext::new(loop_token.clone())
                .with_generation_config(generation_config.clone());
            if let Some(contract) = config.final_output_contract().cloned() {
                step_context = step_context.with_final_output_contract(contract);
            }
            let stream =
                match self.step_with_active_permit(input, step_context, loop_permit.clone()) {
                    Ok(stream) => stream,
                    Err(source) => {
                        trace_loop_error(self.session_id().as_str(), model_turns_run, &source);
                        return Err(AgentLoopError::new(events, source));
                    }
                };
            model_turns_run += 1;

            let mut step_events = collect_step_events(stream).await;
            let step_final_output = final_assistant_output_from_step(self, &step_events).await;
            let outcome = classify_step_events(&step_events, config.final_output_contract());
            events.append(&mut step_events);

            match outcome {
                StepOutcome::Completed => {
                    if let Some(notification_input) = take_subagent_notification_input(self).await {
                        if model_turns_run >= config.max_model_turns() {
                            trace_loop_finish(
                                self.session_id().as_str(),
                                "blocked",
                                model_turns_run,
                                Some("max_model_turns_reached"),
                            );
                            let session_usage = self.usage().await;
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
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "completed",
                        model_turns_run,
                        None,
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Completed,
                        events,
                        model_turns_run,
                        step_final_output,
                        session_usage,
                    ));
                }
                StepOutcome::Failed(diagnostic) => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "failed",
                        model_turns_run,
                        Some(diagnostic.code()),
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Failed { diagnostic },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }
                StepOutcome::Cancelled(diagnostic) => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "cancelled",
                        model_turns_run,
                        Some(diagnostic.code()),
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Cancelled { diagnostic },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }
                StepOutcome::Blocked(reason) => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "blocked",
                        model_turns_run,
                        Some(blocked_reason_code(&reason)),
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Blocked { reason },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }
                StepOutcome::Pending(PendingLoopToolCall::Bridge(call)) => {
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "blocked",
                        model_turns_run,
                        Some("bridge_tool_call_requested"),
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new(
                        AgentLoopStatus::Blocked {
                            reason: AgentLoopBlockedReason::BridgeToolCallRequested {
                                call_id: call.id().clone(),
                                tool_name: call.name().clone(),
                            },
                        },
                        events,
                        model_turns_run,
                        None,
                        session_usage,
                    ));
                }
                StepOutcome::ToolResultRecorded => {
                    if model_turns_run >= config.max_model_turns() {
                        trace_loop_finish(
                            self.session_id().as_str(),
                            "blocked",
                            model_turns_run,
                            Some("max_model_turns_reached"),
                        );
                        let session_usage = self.usage().await;
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
                StepOutcome::Pending(PendingLoopToolCall::FinalOutput(call)) => {
                    if let Some((notification_input, notification_text)) =
                        take_subagent_notification(self).await
                    {
                        let mut failure_events = match self
                            .submit_tool_execution_failure_with_active_permit(
                                call.id(),
                                &notification_text,
                                &loop_permit,
                            )
                            .await
                        {
                            Ok(events) => events,
                            Err(source) => {
                                trace_loop_error(
                                    self.session_id().as_str(),
                                    model_turns_run,
                                    &source,
                                );
                                return Err(AgentLoopError::new(events, source));
                            }
                        };
                        events.append(&mut failure_events);
                        if model_turns_run >= config.max_model_turns() {
                            trace_loop_finish(
                                self.session_id().as_str(),
                                "blocked",
                                model_turns_run,
                                Some("max_model_turns_reached"),
                            );
                            let session_usage = self.usage().await;
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
                        let mut failure_events = match self
                            .submit_tool_input_validation_failure_with_active_permit(
                                &call,
                                error,
                                &loop_permit,
                            )
                            .await
                        {
                            Ok(events) => events,
                            Err(source) => {
                                trace_loop_error(
                                    self.session_id().as_str(),
                                    model_turns_run,
                                    &source,
                                );
                                return Err(AgentLoopError::new(events, source));
                            }
                        };
                        events.append(&mut failure_events);
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
                            self,
                            events,
                            model_turns_run,
                            error_message,
                        )
                        .await);
                    }

                    if let Some(contract) = config.final_output_contract()
                        && let Err(error_message) = validate_final_output(contract, &call)
                    {
                        let mut failure_events = match self
                            .submit_structured_output_failure_with_active_permit(
                                call.id(),
                                &error_message,
                                &loop_permit,
                            )
                            .await
                        {
                            Ok(events) => events,
                            Err(source) => {
                                trace_loop_error(
                                    self.session_id().as_str(),
                                    model_turns_run,
                                    &source,
                                );
                                return Err(AgentLoopError::new(events, source));
                            }
                        };
                        events.append(&mut failure_events);
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
                            self,
                            events,
                            model_turns_run,
                            error_message,
                        )
                        .await);
                    }

                    let (final_output, mut final_events) =
                        match record_final_output_tool_call(self, call).await {
                            Ok(recorded) => recorded,
                            Err(source) => {
                                trace_loop_error(
                                    self.session_id().as_str(),
                                    model_turns_run,
                                    &source,
                                );
                                return Err(AgentLoopError::new(events, source));
                            }
                        };
                    events.append(&mut final_events);
                    trace_loop_finish(
                        self.session_id().as_str(),
                        "completed",
                        model_turns_run,
                        None,
                    );
                    let session_usage = self.usage().await;
                    return Ok(AgentLoopResult::new_with_final_output_json(
                        AgentLoopStatus::Completed,
                        events,
                        model_turns_run,
                        None,
                        Some(final_output),
                        session_usage,
                    ));
                }
                StepOutcome::PendingBatch(calls) => {
                    if model_turns_run >= config.max_model_turns() {
                        let session_usage = self.usage().await;
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

                    if let Some(call) = calls.iter().find_map(|call| match call {
                        PendingLoopToolCall::Bridge(call) => Some(call),
                        PendingLoopToolCall::Runtime(_) | PendingLoopToolCall::FinalOutput(_) => {
                            None
                        }
                    }) {
                        let session_usage = self.usage().await;
                        return Ok(AgentLoopResult::new(
                            AgentLoopStatus::Blocked {
                                reason: AgentLoopBlockedReason::BridgeToolCallRequested {
                                    call_id: call.id().clone(),
                                    tool_name: call.name().clone(),
                                },
                            },
                            events,
                            model_turns_run,
                            None,
                            session_usage,
                        ));
                    }

                    let runtime_calls = calls
                        .into_iter()
                        .map(|call| match call {
                            PendingLoopToolCall::Runtime(call) => call,
                            PendingLoopToolCall::FinalOutput(_) => {
                                unreachable!(
                                    "mixed final-output batches are rejected by provider step"
                                )
                            }
                            PendingLoopToolCall::Bridge(_) => {
                                unreachable!("bridge batches return before runtime execution")
                            }
                        })
                        .collect();
                    let execution = self
                        .execute_tool_call_batch_with_active_permit(
                            runtime_calls,
                            ToolExecutionContext::new(loop_token.clone()),
                            &loop_permit,
                        )
                        .await;
                    let (mut execution_events, error) = execution.into_parts();
                    events.append(&mut execution_events);

                    if let Some(error) = error {
                        if let RuntimeError::ToolExecutionCancelled { call_id, .. } = error {
                            let session_usage = self.usage().await;
                            return Ok(AgentLoopResult::new(
                                AgentLoopStatus::Cancelled {
                                    diagnostic: tool_execution_cancelled_diagnostic(&call_id),
                                },
                                events,
                                model_turns_run,
                                None,
                                session_usage,
                            ));
                        }
                        trace_loop_error(self.session_id().as_str(), model_turns_run, &error);
                        return Err(AgentLoopError::new(events, error));
                    }

                    next_input = Some(continuation_step_input());
                }
                StepOutcome::Pending(PendingLoopToolCall::Runtime(call)) => {
                    if model_turns_run >= config.max_model_turns() {
                        trace_loop_finish(
                            self.session_id().as_str(),
                            "blocked",
                            model_turns_run,
                            Some("max_model_turns_reached"),
                        );
                        let session_usage = self.usage().await;
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

                    tracing::info!(
                        event = "runtime.tool.pending",
                        session_id = self.session_id().as_str(),
                        step_index = model_turns_run,
                        tool_call_id = call.id().as_str(),
                        tool_name = call.name().as_str(),
                        "runtime loop saw pending tool"
                    );
                    tracing::info!(
                        event = "runtime.tool.execute.start",
                        session_id = self.session_id().as_str(),
                        step_index = model_turns_run,
                        tool_call_id = call.id().as_str(),
                        tool_name = call.name().as_str(),
                        "runtime loop tool execution start"
                    );
                    match self
                        .execute_tool_call_with_active_permit(
                            call.id(),
                            ToolExecutionContext::new(loop_token.clone()),
                            &loop_permit,
                        )
                        .await
                    {
                        Ok(execution_events) => {
                            if !tool_resolution_is_policy_denied(&execution_events) {
                                tracing::info!(
                                    event = "runtime.tool.execute.finish",
                                    session_id = self.session_id().as_str(),
                                    step_index = model_turns_run,
                                    tool_call_id = call.id().as_str(),
                                    tool_name = call.name().as_str(),
                                    status = tool_resolution_status(&execution_events),
                                    artifact_id = tool_resolution_artifact_id(&execution_events),
                                    diagnostic_code =
                                        tool_resolution_diagnostic_code(&execution_events)
                                            .unwrap_or(""),
                                    "runtime loop tool execution finish"
                                );
                            }
                            events.extend(execution_events);
                        }
                        Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
                            trace_loop_finish(
                                self.session_id().as_str(),
                                "cancelled",
                                model_turns_run,
                                Some("tool_execution_cancelled"),
                            );
                            let session_usage = self.usage().await;
                            return Ok(AgentLoopResult::new(
                                AgentLoopStatus::Cancelled {
                                    diagnostic: tool_execution_cancelled_diagnostic(&call_id),
                                },
                                events,
                                model_turns_run,
                                None,
                                session_usage,
                            ));
                        }
                        Err(source) => {
                            trace_loop_error(self.session_id().as_str(), model_turns_run, &source);
                            return Err(AgentLoopError::new(events, source));
                        }
                    }

                    next_input = Some(continuation_step_input());
                }
            }
        }
    }

    /// Starts a bounded runtime-owned agent loop and returns live events.
    ///
    /// This has the same loop semantics as [`Runtime::run_agent_loop`], but it
    /// returns an [`AgentRun`] handle that yields each observed
    /// [`RuntimeJournalEvent`] as soon as the underlying step or tool execution
    /// produces it. Dropping the handle cancels the loop token and aborts the
    /// loop producer as a final cleanup guard.
    pub fn run_agent_loop_stream(
        &self,
        input: StepInput,
        context: StepContext,
        config: AgentLoopConfig,
    ) -> Result<AgentRun, RuntimeError> {
        let (parent_token, generation_config, context_contract) = context.into_parts();
        let config = config
            .merge_context_final_output_contract(context_contract)
            .map_err(|source| RuntimeError::AgentLoopConfig { source })?;
        let loop_permit = self.acquire_active_step_permit()?;
        let loop_token = parent_token.child_token();
        let producer_token = loop_token.clone();
        let (sender, receiver) = mpsc::channel(16);
        let (result_sender, result_receiver) = oneshot::channel();
        let (bridge_sender, bridge_receiver) = mpsc::channel(1);
        let bridge_resolution_epoch = Arc::new(AtomicU64::new(0));
        let producer_bridge_resolution_epoch = Arc::clone(&bridge_resolution_epoch);
        let runtime = self.clone();
        let session_id = self.session_id().clone();
        let producer_handle = tokio::spawn(async move {
            let result = run_agent_loop_stream_producer(AgentLoopStreamProducer::new(
                AgentLoopStreamProducerInput {
                    runtime,
                    input,
                    loop_token: producer_token,
                    generation_config,
                    config,
                    loop_permit,
                    sender,
                    bridge_receiver,
                    bridge_resolution_epoch: producer_bridge_resolution_epoch,
                },
            ))
            .await;
            let _ = result_sender.send(result);
        });

        Ok(AgentRun::new(
            session_id,
            ReceiverStream::new(receiver),
            loop_token,
            producer_handle,
            result_receiver,
            bridge_sender,
            bridge_resolution_epoch,
        ))
    }
}
