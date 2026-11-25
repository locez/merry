use super::commands::InteractiveCommand;
use super::{
    InteractiveError, InteractiveRunId, InteractiveSettingsUpdate, InterruptReason, QueuedInputId,
    types::{InputRecord, InputRecords},
};
use crate::bridge::BridgeToolResultCommand;
use crate::hooks::bridge::HookDecisionCommand;
use crate::{
    CompactionOutcome, FileSessionStore, HookDecision, HookInvocation, HookInvocationId,
    UserMessageInput,
};
use merry_core::{PendingToolCallBatch, PlanId, RuntimeEvent, ToolCallBatchId, ToolCallId};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::{
    sync::{mpsc, oneshot, watch},
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
    inner_closed: bool,
    rate_updates: watch::Receiver<Option<RuntimeEvent>>,
    rate_updates_closed: bool,
    cancellation_token: CancellationToken,
    producer_handle: Option<JoinHandle<Result<(), InteractiveError>>>,
    bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
    run_id: InteractiveRunId,
    pending_tool_invocations: Option<PendingToolCallBatch>,
    unread_message: Option<InteractiveRunMessage>,
    bridge_resolution_epoch: Arc<AtomicU64>,
    observed_bridge_resolution_epoch: u64,
    hook_sender: mpsc::Sender<HookDecisionCommand>,
    pending_hook_invocation: Option<HookInvocation>,
    hook_resolution_epoch: Arc<AtomicU64>,
    observed_hook_resolution_epoch: u64,
    closed_event_observed: bool,
}

impl InteractiveRunEventStream {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        run_id: InteractiveRunId,
        inner: ReceiverStream<InteractiveRunMessage>,
        rate_updates: watch::Receiver<Option<RuntimeEvent>>,
        cancellation_token: CancellationToken,
        producer_handle: JoinHandle<Result<(), InteractiveError>>,
        bridge_sender: mpsc::Sender<BridgeToolResultCommand>,
        bridge_resolution_epoch: Arc<AtomicU64>,
        hook_sender: mpsc::Sender<HookDecisionCommand>,
        hook_resolution_epoch: Arc<AtomicU64>,
    ) -> Self {
        let observed_bridge_resolution_epoch = bridge_resolution_epoch.load(Ordering::Acquire);
        let observed_hook_resolution_epoch = hook_resolution_epoch.load(Ordering::Acquire);
        Self {
            inner: Some(inner),
            inner_closed: false,
            rate_updates,
            rate_updates_closed: false,
            cancellation_token,
            producer_handle: Some(producer_handle),
            bridge_sender,
            run_id,
            pending_tool_invocations: None,
            unread_message: None,
            bridge_resolution_epoch,
            observed_bridge_resolution_epoch,
            hook_sender,
            pending_hook_invocation: None,
            hook_resolution_epoch,
            observed_hook_resolution_epoch,
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
        let hook_epoch = self.hook_resolution_epoch.load(Ordering::Acquire);
        if hook_epoch != self.observed_hook_resolution_epoch {
            self.pending_hook_invocation = None;
            self.unread_message = None;
            self.observed_hook_resolution_epoch = hook_epoch;
        }
    }

    /// Returns the next ordered event, host tool handoff, or subscribed hook invocation.
    pub async fn next_message(
        &mut self,
    ) -> Result<Option<InteractiveRunMessage>, InteractiveError> {
        self.synchronize_bridge_resolution();
        if self.pending_tool_invocations.is_some() {
            return Err(InteractiveError::ToolInvocationsPending {
                run_id: self.run_id,
            });
        }
        if self.pending_hook_invocation.is_some() {
            return Err(InteractiveError::HookInvocationPending {
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
        loop {
            if self.inner_closed && self.rate_updates_closed {
                return None;
            }
            if self.inner_closed {
                match self.rate_updates.changed().await {
                    Ok(()) => {
                        if let Some(event) = self.rate_updates.borrow_and_update().clone() {
                            return Some(InteractiveRunMessage::Event(event));
                        }
                    }
                    Err(_) => self.rate_updates_closed = true,
                }
                continue;
            }
            let Some(inner) = self.inner.as_mut() else {
                self.inner_closed = true;
                continue;
            };
            if self.rate_updates_closed {
                let message = inner.next().await;
                if message.is_none() {
                    self.inner_closed = true;
                }
                return message;
            }
            tokio::select! {
                biased;
                message = inner.next() => {
                    if message.is_none() {
                        self.inner_closed = true;
                    }
                    return message;
                }
                changed = self.rate_updates.changed() => {
                    match changed {
                        Ok(()) => {
                            if let Some(event) = self.rate_updates.borrow_and_update().clone() {
                                return Some(InteractiveRunMessage::Event(event));
                            }
                        }
                        Err(_) => self.rate_updates_closed = true,
                    }
                }
            }
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
            Err(error) => {
                let reason = error.to_string();
                tracing::error!(
                    run_id = self.run_id.as_u64(),
                    reason = %reason,
                    "interactive run producer task failed"
                );
                Err(InteractiveError::ProducerTaskFailed {
                    run_id: self.run_id,
                    reason,
                })
            }
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
        match message {
            InteractiveRunMessage::ToolInvocations { batch } => {
                if batch.calls().is_empty() {
                    return Err(InteractiveError::InvalidToolInvocationBatch {
                        run_id: self.run_id,
                    });
                }
                self.pending_tool_invocations = Some(batch.clone());
            }
            InteractiveRunMessage::HookInvocation { invocation } => {
                self.pending_hook_invocation = Some(invocation.clone());
            }
            InteractiveRunMessage::Event(_) => {}
        }
        Ok(())
    }

    /// Returns the next runtime event when no host handoff is encountered.
    ///
    /// Hosts that register bridge tools or hooks must use [`Self::next_message`].
    pub async fn next_event(&mut self) -> Result<Option<RuntimeEvent>, InteractiveError> {
        self.synchronize_bridge_resolution();
        if self.pending_tool_invocations.is_some() {
            return Err(InteractiveError::ToolInvocationsPending {
                run_id: self.run_id,
            });
        }
        if self.pending_hook_invocation.is_some() {
            return Err(InteractiveError::HookInvocationPending {
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
            InteractiveRunMessage::HookInvocation { invocation } => {
                self.unread_message = Some(InteractiveRunMessage::HookInvocation { invocation });
                Err(InteractiveError::HookInvocationRequiresMessageProtocol {
                    run_id: self.run_id,
                })
            }
        }
    }

    /// Submits a typed decision for the active host-bridged hook invocation.
    pub async fn submit_hook_decision(
        &mut self,
        invocation_id: &HookInvocationId,
        decision: HookDecision,
    ) -> Result<(), InteractiveError> {
        self.submit_hook_outcome(invocation_id, Ok(decision)).await
    }

    /// Reports a fail-closed host hook error for the active hook invocation.
    pub async fn fail_hook_invocation(
        &mut self,
        invocation_id: &HookInvocationId,
        message: String,
    ) -> Result<(), InteractiveError> {
        self.submit_hook_outcome(invocation_id, Err(message)).await
    }

    async fn submit_hook_outcome(
        &mut self,
        invocation_id: &HookInvocationId,
        outcome: Result<HookDecision, String>,
    ) -> Result<(), InteractiveError> {
        self.synchronize_bridge_resolution();
        if self.pending_hook_invocation.is_none() {
            return Err(InteractiveError::NoPendingHookInvocation {
                run_id: self.run_id,
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
            .map_err(|_| InteractiveError::RunClosed {
                run_id: self.run_id,
            })?;
        let result = ack_receiver
            .await
            .map_err(|_| InteractiveError::RunClosed {
                run_id: self.run_id,
            })?;
        match result {
            Ok(()) => {
                self.pending_hook_invocation = None;
                Ok(())
            }
            Err(source) => Err(InteractiveError::Runtime { source }),
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

    /// Cancels the producer, drains all buffered events and handoffs, and waits
    /// for durable shutdown (including settling any pending bridge tool calls).
    pub async fn cancel_and_wait_until_closed(&mut self) -> Result<(), InteractiveError> {
        self.cancellation_token.cancel();
        self.pending_tool_invocations = None;
        self.pending_hook_invocation = None;
        self.unread_message = None;
        while let Some(message) = self.receive_message().await {
            self.observe_terminal_message(&message);
            if self.closed_event_observed {
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

    /// Appends text to the pending queue (runs immediately when idle and not paused).
    pub async fn submit(&self, text: &str) -> Result<InteractiveInputItem, InteractiveError> {
        let message = UserMessageInput::text_only(text)?;
        self.submit_message(message).await
    }

    /// Appends a user message to the pending queue (runs immediately when idle and not paused).
    pub async fn submit_message(
        &self,
        message: UserMessageInput,
    ) -> Result<InteractiveInputItem, InteractiveError> {
        let text = message.text().to_owned();
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Submit {
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
            .map(|id| InteractiveInputItem::new(self.clone(), id, text))
    }

    /// Updates a pending item's text by its stable [`QueuedInputId`].
    pub async fn update_by_id(
        &self,
        id: QueuedInputId,
        text: &str,
    ) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Update {
                id,
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

    /// Removes a pending item by its stable [`QueuedInputId`].
    pub async fn remove_by_id(&self, id: QueuedInputId) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::Remove { id, ack_sender })
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

    /// Pauses or resumes automatic dispatch of pending items.
    ///
    /// While paused (for example, when any pending item is being edited in a UI),
    /// pending items will not auto-send at turn boundaries. Passing `false` resumes
    /// auto-dispatch and immediately sends all pending items together if the loop is idle.
    pub async fn set_paused(&self, paused: bool) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::SetPendingPaused { paused, ack_sender })
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

    /// Resumes and immediately dispatches all pending items together if the loop is idle.
    pub async fn flush(&self) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::FlushPending { ack_sender })
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
        items: &[InteractiveInputItem],
    ) -> Result<(), InteractiveError> {
        if items.iter().any(|item| item.input.run_id != self.run_id) {
            return Err(InteractiveError::InvalidPendingOrder {
                reason: "input item belongs to another interactive run",
            });
        }
        let ids = items.iter().map(|item| item.id).collect::<Vec<_>>();
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::ReplacePendingOrder { ids, ack_sender })
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
    id: QueuedInputId,
    text: String,
}

impl InteractiveInputItem {
    fn new(input: AgentLoopInput, id: QueuedInputId, text: String) -> Self {
        Self { input, id, text }
    }

    #[must_use]
    pub fn id(&self) -> QueuedInputId {
        self.id
    }

    #[must_use]
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    pub async fn update(&mut self, text: impl Into<String>) -> Result<(), InteractiveError> {
        let text = text.into();
        self.input.update_by_id(self.id, &text).await?;
        self.text = text;
        Ok(())
    }

    pub async fn remove(self) -> Result<(), InteractiveError> {
        self.input.remove_by_id(self.id).await
    }
}

#[derive(Clone)]
pub struct InteractiveInputSnapshot {
    pub items: Vec<InteractiveInputItem>,
    pub paused: bool,
}

impl InteractiveInputSnapshot {
    fn from_records(input: AgentLoopInput, records: InputRecords) -> Self {
        Self {
            items: records_to_items(input, records.items),
            paused: records.paused,
        }
    }
}

fn records_to_items(input: AgentLoopInput, records: Vec<InputRecord>) -> Vec<InteractiveInputItem> {
    records
        .into_iter()
        .map(|record| InteractiveInputItem::new(input.clone(), record.id, record.text))
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

    /// Manually compacts session history at an idle boundary, optionally
    /// emphasizing `focus` instructions in the generated summary.
    ///
    /// Returns `Ok(None)` when no compressible history exists before the
    /// retained tail, and [`InteractiveError::CompactRequiresIdle`] immediately
    /// when a model, tool, compaction, or interrupt phase is already active.
    pub async fn compact_context(
        &self,
        focus: Option<String>,
    ) -> Result<Option<CompactionOutcome>, InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::CompactContext { focus, ack_sender })
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

    /// Toggles Plan Mode on or off (works both while idle and mid-turn).
    pub async fn set_plan_mode(&self, enabled: bool) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::SetPlanMode {
                enabled,
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

    /// Approves a submitted plan, exits Plan Mode, and continues execution.
    pub async fn approve_plan(
        &self,
        plan_id: PlanId,
        note: Option<String>,
    ) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::ApprovePlan {
                plan_id,
                note,
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

    /// Keeps Plan Mode active and delivers revision feedback to the agent.
    pub async fn request_plan_changes(
        &self,
        plan_id: PlanId,
        feedback: String,
    ) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::RequestPlanChanges {
                plan_id,
                feedback,
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

    /// Abandons a submitted plan and exits Plan Mode without implementing.
    pub async fn abandon_plan(&self, plan_id: PlanId) -> Result<(), InteractiveError> {
        let (ack_sender, ack_receiver) = oneshot::channel();
        self.command_sender
            .send(InteractiveCommand::AbandonPlan {
                plan_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn producer_panic_preserves_join_error_reason() {
        let (message_tx, message_rx) = mpsc::channel(1);
        let (rate_tx, rate_rx) = watch::channel(None);
        let (bridge_tx, _bridge_rx) = mpsc::channel(1);
        let (hook_tx, _hook_rx) = mpsc::channel(1);
        let producer = tokio::spawn(async move {
            let _tx = message_tx;
            let _rate = rate_tx;
            panic!("injected thread spawn failure: os error 11");
        });

        let mut stream = InteractiveRunEventStream::new(
            InteractiveRunId(42),
            ReceiverStream::new(message_rx),
            rate_rx,
            CancellationToken::new(),
            producer,
            bridge_tx,
            Arc::new(AtomicU64::new(0)),
            hook_tx,
            Arc::new(AtomicU64::new(0)),
        );

        let err = stream
            .next_message()
            .await
            .expect_err("panicked producer should surface ProducerTaskFailed");
        match &err {
            InteractiveError::ProducerTaskFailed { run_id, reason } => {
                assert_eq!(*run_id, InteractiveRunId(42));
                assert!(
                    reason.contains("injected thread spawn failure: os error 11"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("expected ProducerTaskFailed, got {other:?}"),
        }
        assert!(
            err.to_string()
                .contains("injected thread spawn failure: os error 11")
        );
    }
}
