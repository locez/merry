//! Three-stage tool-wave hook orchestration shared across agent loops.
//!
//! Stage 1 (Pre-Hook): Runs `before_tool_call` serially in model order.
//! Stage 2 (Execute): Caller runs allowed calls (preserving `ParallelSafe` concurrency).
//! Stage 3 (Post-Hook): Runs `after_tool_call` before durable commit/emission.

use super::{
    AfterToolCallContext, AfterToolDecision, BeforeToolCallContext, BeforeToolDecision,
    bridge::{
        StreamHookBridge, evaluate_after_tool_call, evaluate_before_tool_call, has_after_tool_hook,
        has_before_tool_hook,
    },
};
use crate::{Runtime, RuntimeError, ToolExecutionOutcome, events::ActiveStepPermit};
use merry_core::{PendingToolCall, RuntimeJournalEvent, RuntimeJournalPayload, ToolCallId};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

/// Shared loop execution context passed into tool-wave hook orchestration.
pub(crate) struct HookWaveContext<'a> {
    pub(crate) runtime: &'a Runtime,
    pub(crate) model_turn: usize,
    pub(crate) token: &'a CancellationToken,
    pub(crate) loop_permit: &'a ActiveStepPermit,
}

/// Outcome of Stage 1 (`before_tool_call`) across a tool wave.
pub(crate) enum PreToolWaveOutcome {
    /// Proceed to execute `allowed_calls`, after publishing any `pre_events`
    /// produced by `Deny`, `ShortCircuit`, or modified-input validation failures.
    Proceed {
        pre_events: Vec<RuntimeJournalEvent>,
        allowed_calls: Vec<PendingToolCall>,
    },
    /// A hook stopped the turn before remaining tools in the wave executed.
    StoppedByHook {
        pre_events: Vec<RuntimeJournalEvent>,
        reason: String,
        final_output: Option<String>,
    },
}

/// Runs Stage 1 (`before_tool_call`) in model order for a tool wave.
///
/// Calls whose tool name has no matching native or bridge hook pass straight
/// through with zero overhead.
pub(crate) async fn run_pre_tool_wave_hooks(
    wave: &HookWaveContext<'_>,
    calls: Vec<PendingToolCall>,
    is_bridge_tool: bool,
    mut bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<PreToolWaveOutcome, RuntimeError> {
    let runtime = wave.runtime;
    let bridge_enabled = bridge.is_some();
    let mut pre_events = Vec::new();
    let mut allowed_calls = Vec::with_capacity(calls.len());

    for call in calls {
        if !has_before_tool_hook(runtime, call.name(), bridge_enabled) {
            allowed_calls.push(call);
            continue;
        }

        let context = BeforeToolCallContext {
            session_id: runtime.session_id().clone(),
            model_turn: wave.model_turn,
            call_id: call.id().clone(),
            tool_name: call.name().clone(),
            arguments: call.arguments().clone(),
            is_bridge_tool,
        };
        let decision =
            evaluate_before_tool_call(runtime, context, wave.token, bridge.as_deref_mut()).await?;

        match decision {
            BeforeToolDecision::Allow => {
                allowed_calls.push(call);
            }
            BeforeToolDecision::ModifyInput { arguments } => {
                let candidate =
                    PendingToolCall::new(call.id().clone(), call.name().clone(), arguments.clone());
                if let Some(Err(validation_error)) =
                    runtime.validate_registered_tool_input(&candidate)
                {
                    let mut events = runtime
                        .submit_tool_input_validation_failure_with_active_permit(
                            &candidate,
                            validation_error,
                            wave.loop_permit,
                        )
                        .await?;
                    pre_events.append(&mut events);
                } else {
                    let updated = runtime
                        .replace_pending_tool_call_arguments_with_active_permit(
                            call.id(),
                            arguments,
                            wave.loop_permit,
                        )
                        .await?;
                    allowed_calls.push(updated);
                }
            }
            BeforeToolDecision::Deny { message } => {
                let mut events = runtime
                    .submit_hook_denied_tool_call_with_active_permit(
                        &call,
                        &message,
                        wave.loop_permit,
                    )
                    .await?;
                pre_events.append(&mut events);
            }
            BeforeToolDecision::ShortCircuit { output } => {
                let mut events = runtime
                    .submit_hook_short_circuit_tool_call_with_active_permit(
                        call.id(),
                        output,
                        wave.loop_permit,
                    )
                    .await?;
                pre_events.append(&mut events);
            }
            BeforeToolDecision::StopTurn {
                reason,
                final_output,
            } => {
                return Ok(PreToolWaveOutcome::StoppedByHook {
                    pre_events,
                    reason,
                    final_output,
                });
            }
        }
    }

    Ok(PreToolWaveOutcome::Proceed {
        pre_events,
        allowed_calls,
    })
}

/// Outcome of Stage 3 (`after_tool_call`) for a bridge tool batch before commit.
pub(crate) enum PostBridgeWaveOutcome {
    /// Commit all outcomes and continue the turn.
    Continue(Vec<(ToolCallId, ToolExecutionOutcome)>),
    /// Commit `outcomes_to_commit`, settle remaining pending calls, and stop the turn.
    StoppedByHook {
        outcomes_to_commit: Vec<(ToolCallId, ToolExecutionOutcome)>,
        reason: String,
        final_output: Option<String>,
    },
}

/// Runs Stage 3 (`after_tool_call`) for host-executed bridge tool outcomes before durable commit.
pub(crate) async fn run_post_bridge_tool_hooks(
    runtime: &Runtime,
    calls: &[PendingToolCall],
    outcomes: Vec<(ToolCallId, ToolExecutionOutcome)>,
    model_turn: usize,
    token: &CancellationToken,
    mut bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<PostBridgeWaveOutcome, RuntimeError> {
    let bridge_enabled = bridge.is_some();
    if !calls
        .iter()
        .any(|call| has_after_tool_hook(runtime, call.name(), bridge_enabled))
    {
        return Ok(PostBridgeWaveOutcome::Continue(outcomes));
    }

    let calls_by_id: BTreeMap<&ToolCallId, &PendingToolCall> =
        calls.iter().map(|call| (call.id(), call)).collect();
    let mut final_outcomes = Vec::with_capacity(outcomes.len());

    for (call_id, mut outcome) in outcomes {
        let Some(call) = calls_by_id.get(&call_id).copied() else {
            final_outcomes.push((call_id, outcome));
            continue;
        };
        if !has_after_tool_hook(runtime, call.name(), bridge_enabled) {
            final_outcomes.push((call_id, outcome));
            continue;
        }

        let context = AfterToolCallContext {
            session_id: runtime.session_id().clone(),
            model_turn,
            call_id: call.id().clone(),
            tool_name: call.name().clone(),
            arguments: call.arguments().clone(),
            status: outcome.status(),
            output: outcome.content().clone(),
            diagnostic: outcome.diagnostic().cloned(),
            is_bridge_tool: true,
        };
        let decision =
            evaluate_after_tool_call(runtime, context, token, bridge.as_deref_mut()).await?;

        match decision {
            AfterToolDecision::Continue => {
                final_outcomes.push((call_id, outcome));
            }
            AfterToolDecision::ModifyOutput { output } => {
                outcome.replace_output(output);
                final_outcomes.push((call_id, outcome));
            }
            AfterToolDecision::StopTurn {
                reason,
                final_output,
            } => {
                final_outcomes.push((call_id, outcome));
                return Ok(PostBridgeWaveOutcome::StoppedByHook {
                    outcomes_to_commit: final_outcomes,
                    reason,
                    final_output,
                });
            }
        }
    }

    Ok(PostBridgeWaveOutcome::Continue(final_outcomes))
}

/// Outcome of Stage 3 (`after_tool_call`) for runtime-executed tool events.
pub(crate) enum PostRuntimeToolOutcome {
    Continue,
    StoppedByHook {
        reason: String,
        final_output: Option<String>,
    },
}

/// Evaluates `after_tool_call` for any `ToolCallResolved` events produced by a runtime tool wave.
pub(crate) async fn run_post_runtime_tool_hooks(
    wave: &HookWaveContext<'_>,
    calls: &[PendingToolCall],
    events: &[RuntimeJournalEvent],
    mut bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<PostRuntimeToolOutcome, RuntimeError> {
    let runtime = wave.runtime;
    let bridge_enabled = bridge.is_some();
    if !calls
        .iter()
        .any(|call| has_after_tool_hook(runtime, call.name(), bridge_enabled))
    {
        return Ok(PostRuntimeToolOutcome::Continue);
    }

    let calls_by_id: BTreeMap<&ToolCallId, &PendingToolCall> =
        calls.iter().map(|call| (call.id(), call)).collect();

    for event in events {
        let RuntimeJournalPayload::ToolCallResolved { result } = &event.payload else {
            continue;
        };
        let Some(call) = calls_by_id.get(result.call_id()).copied() else {
            continue;
        };
        if !has_after_tool_hook(runtime, call.name(), bridge_enabled) {
            continue;
        }
        let Some(output) = runtime.read_tool_output(call.id()).await? else {
            continue;
        };
        let context = AfterToolCallContext {
            session_id: runtime.session_id().clone(),
            model_turn: wave.model_turn,
            call_id: call.id().clone(),
            tool_name: call.name().clone(),
            arguments: call.arguments().clone(),
            status: result.status(),
            output,
            diagnostic: result.diagnostic().cloned(),
            is_bridge_tool: false,
        };
        let decision =
            evaluate_after_tool_call(runtime, context, wave.token, bridge.as_deref_mut()).await?;

        match decision {
            AfterToolDecision::Continue => {}
            AfterToolDecision::ModifyOutput { output } => {
                runtime
                    .replace_tool_result_output_with_active_permit(
                        call.id(),
                        output,
                        wave.loop_permit,
                    )
                    .await?;
            }
            AfterToolDecision::StopTurn {
                reason,
                final_output,
            } => {
                return Ok(PostRuntimeToolOutcome::StoppedByHook {
                    reason,
                    final_output,
                });
            }
        }
    }

    Ok(PostRuntimeToolOutcome::Continue)
}
