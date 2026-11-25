//! Shared model-step hook orchestration for synchronous and streaming agent loops.

use super::{
    AgentLoopBlockedReason, AgentLoopError, AgentLoopResult, AgentLoopStatus, AgentRunMessage,
    PendingLoopToolCall, StepOutcome, agent_loop_cancelled_diagnostic,
    agent_loop_stream_error_with_source, publish_journal_event,
    tool_execution_cancelled_diagnostic, tool_flow::ToolWaveOutcome,
};
use crate::{
    AfterModelDecision, AfterModelStepContext, BeforeModelDecision, BeforeModelStepContext,
    HookPoint, Runtime, RuntimeError, StepInput,
    events::{ActiveStepPermit, RuntimeEventProjector},
    hooks::bridge::{
        StreamHookBridge, evaluate_after_model_step, evaluate_before_model_step,
        has_after_model_hook, has_before_model_hook,
    },
};
use merry_core::{RuntimeJournalEvent, ToolName};
use merry_llm::GenerationConfig;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Shared runtime/loop metadata passed to step-level hook helpers.
pub(crate) struct StepHookContext<'a> {
    pub(crate) runtime: &'a Runtime,
    pub(crate) model_turn: usize,
    pub(crate) max_model_turns: usize,
    pub(crate) loop_token: &'a CancellationToken,
    pub(crate) loop_permit: &'a ActiveStepPermit,
}

/// Outcome of evaluating `before_model_step` hooks for one model step.
pub(crate) enum BeforeStepHookAction {
    /// Proceed with the model step using the (possibly updated) generation config.
    Proceed(GenerationConfig),
    /// Terminate the loop cleanly (`StoppedByHook` or `Cancelled`).
    Finish {
        status: AgentLoopStatus,
        final_output: Option<String>,
    },
}

/// Evaluates `before_model_step` hooks and applies steering notes, generation
/// overrides, and `force_compact` in place.
pub(crate) async fn run_before_model_step_hook(
    ctx: &StepHookContext<'_>,
    input: &mut StepInput,
    step_generation_config: GenerationConfig,
    bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<BeforeStepHookAction, RuntimeError> {
    let runtime = ctx.runtime;
    if !has_before_model_hook(runtime, bridge.is_some()) {
        return Ok(BeforeStepHookAction::Proceed(step_generation_config));
    }

    let context = BeforeModelStepContext {
        session_id: runtime.session_id().clone(),
        model_turn: ctx.model_turn,
        max_model_turns: ctx.max_model_turns,
        session_usage: runtime.usage().await,
        generation_config: step_generation_config.clone(),
    };
    let decision = match evaluate_before_model_step(runtime, context, ctx.loop_token, bridge).await
    {
        Ok(decision) => decision,
        Err(RuntimeError::HookCancelled { .. }) => {
            return Ok(BeforeStepHookAction::Finish {
                status: AgentLoopStatus::Cancelled {
                    diagnostic: agent_loop_cancelled_diagnostic(),
                },
                final_output: None,
            });
        }
        Err(source) => return Err(source),
    };

    match decision {
        BeforeModelDecision::Continue {
            generation_override,
            dynamic_steering_note,
            force_compact,
        } => {
            let effective_gen = generation_override.unwrap_or(step_generation_config);
            if let Some(note) = dynamic_steering_note
                && !note.trim().is_empty()
            {
                input.append_control_note(&note)?;
            }
            if force_compact {
                let _ = runtime
                    .compact_context_for_hook_with_active_permit(
                        ctx.loop_token.clone(),
                        ctx.loop_permit,
                    )
                    .await?;
            }
            Ok(BeforeStepHookAction::Proceed(effective_gen))
        }
        BeforeModelDecision::StopTurn {
            reason,
            final_output,
        } => Ok(BeforeStepHookAction::Finish {
            status: AgentLoopStatus::StoppedByHook {
                hook_point: HookPoint::BeforeModelStep,
                reason,
            },
            final_output,
        }),
    }
}

/// Outcome of evaluating `after_model_step` hooks for one model step.
pub(crate) enum AfterStepHookAction {
    /// Continue normal runtime control flow for this step outcome.
    Accept,
    /// Continue the loop with a dynamic control message after publishing `settled_events`.
    ContinueWithInput {
        settled_events: Vec<RuntimeJournalEvent>,
        input: StepInput,
    },
    /// Terminate the loop cleanly after publishing `settled_events`.
    Finish {
        settled_events: Vec<RuntimeJournalEvent>,
        status: AgentLoopStatus,
        final_output: Option<String>,
    },
}

/// Returns whether `outcome` is eligible for `after_model_step` hook inspection.
pub(crate) fn outcome_allows_after_model_hook(outcome: &StepOutcome) -> bool {
    matches!(
        outcome,
        StepOutcome::Completed
            | StepOutcome::Pending(
                PendingLoopToolCall::Runtime(_) | PendingLoopToolCall::Bridge(_)
            )
            | StepOutcome::PendingBatch(_)
    )
}

/// Extracts ordered pending tool names from a classified step outcome.
pub(crate) fn pending_tool_names_for_outcome(outcome: &StepOutcome) -> Vec<ToolName> {
    match outcome {
        StepOutcome::Pending(
            PendingLoopToolCall::Runtime(call) | PendingLoopToolCall::Bridge(call),
        ) => vec![call.name().clone()],
        StepOutcome::PendingBatch(calls) => calls
            .iter()
            .map(|call| match call {
                PendingLoopToolCall::Runtime(c)
                | PendingLoopToolCall::Bridge(c)
                | PendingLoopToolCall::FinalOutput(c) => c.name().clone(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Evaluates `after_model_step` hooks and settles pending tool calls on continuation,
/// stop, cancellation, or hook failure.
pub(crate) async fn run_after_model_step_hook(
    ctx: &StepHookContext<'_>,
    outcome: &StepOutcome,
    step_final_output: &Option<String>,
    bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<AfterStepHookAction, RuntimeError> {
    let runtime = ctx.runtime;
    if !has_after_model_hook(runtime, bridge.is_some()) || !outcome_allows_after_model_hook(outcome)
    {
        return Ok(AfterStepHookAction::Accept);
    }

    let context = AfterModelStepContext {
        session_id: runtime.session_id().clone(),
        model_turn: ctx.model_turn,
        max_model_turns: ctx.max_model_turns,
        assistant_text: step_final_output.clone(),
        pending_tool_names: pending_tool_names_for_outcome(outcome),
        is_final_turn: matches!(outcome, StepOutcome::Completed),
        session_usage: runtime.usage().await,
    };

    let decision = match evaluate_after_model_step(runtime, context, ctx.loop_token, bridge).await {
        Ok(decision) => decision,
        Err(RuntimeError::HookCancelled { .. }) => {
            let settled_events = runtime
                .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                    "cancelled during after_model_step hook",
                    ctx.loop_permit,
                )
                .await?;
            return Ok(AfterStepHookAction::Finish {
                settled_events,
                status: AgentLoopStatus::Cancelled {
                    diagnostic: agent_loop_cancelled_diagnostic(),
                },
                final_output: None,
            });
        }
        Err(source) => {
            let _ = runtime
                .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                    "failed during after_model_step hook",
                    ctx.loop_permit,
                )
                .await;
            return Err(source);
        }
    };

    match decision {
        AfterModelDecision::Accept => Ok(AfterStepHookAction::Accept),
        AfterModelDecision::ContinueWithInput { text } => {
            let settled_events = runtime
                .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                    "skipped by after_model_step hook continuation",
                    ctx.loop_permit,
                )
                .await?;
            if ctx.model_turn >= ctx.max_model_turns {
                return Ok(AfterStepHookAction::Finish {
                    settled_events,
                    status: AgentLoopStatus::Blocked {
                        reason: AgentLoopBlockedReason::MaxModelTurnsReached {
                            max_model_turns: ctx.max_model_turns,
                        },
                    },
                    final_output: None,
                });
            }
            let input = StepInput::loop_control_text(&text)?;
            Ok(AfterStepHookAction::ContinueWithInput {
                settled_events,
                input,
            })
        }
        AfterModelDecision::StopTurn {
            reason,
            final_output,
        } => {
            let settled_events = runtime
                .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                    &reason,
                    ctx.loop_permit,
                )
                .await?;
            Ok(AfterStepHookAction::Finish {
                settled_events,
                status: AgentLoopStatus::StoppedByHook {
                    hook_point: HookPoint::AfterModelStep,
                    reason,
                },
                final_output: final_output.or_else(|| step_final_output.clone()),
            })
        }
    }
}

/// Helper building a terminal [`AgentLoopResult`] with current runtime usage.
pub(crate) async fn finish_hook_result(
    runtime: &Runtime,
    status: AgentLoopStatus,
    events: Vec<RuntimeJournalEvent>,
    model_turns_run: usize,
    final_output: Option<String>,
) -> AgentLoopResult {
    let session_usage = runtime.usage().await;
    AgentLoopResult::new(status, events, model_turns_run, final_output, session_usage)
}

/// Publishes a batch of settled journal events on a streaming agent loop.
pub(crate) async fn publish_stream_events(
    runtime: &Runtime,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    new_events: Vec<RuntimeJournalEvent>,
    model_turns_run: usize,
) -> Result<(), AgentLoopError> {
    for event in new_events {
        publish_journal_event(runtime, projector, sender, events, event)
            .await
            .map_err(|source| {
                agent_loop_stream_error_with_source(runtime, model_turns_run, events, source)
            })?;
    }
    Ok(())
}

/// Handles a streaming tool-wave outcome, settling pending calls on hook stops/failures.
pub(crate) async fn handle_stream_wave_outcome(
    runtime: &Runtime,
    loop_permit: &ActiveStepPermit,
    projector: &mut RuntimeEventProjector,
    sender: &mpsc::Sender<AgentRunMessage>,
    events: &mut Vec<RuntimeJournalEvent>,
    model_turns_run: usize,
    wave_outcome: Result<ToolWaveOutcome, RuntimeError>,
) -> Option<Result<AgentLoopResult, AgentLoopError>> {
    match wave_outcome {
        Ok(ToolWaveOutcome::Continue) => None,
        Ok(ToolWaveOutcome::StoppedByHook(stop)) => {
            let settled = match runtime
                .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                    &stop.reason,
                    loop_permit,
                )
                .await
            {
                Ok(settled) => settled,
                Err(source) => {
                    return Some(Err(agent_loop_stream_error_with_source(
                        runtime,
                        model_turns_run,
                        events,
                        source,
                    )));
                }
            };
            if let Err(err) =
                publish_stream_events(runtime, projector, sender, events, settled, model_turns_run)
                    .await
            {
                return Some(Err(err));
            }
            let taken_events = std::mem::take(events);
            Some(Ok(finish_hook_result(
                runtime,
                AgentLoopStatus::StoppedByHook {
                    hook_point: stop.hook_point,
                    reason: stop.reason,
                },
                taken_events,
                model_turns_run,
                stop.final_output,
            )
            .await))
        }
        Ok(ToolWaveOutcome::RuntimeError(RuntimeError::ToolExecutionCancelled {
            call_id, ..
        }))
        | Err(RuntimeError::ToolExecutionCancelled { call_id, .. }) => {
            let taken_events = std::mem::take(events);
            Some(Ok(finish_hook_result(
                runtime,
                AgentLoopStatus::Cancelled {
                    diagnostic: tool_execution_cancelled_diagnostic(&call_id),
                },
                taken_events,
                model_turns_run,
                None,
            )
            .await))
        }
        Err(RuntimeError::HookCancelled { .. }) => {
            let _ = runtime
                .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                    "cancelled during tool hook",
                    loop_permit,
                )
                .await;
            let taken_events = std::mem::take(events);
            Some(Ok(finish_hook_result(
                runtime,
                AgentLoopStatus::Cancelled {
                    diagnostic: agent_loop_cancelled_diagnostic(),
                },
                taken_events,
                model_turns_run,
                None,
            )
            .await))
        }
        Ok(ToolWaveOutcome::RuntimeError(error)) => Some(Err(agent_loop_stream_error_with_source(
            runtime,
            model_turns_run,
            events,
            error,
        ))),
        Err(error) => {
            if matches!(error, RuntimeError::HookFailed { .. }) {
                let _ = runtime
                    .settle_pending_tool_calls_stopped_by_hook_with_active_permit(
                        "aborted after hook failure",
                        loop_permit,
                    )
                    .await;
            }
            Some(Err(agent_loop_stream_error_with_source(
                runtime,
                model_turns_run,
                events,
                error,
            )))
        }
    }
}
