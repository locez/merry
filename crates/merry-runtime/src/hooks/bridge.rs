//! Internal channel bridge and unified native+bridge hook dispatcher.

use super::{
    AfterModelDecision, AfterModelStepContext, AfterToolCallContext, AfterToolDecision,
    BeforeModelDecision, BeforeModelStepContext, BeforeToolCallContext, BeforeToolDecision,
    HookDecision, HookError, HookInvocation, HookInvocationId, HookPoint, HookRequest,
};
use crate::{AgentRunMessage, Runtime, RuntimeError};
use merry_core::ToolName;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Host-to-runtime command resolving a pending bridged hook invocation.
pub(crate) struct HookDecisionCommand {
    pub(crate) invocation_id: HookInvocationId,
    pub(crate) outcome: Result<HookDecision, String>,
    pub(crate) ack_sender: oneshot::Sender<Result<(), RuntimeError>>,
}

/// Evaluates hooks across both in-process Rust handlers and optional host bridges.
///
/// Unsubscribed points and non-matching tools return immediately without
/// allocating a `HookInvocation` or touching the run stream.
pub(crate) struct StreamHookBridge<'a> {
    pub(crate) sender: &'a mpsc::Sender<AgentRunMessage>,
    pub(crate) receiver: &'a mut mpsc::Receiver<HookDecisionCommand>,
    pub(crate) resolution_epoch: &'a AtomicU64,
    pub(crate) sequence: &'a mut u64,
}

impl StreamHookBridge<'_> {
    fn next_invocation_id(&mut self) -> HookInvocationId {
        let id = HookInvocationId::from_sequence(*self.sequence);
        *self.sequence = self.sequence.saturating_add(1);
        id
    }

    pub(crate) async fn invoke(
        &mut self,
        runtime: &Runtime,
        request: HookRequest,
        token: &CancellationToken,
    ) -> Result<HookDecision, RuntimeError> {
        let expected_point = request.point();
        let invocation_id = self.next_invocation_id();
        let invocation = HookInvocation::new(invocation_id.clone(), request);
        self.sender
            .send(AgentRunMessage::HookInvocation { invocation })
            .await
            .map_err(|_| RuntimeError::AgentRunClosed {
                session_id: runtime.session_id().clone(),
                message: "hook invocation receiver closed before the request was delivered",
            })?;

        loop {
            let command = tokio::select! {
                biased;
                () = token.cancelled() => {
                    self.resolution_epoch.fetch_add(1, Ordering::AcqRel);
                    return Err(RuntimeError::HookCancelled {
                        session_id: runtime.session_id().clone(),
                        hook_point: expected_point,
                    });
                }
                cmd = self.receiver.recv() => cmd,
            };
            let Some(HookDecisionCommand {
                invocation_id: received_id,
                outcome,
                ack_sender,
            }) = command
            else {
                self.resolution_epoch.fetch_add(1, Ordering::AcqRel);
                return Err(RuntimeError::AgentRunClosed {
                    session_id: runtime.session_id().clone(),
                    message: "hook decision channel closed before invocation was resolved",
                });
            };

            if received_id != invocation_id {
                let err = RuntimeError::HookInvocationIdMismatch {
                    session_id: runtime.session_id().clone(),
                    expected_invocation_id: invocation_id.clone(),
                    received_invocation_id: received_id,
                };
                let _ = ack_sender.send(Err(err));
                continue;
            }

            match outcome {
                Ok(decision) => {
                    if decision.point() != expected_point {
                        let err = RuntimeError::HookFailed {
                            session_id: runtime.session_id().clone(),
                            hook_point: expected_point,
                            message: HookError::DecisionTypeMismatch {
                                expected: expected_point,
                                actual: decision.point(),
                            }
                            .to_string(),
                        };
                        let _ = ack_sender.send(Err(err));
                        continue;
                    }
                    if let Err(validation_err) = decision.validate() {
                        let err = RuntimeError::HookFailed {
                            session_id: runtime.session_id().clone(),
                            hook_point: expected_point,
                            message: validation_err.to_string(),
                        };
                        let _ = ack_sender.send(Err(err));
                        continue;
                    }
                    self.resolution_epoch.fetch_add(1, Ordering::AcqRel);
                    let _ = ack_sender.send(Ok(()));
                    return Ok(decision);
                }
                Err(message) => {
                    self.resolution_epoch.fetch_add(1, Ordering::AcqRel);
                    let _ = ack_sender.send(Ok(()));
                    return Err(RuntimeError::HookFailed {
                        session_id: runtime.session_id().clone(),
                        hook_point: expected_point,
                        message,
                    });
                }
            }
        }
    }
}

/// Evaluates `before_model_step` across native Rust handler and optional host bridge.
pub(crate) async fn evaluate_before_model_step(
    runtime: &Runtime,
    mut context: BeforeModelStepContext,
    token: &CancellationToken,
    bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<BeforeModelDecision, RuntimeError> {
    let mut merged_generation = None;
    let mut notes: Vec<String> = Vec::new();
    let mut force_compact = false;

    if let Some(handler) = runtime.native_hook_handler()
        && handler.subscriptions().matches_before_model_step()
    {
        let decision = handler
            .before_model_step(context.clone(), token.clone())
            .await
            .map_err(|err| map_hook_error(runtime, HookPoint::BeforeModelStep, err))?;
        match decision {
            BeforeModelDecision::Continue {
                generation_override,
                dynamic_steering_note,
                force_compact: compact,
            } => {
                if let Some(generation_cfg) = generation_override {
                    context.generation_config = generation_cfg.clone();
                    merged_generation = Some(generation_cfg);
                }
                if let Some(note) = dynamic_steering_note
                    && !note.trim().is_empty()
                {
                    notes.push(note);
                }
                force_compact |= compact;
            }
            stop @ BeforeModelDecision::StopTurn { .. } => return Ok(stop),
        }
    }

    if let Some(bridge) = bridge
        && runtime
            .bridge_hook_subscriptions()
            .matches_before_model_step()
    {
        let decision = bridge
            .invoke(runtime, HookRequest::BeforeModelStep(context), token)
            .await?;
        let HookDecision::BeforeModelStep(decision) = decision else {
            return Err(RuntimeError::HookFailed {
                session_id: runtime.session_id().clone(),
                hook_point: HookPoint::BeforeModelStep,
                message: "unexpected hook decision variant".to_owned(),
            });
        };
        match decision {
            BeforeModelDecision::Continue {
                generation_override,
                dynamic_steering_note,
                force_compact: compact,
            } => {
                if let Some(generation_cfg) = generation_override {
                    merged_generation = Some(generation_cfg);
                }
                if let Some(note) = dynamic_steering_note
                    && !note.trim().is_empty()
                {
                    notes.push(note);
                }
                force_compact |= compact;
            }
            stop @ BeforeModelDecision::StopTurn { .. } => return Ok(stop),
        }
    }

    Ok(BeforeModelDecision::Continue {
        generation_override: merged_generation,
        dynamic_steering_note: (!notes.is_empty()).then(|| notes.join("\n\n")),
        force_compact,
    })
}

/// Evaluates `after_model_step` across native Rust handler and optional host bridge.
pub(crate) async fn evaluate_after_model_step(
    runtime: &Runtime,
    context: AfterModelStepContext,
    token: &CancellationToken,
    bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<AfterModelDecision, RuntimeError> {
    if let Some(handler) = runtime.native_hook_handler()
        && handler.subscriptions().matches_after_model_step()
    {
        let decision = handler
            .after_model_step(context.clone(), token.clone())
            .await
            .map_err(|err| map_hook_error(runtime, HookPoint::AfterModelStep, err))?;
        match decision {
            AfterModelDecision::Accept => {}
            short_circuit => return Ok(short_circuit),
        }
    }

    if let Some(bridge) = bridge
        && runtime
            .bridge_hook_subscriptions()
            .matches_after_model_step()
    {
        let decision = bridge
            .invoke(runtime, HookRequest::AfterModelStep(context), token)
            .await?;
        let HookDecision::AfterModelStep(decision) = decision else {
            return Err(RuntimeError::HookFailed {
                session_id: runtime.session_id().clone(),
                hook_point: HookPoint::AfterModelStep,
                message: "unexpected hook decision variant".to_owned(),
            });
        };
        return Ok(decision);
    }

    Ok(AfterModelDecision::Accept)
}

/// Evaluates `before_tool_call` across native Rust handler and optional host bridge.
pub(crate) async fn evaluate_before_tool_call(
    runtime: &Runtime,
    mut context: BeforeToolCallContext,
    token: &CancellationToken,
    bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<BeforeToolDecision, RuntimeError> {
    let mut modified = false;

    if let Some(handler) = runtime.native_hook_handler()
        && handler
            .subscriptions()
            .matches_before_tool_call(&context.tool_name)
    {
        let decision = handler
            .before_tool_call(context.clone(), token.clone())
            .await
            .map_err(|err| map_hook_error(runtime, HookPoint::BeforeToolCall, err))?;
        match decision {
            BeforeToolDecision::Allow => {}
            BeforeToolDecision::ModifyInput { arguments } => {
                context.arguments = arguments;
                modified = true;
            }
            short_circuit => return Ok(short_circuit),
        }
    }

    if let Some(bridge) = bridge
        && runtime
            .bridge_hook_subscriptions()
            .matches_before_tool_call(&context.tool_name)
    {
        let saved_args = modified.then(|| context.arguments.clone());
        let decision = bridge
            .invoke(runtime, HookRequest::BeforeToolCall(context), token)
            .await?;
        let HookDecision::BeforeToolCall(decision) = decision else {
            return Err(RuntimeError::HookFailed {
                session_id: runtime.session_id().clone(),
                hook_point: HookPoint::BeforeToolCall,
                message: "unexpected hook decision variant".to_owned(),
            });
        };
        return Ok(match decision {
            BeforeToolDecision::Allow => match saved_args {
                Some(arguments) => BeforeToolDecision::ModifyInput { arguments },
                None => BeforeToolDecision::Allow,
            },
            other => other,
        });
    }

    if modified {
        Ok(BeforeToolDecision::ModifyInput {
            arguments: context.arguments,
        })
    } else {
        Ok(BeforeToolDecision::Allow)
    }
}

/// Evaluates `after_tool_call` across native Rust handler and optional host bridge.
pub(crate) async fn evaluate_after_tool_call(
    runtime: &Runtime,
    mut context: AfterToolCallContext,
    token: &CancellationToken,
    bridge: Option<&mut StreamHookBridge<'_>>,
) -> Result<AfterToolDecision, RuntimeError> {
    let mut modified = false;

    if let Some(handler) = runtime.native_hook_handler()
        && handler
            .subscriptions()
            .matches_after_tool_call(&context.tool_name)
    {
        let decision = handler
            .after_tool_call(context.clone(), token.clone())
            .await
            .map_err(|err| map_hook_error(runtime, HookPoint::AfterToolCall, err))?;
        match decision {
            AfterToolDecision::Continue => {}
            AfterToolDecision::ModifyOutput { output } => {
                context.output = output;
                modified = true;
            }
            stop @ AfterToolDecision::StopTurn { .. } => return Ok(stop),
        }
    }

    if let Some(bridge) = bridge
        && runtime
            .bridge_hook_subscriptions()
            .matches_after_tool_call(&context.tool_name)
    {
        let saved_output = modified.then(|| context.output.clone());
        let decision = bridge
            .invoke(runtime, HookRequest::AfterToolCall(context), token)
            .await?;
        let HookDecision::AfterToolCall(decision) = decision else {
            return Err(RuntimeError::HookFailed {
                session_id: runtime.session_id().clone(),
                hook_point: HookPoint::AfterToolCall,
                message: "unexpected hook decision variant".to_owned(),
            });
        };
        return Ok(match decision {
            AfterToolDecision::Continue => match saved_output {
                Some(output) => AfterToolDecision::ModifyOutput { output },
                None => AfterToolDecision::Continue,
            },
            other => other,
        });
    }

    if modified {
        Ok(AfterToolDecision::ModifyOutput {
            output: context.output,
        })
    } else {
        Ok(AfterToolDecision::Continue)
    }
}

/// Fast-path check: whether any native or bridge hook cares about `before_model_step`.
pub(crate) fn has_before_model_hook(runtime: &Runtime, bridge_enabled: bool) -> bool {
    runtime
        .native_hook_handler()
        .is_some_and(|handler| handler.subscriptions().matches_before_model_step())
        || (bridge_enabled
            && runtime
                .bridge_hook_subscriptions()
                .matches_before_model_step())
}

/// Fast-path check: whether any native or bridge hook cares about `after_model_step`.
pub(crate) fn has_after_model_hook(runtime: &Runtime, bridge_enabled: bool) -> bool {
    runtime
        .native_hook_handler()
        .is_some_and(|handler| handler.subscriptions().matches_after_model_step())
        || (bridge_enabled
            && runtime
                .bridge_hook_subscriptions()
                .matches_after_model_step())
}

/// Fast-path check: whether any native or bridge hook cares about `before_tool_call` for `tool_name`.
pub(crate) fn has_before_tool_hook(
    runtime: &Runtime,
    tool_name: &ToolName,
    bridge_enabled: bool,
) -> bool {
    runtime
        .native_hook_handler()
        .is_some_and(|handler| handler.subscriptions().matches_before_tool_call(tool_name))
        || (bridge_enabled
            && runtime
                .bridge_hook_subscriptions()
                .matches_before_tool_call(tool_name))
}

/// Fast-path check: whether any native or bridge hook cares about `after_tool_call` for `tool_name`.
pub(crate) fn has_after_tool_hook(
    runtime: &Runtime,
    tool_name: &ToolName,
    bridge_enabled: bool,
) -> bool {
    runtime
        .native_hook_handler()
        .is_some_and(|handler| handler.subscriptions().matches_after_tool_call(tool_name))
        || (bridge_enabled
            && runtime
                .bridge_hook_subscriptions()
                .matches_after_tool_call(tool_name))
}

fn map_hook_error(runtime: &Runtime, hook_point: HookPoint, err: HookError) -> RuntimeError {
    match err {
        HookError::Cancelled => RuntimeError::HookCancelled {
            session_id: runtime.session_id().clone(),
            hook_point,
        },
        other => RuntimeError::HookFailed {
            session_id: runtime.session_id().clone(),
            hook_point,
            message: other.to_string(),
        },
    }
}
