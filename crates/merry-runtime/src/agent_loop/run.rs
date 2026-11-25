//! Single-consumer stream handle for a runtime-owned agent loop.

use super::{AgentLoopError, AgentLoopResult, agent_loop_stream_error};
use crate::{
    HookDecision, HookInvocation, HookInvocationId, RuntimeError, ToolExecutionOutcome,
    bridge::{BridgeToolResultCommand, BridgeToolResultPayload},
    hooks::bridge::HookDecisionCommand,
};
use futures_util::StreamExt;
use merry_core::{PendingToolCallBatch, RuntimeEvent, SessionId, ToolCallBatchId, ToolCallId};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

/// Runtime-owned single-consumer protocol for one agent run.
///
/// [`Self::next_message`] is the only output path. It yields durable runtime
/// events, explicit host-owned tool batches, and subscribed host-bridged hook
/// invocations from the same ordered channel.
pub struct AgentRun {
    session_id: SessionId,
    events: ReceiverStream<AgentRunMessage>,
    loop_token: tokio_util::sync::CancellationToken,
    producer_handle: Option<tokio::task::JoinHandle<()>>,
    result_receiver: Option<oneshot::Receiver<Result<AgentLoopResult, AgentLoopError>>>,
    bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
    pending_tool_invocations: Option<PendingToolCallBatch>,
    bridge_resolution_epoch: Arc<AtomicU64>,
    observed_bridge_resolution_epoch: u64,
    hook_sender: mpsc::Sender<HookDecisionCommand>,
    pending_hook_invocation: Option<HookInvocation>,
    hook_resolution_epoch: Arc<AtomicU64>,
    observed_hook_resolution_epoch: u64,
}

impl AgentRun {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session_id: SessionId,
        events: ReceiverStream<AgentRunMessage>,
        loop_token: tokio_util::sync::CancellationToken,
        producer_handle: tokio::task::JoinHandle<()>,
        result_receiver: oneshot::Receiver<Result<AgentLoopResult, AgentLoopError>>,
        bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
        bridge_resolution_epoch: Arc<AtomicU64>,
        hook_sender: mpsc::Sender<HookDecisionCommand>,
        hook_resolution_epoch: Arc<AtomicU64>,
    ) -> Self {
        let observed_bridge_resolution_epoch = bridge_resolution_epoch.load(Ordering::Acquire);
        let observed_hook_resolution_epoch = hook_resolution_epoch.load(Ordering::Acquire);
        Self {
            session_id,
            events,
            loop_token,
            producer_handle: Some(producer_handle),
            result_receiver: Some(result_receiver),
            bridge_sender,
            pending_tool_invocations: None,
            bridge_resolution_epoch,
            observed_bridge_resolution_epoch,
            hook_sender,
            pending_hook_invocation: None,
            hook_resolution_epoch,
            observed_hook_resolution_epoch,
        }
    }

    fn synchronize_bridge_resolution(&mut self) {
        let epoch = self.bridge_resolution_epoch.load(Ordering::Acquire);
        if epoch != self.observed_bridge_resolution_epoch {
            self.pending_tool_invocations = None;
            self.observed_bridge_resolution_epoch = epoch;
        }
        let hook_epoch = self.hook_resolution_epoch.load(Ordering::Acquire);
        if hook_epoch != self.observed_hook_resolution_epoch {
            self.pending_hook_invocation = None;
            self.observed_hook_resolution_epoch = hook_epoch;
        }
    }

    /// Submits a complete batch of host-executed tool outcomes.
    ///
    /// The host may prepare results concurrently, then submit the complete set
    /// in any order. The batch itself does not grant parallel-execution safety;
    /// the runtime validates the complete set and records it in pending-call
    /// order before starting the next model turn. Correctable validation errors
    /// keep the batch pending; a non-correctable recording error is converted
    /// into a failed tool result so the model loop can still recover.
    pub async fn submit_bridge_tool_outcomes(
        &mut self,
        batch_id: &ToolCallBatchId,
        outcomes: Vec<(ToolCallId, ToolExecutionOutcome)>,
    ) -> Result<(), RuntimeError> {
        let Some(expected_batch) = self.pending_tool_invocations.as_ref() else {
            return Err(RuntimeError::NoPendingAgentRunToolInvocations {
                session_id: self.session_id.clone(),
            });
        };
        if expected_batch.id() != batch_id {
            return Err(RuntimeError::BridgeToolResultBatchIdMismatch {
                session_id: self.session_id.clone(),
                expected_batch_id: expected_batch.id().clone(),
                received_batch_id: batch_id.clone(),
            });
        }
        if outcomes.is_empty() {
            return Err(RuntimeError::BridgeToolResultBatchEmpty {
                session_id: self.session_id.clone(),
            });
        }
        let result = self
            .submit_bridge_command(BridgeToolResultPayload::Outcomes {
                batch_id: batch_id.clone(),
                outcomes,
            })
            .await;
        match &result {
            Ok(()) => self.pending_tool_invocations = None,
            Err(error) if error.is_retryable_bridge_tool_result() => {}
            Err(_) => self.pending_tool_invocations = None,
        }
        result
    }

    async fn submit_bridge_command(
        &self,
        payload: BridgeToolResultPayload,
    ) -> Result<(), RuntimeError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        let command = BridgeToolResultCommand {
            payload,
            ack_sender,
        };
        self.bridge_sender
            .send(command)
            .await
            .map_err(|_| RuntimeError::AgentRunClosed {
                session_id: self.session_id.clone(),
                message: "agent run closed before accepting the bridge tool result",
            })?;
        ack_receiver
            .await
            .map_err(|_| RuntimeError::AgentRunClosed {
                session_id: self.session_id.clone(),
                message: "agent run closed before acknowledging the bridge tool result",
            })?
    }

    /// Submits a typed decision for the active host-bridged hook invocation.
    pub async fn submit_hook_decision(
        &mut self,
        invocation_id: &HookInvocationId,
        decision: HookDecision,
    ) -> Result<(), RuntimeError> {
        self.submit_hook_outcome(invocation_id, Ok(decision)).await
    }

    /// Reports a fail-closed host hook error for the active hook invocation.
    pub async fn fail_hook_invocation(
        &mut self,
        invocation_id: &HookInvocationId,
        message: String,
    ) -> Result<(), RuntimeError> {
        self.submit_hook_outcome(invocation_id, Err(message)).await
    }

    async fn submit_hook_outcome(
        &mut self,
        invocation_id: &HookInvocationId,
        outcome: Result<HookDecision, String>,
    ) -> Result<(), RuntimeError> {
        self.synchronize_bridge_resolution();
        let Some(expected) = self.pending_hook_invocation.as_ref() else {
            return Err(RuntimeError::NoPendingAgentRunHookInvocation {
                session_id: self.session_id.clone(),
            });
        };
        if expected.id() != invocation_id {
            return Err(RuntimeError::HookInvocationIdMismatch {
                session_id: self.session_id.clone(),
                expected_invocation_id: expected.id().clone(),
                received_invocation_id: invocation_id.clone(),
            });
        }
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.hook_sender
            .send(HookDecisionCommand {
                invocation_id: invocation_id.clone(),
                outcome,
                ack_sender,
            })
            .await
            .map_err(|_| RuntimeError::AgentRunClosed {
                session_id: self.session_id.clone(),
                message: "agent run closed before accepting the hook decision",
            })?;
        let result = ack_receiver
            .await
            .map_err(|_| RuntimeError::AgentRunClosed {
                session_id: self.session_id.clone(),
                message: "agent run closed before acknowledging the hook decision",
            })?;
        if result.is_ok() {
            self.pending_hook_invocation = None;
        }
        result
    }

    /// Returns the collected loop result once the run has completed.
    ///
    /// Unconsumed events are drained. An unresolved host-tool batch or hook
    /// invocation is reported as an error and cancellation is requested.
    pub async fn result(&mut self) -> Result<AgentLoopResult, AgentLoopError> {
        loop {
            match self.next_message().await {
                Ok(Some(AgentRunMessage::Event(_))) => {}
                Ok(Some(AgentRunMessage::ToolInvocations { batch: _ })) => {
                    self.request_cancel();
                    self.pending_tool_invocations = None;
                    return Err(agent_loop_stream_error(
                        &self.session_id,
                        Vec::new(),
                        "agent run result requested before the host tool batch was resolved",
                    ));
                }
                Ok(Some(AgentRunMessage::HookInvocation { .. })) => {
                    self.request_cancel();
                    self.pending_hook_invocation = None;
                    return Err(agent_loop_stream_error(
                        &self.session_id,
                        Vec::new(),
                        "agent run result requested before the hook invocation was resolved",
                    ));
                }
                Ok(None) => break,
                Err(source) => {
                    self.request_cancel();
                    self.pending_tool_invocations = None;
                    self.pending_hook_invocation = None;
                    return Err(AgentLoopError::new(Vec::new(), source));
                }
            }
        }

        let Some(result_receiver) = self.result_receiver.take() else {
            return Err(agent_loop_stream_error(
                &self.session_id,
                Vec::new(),
                "agent loop stream result was already consumed",
            ));
        };
        match result_receiver.await {
            Ok(result) => result,
            Err(_) => Err(agent_loop_stream_error(
                &self.session_id,
                Vec::new(),
                "agent loop stream producer stopped before returning a result",
            )),
        }
    }

    /// Cancels the loop producer and waits until its task has stopped.
    ///
    /// This is the output-boundary cancellation path: once a consumer cannot
    /// accept another event, callers must stop provider/tool work before
    /// settling and persisting the runtime state.
    pub async fn cancel_and_wait(&mut self) {
        self.loop_token.cancel();
        self.pending_tool_invocations = None;
        self.pending_hook_invocation = None;
        // A producer may be waiting for capacity while publishing a durable
        // event. Drain the bounded channel so cancellation can reach the
        // runtime checkpoint instead of relying on task abortion.
        while self.events.next().await.is_some() {}
        if let Some(handle) = self.producer_handle.take() {
            let _ = handle.await;
        }
    }

    /// Returns the next runtime-owned run message.
    ///
    /// SDK host adapters use this to execute bridge tool calls and subscribed
    /// host hooks without exposing bridge handoff as a public [`RuntimeEvent`].
    pub async fn next_message(&mut self) -> Result<Option<AgentRunMessage>, RuntimeError> {
        self.synchronize_bridge_resolution();
        if let Some(batch) = self.pending_tool_invocations.as_ref() {
            return Err(RuntimeError::AgentRunToolInvocationsPending {
                session_id: self.session_id.clone(),
                batch_id: batch.id().clone(),
            });
        }
        if let Some(invocation) = self.pending_hook_invocation.as_ref() {
            return Err(RuntimeError::AgentRunHookInvocationPending {
                session_id: self.session_id.clone(),
                invocation_id: invocation.id().clone(),
            });
        }
        let Some(message) = self.events.next().await else {
            return Ok(None);
        };
        match &message {
            AgentRunMessage::ToolInvocations { batch } => {
                if batch.calls().is_empty() {
                    return Err(RuntimeError::BridgeToolResultBatchEmpty {
                        session_id: self.session_id.clone(),
                    });
                }
                self.pending_tool_invocations = Some(batch.clone());
            }
            AgentRunMessage::HookInvocation { invocation } => {
                self.pending_hook_invocation = Some(invocation.clone());
            }
            AgentRunMessage::Event(_) => {}
        }
        Ok(Some(message))
    }

    /// Returns the next runtime-owned run message.
    ///
    /// This Rust convenience alias has the same message-first semantics as
    /// [`Self::next_message`]; it never filters host-tool handoffs.
    pub async fn next(&mut self) -> Result<Option<AgentRunMessage>, RuntimeError> {
        self.next_message().await
    }

    /// Requests cancellation without waiting for the producer task to stop.
    ///
    /// This is intended for synchronous cleanup guards that cannot await, such
    /// as a facade tool-invocation lease being dropped before it is resolved.
    /// Callers that own an async lifecycle should prefer [`Self::cancel_and_wait`]
    /// so producer completion and the terminal result are observed explicitly.
    pub fn request_cancel(&mut self) {
        self.loop_token.cancel();
        self.pending_tool_invocations = None;
        self.pending_hook_invocation = None;
    }
}

impl Drop for AgentRun {
    fn drop(&mut self) {
        self.loop_token.cancel();
        if let Some(handle) = self.producer_handle.take() {
            handle.abort();
        }
    }
}

/// Message emitted by an [`AgentRun`].
// Keep the public event inline to avoid an allocation for every streamed event.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentRunMessage {
    /// Public SDK/UI event.
    Event(RuntimeEvent),
    /// Ordered host-owned tool calls from one runtime execution wave.
    ///
    /// Runtime-owned calls from the same model response are executed internally
    /// and are not emitted here. A single host call is represented as a
    /// one-call batch; calls are never combined across model responses or
    /// separate run reads.
    ToolInvocations {
        /// Runtime-owned batch ID and host-owned calls in provider/model order.
        /// Every call must be resolved before the run can request the next
        /// runtime execution wave or model response.
        batch: PendingToolCallBatch,
    },
    /// Subscribed host-bridged lifecycle hook awaiting a typed decision.
    ///
    /// Emitted only when the host explicitly subscribed to this hook point
    /// (and matching tool name, for tool hooks).
    HookInvocation {
        /// Pending hook invocation payload and identifier.
        invocation: HookInvocation,
    },
}

impl AgentRunMessage {
    #[must_use]
    pub fn as_event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(event) => Some(event),
            Self::ToolInvocations { .. } | Self::HookInvocation { .. } => None,
        }
    }

    /// Borrows the runtime-owned tool batch when this message carries a host
    /// invocation handoff.
    #[must_use]
    pub fn as_tool_invocations(&self) -> Option<&PendingToolCallBatch> {
        match self {
            Self::ToolInvocations { batch } => Some(batch),
            Self::Event(_) | Self::HookInvocation { .. } => None,
        }
    }

    /// Borrows the hook invocation when this message carries a bridged hook request.
    #[must_use]
    pub fn as_hook_invocation(&self) -> Option<&HookInvocation> {
        match self {
            Self::HookInvocation { invocation } => Some(invocation),
            Self::Event(_) | Self::ToolInvocations { .. } => None,
        }
    }
}
