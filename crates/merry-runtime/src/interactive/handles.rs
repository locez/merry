use super::{
    InteractiveCommand, InteractiveError, InteractiveRunId, InteractiveSettingsUpdate,
    InterruptReason,
    types::{InputReceipt, InputRecord, InputRecords},
};
use crate::UserMessageInput;
use futures_core::Stream;
use merry_core::{QueuedInputLane, RuntimeEvent};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

pub struct InteractiveRunEventStream {
    inner: Option<ReceiverStream<RuntimeEvent>>,
    cancellation_token: CancellationToken,
    producer_handle: Option<JoinHandle<()>>,
}

impl InteractiveRunEventStream {
    pub(super) fn new(
        inner: ReceiverStream<RuntimeEvent>,
        cancellation_token: CancellationToken,
        producer_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            inner: Some(inner),
            cancellation_token,
            producer_handle: Some(producer_handle),
        }
    }

    pub async fn next_event(&mut self) -> Option<RuntimeEvent> {
        use futures_util::StreamExt;

        self.next().await
    }

    pub async fn wait_until_closed(&mut self) {
        use futures_util::StreamExt;

        if let Some(inner) = self.inner.as_mut() {
            while let Some(event) = inner.next().await {
                if matches!(event, RuntimeEvent::Closed) {
                    break;
                }
            }
        }
        self.inner.take();
        if let Some(handle) = self.producer_handle.take() {
            let _ = handle.await;
        }
    }
}

impl Stream for InteractiveRunEventStream {
    type Item = RuntimeEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(None);
        };

        match Pin::new(inner).poll_next(cx) {
            Poll::Ready(None) => {
                self.producer_handle.take();
                Poll::Ready(None)
            }
            poll => poll,
        }
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
