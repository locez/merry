use super::commands::InteractiveCommand;
use super::{
    InteractiveError, InteractiveRunId, InteractiveSettingsUpdate, InterruptReason,
    types::{InputReceipt, InputRecord, InputRecords},
};
use crate::bridge::BridgeToolResultCommand;
use crate::{FileSessionStore, PlanApprovalInput, UserMessageInput};
use merry_core::{
    PendingToolCallBatch, PlanNodeId, QueuedInputLane, RuntimeEvent, ToolCallBatchId, ToolCallId,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

/// Runtime message contract shared by interactive and non-interactive runs.
pub use crate::agent_loop::AgentRunMessage as InteractiveRunMessage;

/// Single-consumer output stream for one interactive run.
///
/// This stream intentionally does not implement `futures::Stream`: a host
/// handoff is a protocol phase, not an ordinary event. Callers use
/// [`Self::next_message`] and must submit the complete batch before reading the
/// next message. This contract is shared by Rust and future foreign-language
/// bindings.
pub struct InteractiveRunEventStream {
    inner: Option<ReceiverStream<InteractiveRunMessage>>,
    cancellation_token: CancellationToken,
    producer_handle: Option<JoinHandle<Result<(), InteractiveError>>>,
    bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
    run_id: InteractiveRunId,
    pending_tool_invocations: Option<PendingToolCallBatch>,
    unread_message: Option<InteractiveRunMessage>,
    bridge_resolution_epoch: Arc<AtomicU64>,
    observed_bridge_resolution_epoch: u64,
    closed_event_observed: bool,
}

impl InteractiveRunEventStream {
    pub(super) fn new(
        run_id: InteractiveRunId,
        inner: ReceiverStream<InteractiveRunMessage>,
        cancellation_token: CancellationToken,
        producer_handle: JoinHandle<Result<(), InteractiveError>>,
        bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
        bridge_resolution_epoch: Arc<AtomicU64>,
    ) -> Self {
        let observed_bridge_resolution_epoch = bridge_resolution_epoch.load(Ordering::Acquire);
        Self {
            inner: Some(inner),
            cancellation_token,
            producer_handle: Some(producer_handle),
            bridge_sender,
            run_id,
            pending_tool_invocations: None,
            unread_message: None,
            bridge_resolution_epoch,
            observed_bridge_resolution_epoch,
            closed_event_observed: false,
        }
    }

    fn synchronize_bridge_resolution(&mut self) {
        let epoch = self.bridge_resolution_epoch.load(Ordering::Acquire);
        if epoch != self.observed_bridge_resolution_epoch {
            self.pending_tool_invocations = None;
            self.unread_message = None;
            self.observed_bridge_resolution_epoch = epoch;
        }
    }

    /// Returns the next ordered event or host tool handoff.
    ///
    /// Calling this while a previous tool batch is unresolved returns an
    /// explicit error. A rejected result submission leaves the same batch
    /// active so the host can correct and retry it.
    pub async fn next_message(
        &mut self,
    ) -> Result<Option<InteractiveRunMessage>, InteractiveError> {
        self.synchronize_bridge_resolution();
        if self.pending_tool_invocations.is_some() {
            return Err(InteractiveError::ToolInvocationsPending {
                run_id: self.run_id,
            });
        }

        let message = self.receive_message().await;
        let Some(message) = message else {
            self.finish_after_eof().await?;
            return Ok(None);
        };

        self.observe_terminal_message(&message);
        self.mark_pending_tool_invocations(&message)?;
        Ok(Some(message))
    }

    async fn receive_message(&mut self) -> Option<InteractiveRunMessage> {
        if let Some(message) = self.unread_message.take() {
            return Some(message);
        }
        use futures_util::StreamExt;
        match self.inner.as_mut() {
            Some(inner) => inner.next().await,
            None => None,
        }
    }

    fn observe_terminal_message(&mut self, message: &InteractiveRunMessage) {
        if matches!(message, InteractiveRunMessage::Event(RuntimeEvent::Closed)) {
            self.closed_event_observed = true;
        }
    }

    async fn finish_producer(&mut self) -> Result<(), InteractiveError> {
        let Some(handle) = self.producer_handle.take() else {
            return Ok(());
        };
        match handle.await {
            Ok(result) => result,
            Err(_) => Err(InteractiveError::ProducerTaskFailed {
                run_id: self.run_id,
            }),
        }
    }

    async fn finish_after_eof(&mut self) -> Result<(), InteractiveError> {
        self.finish_producer().await?;
        if self.closed_event_observed {
            Ok(())
        } else {
            Err(InteractiveError::ProducerStopped {
                run_id: self.run_id,
                message: "output channel closed unexpectedly",
            })
        }
    }

    fn mark_pending_tool_invocations(
        &mut self,
        message: &InteractiveRunMessage,
    ) -> Result<(), InteractiveError> {
        let InteractiveRunMessage::ToolInvocations { batch } = message else {
            return Ok(());
        };
        if batch.calls().is_empty() {
            return Err(InteractiveError::InvalidToolInvocationBatch {
                run_id: self.run_id,
            });
        }
        self.pending_tool_invocations = Some(batch.clone());
        Ok(())
    }

    /// Returns the next runtime event when no host handoff is encountered.
    ///
    /// Hosts that register bridge tools must use [`Self::next_message`] so the
    /// tool batch and its completion path remain visible. If a bridge batch is
    /// encountered, it remains unread and the caller can recover by switching
    /// to `next_message`.
    pub async fn next_event(&mut self) -> Result<Option<RuntimeEvent>, InteractiveError> {
        self.synchronize_bridge_resolution();
        if self.pending_tool_invocations.is_some() {
            return Err(InteractiveError::ToolInvocationsPending {
                run_id: self.run_id,
            });
        }
        let Some(message) = self.receive_message().await else {
            self.finish_after_eof().await?;
            return Ok(None);
        };
        match message {
            InteractiveRunMessage::Event(event) => {
                if matches!(event, RuntimeEvent::Closed) {
                    self.closed_event_observed = true;
                }
                Ok(Some(event))
            }
            InteractiveRunMessage::ToolInvocations { batch } => {
                let count = batch.calls().len();
                self.unread_message = Some(InteractiveRunMessage::ToolInvocations { batch });
                Err(InteractiveError::ToolInvocationsRequireMessageProtocol {
                    run_id: self.run_id,
                    count,
                })
            }
        }
    }

    /// Submits all outcomes for the currently emitted host tool batch.
    ///
    /// The runtime validates call ids, content, and result status. Validation
    /// failures that are safe to correct are returned while the batch remains
    /// pending. If runtime has already recorded the calls as failed, the
    /// rejection is returned but the batch is released and the producer can
    /// continue.
    pub async fn submit_tool_invocation_outcomes(
        &mut self,
        batch_id: &ToolCallBatchId,
        outcomes: Vec<(ToolCallId, crate::ToolExecutionOutcome)>,
    ) -> Result<(), InteractiveError> {
        if self.pending_tool_invocations.is_none() {
            return Err(InteractiveError::NoPendingToolInvocations {
                run_id: self.run_id,
            });
        }
        if outcomes.is_empty() {
            return Err(InteractiveError::InvalidToolInvocationBatch {
                run_id: self.run_id,
            });
        }
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.bridge_sender
            .send(BridgeToolResultCommand::outcomes(
                batch_id.clone(),
                outcomes,
                ack_sender,
            ))
            .await
            .map_err(|_| InteractiveError::RunClosed {
                run_id: self.run_id,
            })?;
        let result = ack_receiver
            .await
            .map_err(|_| InteractiveError::RunClosed {
                run_id: self.run_id,
            })?;
        if result.is_ok() {
            self.pending_tool_invocations = None;
        } else if let Err(error) = result {
            if !error.is_retryable_bridge_tool_result() {
                self.pending_tool_invocations = None;
            }
            return Err(InteractiveError::Runtime { source: error });
        }
        Ok(())
    }

    /// Requests cancellation of the producer without awaiting its shutdown.
    ///
    /// This is used by synchronous drop guards at the facade boundary. Callers
    /// that own the async lifecycle should use `wait_until_closed` afterwards.
    pub fn request_cancel(&self) {
        self.cancellation_token.cancel();
    }

    /// Waits for the interactive producer to emit its terminal closed event.
    ///
    /// Ordinary runtime events are drained. If a host-tool handoff is reached,
    /// this returns [`InteractiveError::ToolInvocationsRequireMessageProtocol`]
    /// without consuming the handoff, so the caller can switch to
    /// [`Self::next_message`] and resolve it. This method is therefore a
    /// lifecycle drain, not an implicit host-tool executor.
    pub async fn wait_until_closed(&mut self) -> Result<(), InteractiveError> {
        while let Some(event) = self.next_event().await? {
            if matches!(event, RuntimeEvent::Closed) {
                break;
            }
        }
        self.inner.take();
        self.finish_producer().await
    }
}

impl Drop for InteractiveRunEventStream {
    fn drop(&mut self) {
        self.inner.take();
        self.cancellation_token.cancel();

        if let Some(handle) = self.producer_handle.take() {
            handle.abort();
        }
    }
}

pub struct InteractiveAgentRun {
    stream: InteractiveRunEventStream,
    input: AgentLoopInput,
    control: AgentLoopControl,
}

impl InteractiveAgentRun {
    pub(super) fn new(
        stream: InteractiveRunEventStream,
        input: AgentLoopInput,
        control: AgentLoopControl,
    ) -> Self {
        Self {
            stream,
            input,
            control,
        }
    }

    #[must_use]
    pub fn split(self) -> (InteractiveRunEventStream, AgentLoopInput, AgentLoopControl) {
        (self.stream, self.input, self.control)
    }
}

#[derive(Clone)]
pub struct AgentLoopInput {
    run_id: InteractiveRunId,
    command_sender: mpsc::Sender<InteractiveCommand>,
}

impl AgentLoopInput {
    pub(super) fn new(
        run_id: InteractiveRunId,
        command_sender: mpsc::Sender<InteractiveCommand>,
    ) -> Self {
        Self {
            run_id,
            command_sender,
        }
    }

    #[must_use]
    pub fn run_id(&self) -> InteractiveRunId {
        self.run_id
    }

    pub async fn submit_next(&self, text: &str) -> Result<InteractiveInputItem, InteractiveError> {
        let message = UserMessageInput::text_only(text)?;
        self.submit_next_message(message).await
    }

    pub async fn submit_next_message(
        &self,
        message: UserMessageInput,
    ) -> Result<InteractiveInputItem, InteractiveError> {
        let text = message.text().to_owned();
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::SubmitNext {
                message,
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
            .map(|receipt| InteractiveInputItem::new(self.clone(), receipt, text))
    }

    pub async fn enqueue(&self, text: &str) -> Result<InteractiveInputItem, InteractiveError> {
        let message = UserMessageInput::text_only(text)?;
        self.enqueue_message(message).await
    }

    pub async fn enqueue_message(
        &self,
        message: UserMessageInput,
    ) -> Result<InteractiveInputItem, InteractiveError> {
        let text = message.text().to_owned();
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Enqueue {
                message,
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
            .map(|receipt| InteractiveInputItem::new(self.clone(), receipt, text))
    }

    async fn update_receipt(
        &self,
        receipt: &InputReceipt,
        text: &str,
    ) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Update {
                id: receipt.id,
                text: text.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    async fn remove_receipt(&self, receipt: &InputReceipt) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Remove {
                id: receipt.id,
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn replace_pending_order(
        &self,
        lane: QueuedInputLane,
        items: &[InteractiveInputItem],
    ) -> Result<(), InteractiveError> {
        if let Some(item) = items.iter().find(|item| item.input.run_id != self.run_id) {
            return Err(InteractiveError::InvalidPendingOrder {
                lane: item.lane(),
                reason: "input item belongs to another interactive run",
            });
        }
        let ids = items.iter().map(|item| item.receipt.id).collect::<Vec<_>>();
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::ReplacePendingOrder {
                lane,
                ids,
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn snapshot(&self) -> Result<InteractiveInputSnapshot, InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Snapshot { ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        let records = ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        Ok(InteractiveInputSnapshot::from_records(
            self.clone(),
            records,
        ))
    }
}

#[derive(Clone)]
pub struct InteractiveInputItem {
    input: AgentLoopInput,
    receipt: InputReceipt,
    text: String,
}

impl InteractiveInputItem {
    fn new(input: AgentLoopInput, receipt: InputReceipt, text: String) -> Self {
        Self {
            input,
            receipt,
            text,
        }
    }

    #[must_use]
    pub fn lane(&self) -> QueuedInputLane {
        self.receipt.lane
    }

    #[must_use]
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    pub async fn update(&mut self, text: impl Into<String>) -> Result<(), InteractiveError> {
        let text = text.into();
        self.input.update_receipt(&self.receipt, &text).await?;
        self.text = text;
        Ok(())
    }

    pub async fn remove(self) -> Result<(), InteractiveError> {
        self.input.remove_receipt(&self.receipt).await
    }
}

#[derive(Clone)]
pub struct InteractiveInputSnapshot {
    pub next: Vec<InteractiveInputItem>,
    pub suspended: Vec<InteractiveInputItem>,
    pub backlog: Vec<InteractiveInputItem>,
}

impl InteractiveInputSnapshot {
    fn from_records(input: AgentLoopInput, records: InputRecords) -> Self {
        Self {
            next: records_to_items(input.clone(), records.next),
            suspended: records_to_items(input.clone(), records.suspended),
            backlog: records_to_items(input, records.backlog),
        }
    }
}

fn records_to_items(input: AgentLoopInput, records: Vec<InputRecord>) -> Vec<InteractiveInputItem> {
    records
        .into_iter()
        .map(|record| InteractiveInputItem::new(input.clone(), record.receipt, record.text))
        .collect()
}

#[derive(Clone)]
pub struct AgentLoopControl {
    run_id: InteractiveRunId,
    command_sender: mpsc::Sender<InteractiveCommand>,
}

impl AgentLoopControl {
    pub(super) fn new(
        run_id: InteractiveRunId,
        command_sender: mpsc::Sender<InteractiveCommand>,
    ) -> Self {
        Self {
            run_id,
            command_sender,
        }
    }

    #[must_use]
    pub fn run_id(&self) -> InteractiveRunId {
        self.run_id
    }

    pub async fn update_settings(
        &self,
        update: InteractiveSettingsUpdate,
    ) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::UpdateSettings {
                update: Box::new(update),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn resume_suspended(&self) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::ResumeSuspended { ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn discard_suspended(&self) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::DiscardSuspended { ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn interrupt(&self, reason: InterruptReason) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Interrupt { reason, ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    /// Saves the session only while the interactive producer is at an idle boundary.
    ///
    /// Returns [`InteractiveError::SessionSaveRequiresIdle`] immediately when a model, tool, or
    /// interrupt phase is active.
    pub async fn save_session_to(&self, store: FileSessionStore) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::SaveSession { store, ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn enter_plan_mode(&self, reason: &str) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::EnterPlanMode {
                reason: reason.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn approve_plan(&self, input: PlanApprovalInput) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::ApprovePlan { input, ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn revise_plan(&self, reason: &str) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::RevisePlan {
                reason: reason.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn pause_plan_scheduling(&self, reason: &str) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::PausePlanScheduling {
                reason: reason.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn resume_plan_scheduling(&self, reason: &str) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::ResumePlanScheduling {
                reason: reason.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn cancel_plan(&self, reason: &str) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::CancelPlan {
                reason: reason.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn retry_interrupted_plan_node(
        &self,
        node_id: PlanNodeId,
        reason: &str,
    ) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::RetryInterruptedPlanNode {
                node_id,
                reason: reason.to_owned(),
                ack_sender,
            })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }

    pub async fn close(&self) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Close { ack_sender })
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?;
        ack_receiver
            .await
            .map_err(|_| InteractiveError::CommandChannelClosed {
                run_id: self.run_id,
            })?
    }
}
