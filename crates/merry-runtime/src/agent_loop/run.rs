//! Single-consumer stream handle for a runtime-owned agent loop.

use super::{AgentLoopError, AgentLoopResult, agent_loop_stream_error};
use crate::{
    RuntimeError, ToolExecutionOutcome,
    bridge::{BridgeToolResultCommand, BridgeToolResultPayload},
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
/// events and explicit host-owned tool batches from the same ordered channel,
/// so a consumer cannot accidentally skip a tool handoff by using an
/// event-only stream. Runtime state owns the active batch and its lifecycle.
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
}

impl AgentRun {
    pub(crate) fn new(
        session_id: SessionId,
        events: ReceiverStream<AgentRunMessage>,
        loop_token: tokio_util::sync::CancellationToken,
        producer_handle: tokio::task::JoinHandle<()>,
        result_receiver: oneshot::Receiver<Result<AgentLoopResult, AgentLoopError>>,
        bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
        bridge_resolution_epoch: Arc<AtomicU64>,
    ) -> Self {
        let observed_bridge_resolution_epoch = bridge_resolution_epoch.load(Ordering::Acquire);
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
        }
    }

    fn synchronize_bridge_resolution(&mut self) {
        let epoch = self.bridge_resolution_epoch.load(Ordering::Acquire);
        if epoch != self.observed_bridge_resolution_epoch {
            self.pending_tool_invocations = None;
            self.observed_bridge_resolution_epoch = epoch;
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

    /// Returns the collected loop result once the run has completed.
    ///
    /// Unconsumed events are drained. An unresolved host-tool batch is reported
    /// as an error and cancellation is requested so the producer cannot remain
    /// blocked waiting for a result that the caller has abandoned.
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
                Ok(None) => break,
                Err(source) => {
                    self.request_cancel();
                    self.pending_tool_invocations = None;
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
    /// SDK host adapters use this to execute bridge tool calls without
    /// exposing bridge handoff as a public [`RuntimeEvent`].
    pub async fn next_message(&mut self) -> Result<Option<AgentRunMessage>, RuntimeError> {
        self.synchronize_bridge_resolution();
        if let Some(batch) = self.pending_tool_invocations.as_ref() {
            return Err(RuntimeError::AgentRunToolInvocationsPending {
                session_id: self.session_id.clone(),
                batch_id: batch.id().clone(),
            });
        }
        let Some(message) = self.events.next().await else {
            return Ok(None);
        };
        if let AgentRunMessage::ToolInvocations { batch } = &message {
            if batch.calls().is_empty() {
                return Err(RuntimeError::BridgeToolResultBatchEmpty {
                    session_id: self.session_id.clone(),
                });
            }
            self.pending_tool_invocations = Some(batch.clone());
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
}

impl AgentRunMessage {
    #[must_use]
    pub fn as_event(&self) -> Option<&RuntimeEvent> {
        match self {
            Self::Event(event) => Some(event),
            Self::ToolInvocations { .. } => None,
        }
    }

    /// Borrows the runtime-owned tool batch when this message carries a host
    /// invocation handoff.
    #[must_use]
    pub fn as_tool_invocations(&self) -> Option<&PendingToolCallBatch> {
        match self {
            Self::ToolInvocations { batch } => Some(batch),
            Self::Event(_) => None,
        }
    }
}
