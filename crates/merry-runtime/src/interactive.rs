#![allow(dead_code)]

use crate::RuntimeError;
use futures_core::Stream;
use merry_core::RuntimeEvent;
use std::{
    collections::{HashMap, VecDeque},
    pin::Pin,
    task::{Context, Poll},
};
use thiserror::Error;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InteractiveRunId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InteractiveInputId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    Next,
    Suspended,
    Backlog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveRunState {
    WaitingForInput,
    RunningModel,
    RunningTool,
    Interrupting,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputReceipt {
    pub id: InteractiveInputId,
    pub queue: QueueKind,
    pub position: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedInputSnapshot {
    pub id: InteractiveInputId,
    pub text: String,
    pub queue: QueueKind,
    pub position: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub next: Vec<QueuedInputSnapshot>,
    pub suspended: Vec<QueuedInputSnapshot>,
    pub backlog: Vec<QueuedInputSnapshot>,
}

#[derive(Debug, Error)]
pub enum InteractiveError {
    #[error("interactive run {run_id:?} is closed")]
    RunClosed { run_id: InteractiveRunId },
    #[error("interactive run {run_id:?} command channel is closed")]
    CommandChannelClosed { run_id: InteractiveRunId },
    #[error("invalid interactive input: {reason}")]
    InvalidInput { reason: &'static str },
    #[error("interactive input {id:?} is unknown")]
    UnknownInput { id: InteractiveInputId },
    #[error("interactive input {id:?} is already accepted")]
    AlreadyAccepted { id: InteractiveInputId },
    #[error("interactive input {id:?} is already removed")]
    AlreadyRemoved { id: InteractiveInputId },
    #[error("interactive input {id:?} is in {actual:?}, expected {expected:?}")]
    WrongQueue {
        id: InteractiveInputId,
        expected: QueueKind,
        actual: QueueKind,
    },
    #[error("interactive queue {queue:?} is full")]
    QueueFull { queue: QueueKind },
    #[error("runtime error while running interactive loop: {source}")]
    Runtime {
        #[from]
        source: RuntimeError,
    },
}

#[derive(Debug)]
pub enum InteractiveRunEvent {
    StateChanged {
        state: InteractiveRunState,
    },
    InputAccepted {
        ids: Vec<InteractiveInputId>,
        queue: QueueKind,
    },
    QueueChanged {
        snapshot: QueueSnapshot,
    },
    Runtime(RuntimeEvent),
    Closed,
}

pub struct InteractiveRunEventStream {
    inner: ReceiverStream<InteractiveRunEvent>,
}

impl InteractiveRunEventStream {
    pub(crate) fn new(inner: ReceiverStream<InteractiveRunEvent>) -> Self {
        Self { inner }
    }
}

impl Stream for InteractiveRunEventStream {
    type Item = InteractiveRunEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

pub struct InteractiveAgentRun {
    stream: InteractiveRunEventStream,
    input: AgentLoopInput,
    control: AgentLoopControl,
}

impl InteractiveAgentRun {
    pub(crate) fn new(
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
}

impl AgentLoopInput {
    pub(crate) fn new(run_id: InteractiveRunId) -> Self {
        Self { run_id }
    }

    #[must_use]
    pub fn run_id(&self) -> InteractiveRunId {
        self.run_id
    }
}

#[derive(Clone)]
pub struct AgentLoopControl {
    run_id: InteractiveRunId,
}

impl AgentLoopControl {
    pub(crate) fn new(run_id: InteractiveRunId) -> Self {
        Self { run_id }
    }

    #[must_use]
    pub fn run_id(&self) -> InteractiveRunId {
        self.run_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuedInputState {
    Pending,
    Accepted,
    Removed,
}

#[derive(Debug, Clone)]
struct QueuedInput {
    text: String,
    queue: QueueKind,
    state: QueuedInputState,
}

#[derive(Debug, Default)]
pub(crate) struct InteractiveInputQueue {
    next_id: u64,
    next: VecDeque<InteractiveInputId>,
    suspended: VecDeque<InteractiveInputId>,
    backlog: VecDeque<InteractiveInputId>,
    items: HashMap<InteractiveInputId, QueuedInput>,
}

impl InteractiveInputQueue {
    pub(crate) fn submit_next(&mut self, text: &str) -> Result<InputReceipt, InteractiveError> {
        self.push(text, QueueKind::Next)
    }

    pub(crate) fn enqueue(&mut self, text: &str) -> Result<InputReceipt, InteractiveError> {
        self.push(text, QueueKind::Backlog)
    }

    pub(crate) fn update(
        &mut self,
        id: InteractiveInputId,
        text: &str,
    ) -> Result<(), InteractiveError> {
        validate_interactive_text(text)?;
        let item = self.pending_item_mut(id)?;
        item.text = text.to_owned();
        Ok(())
    }

    pub(crate) fn remove(&mut self, id: InteractiveInputId) -> Result<(), InteractiveError> {
        let queue = self.pending_item(id)?.queue;
        self.remove_from_queue(id, queue)?;
        self.pending_item_mut(id)?.state = QueuedInputState::Removed;
        Ok(())
    }

    pub(crate) fn move_before(
        &mut self,
        id: InteractiveInputId,
        anchor: InteractiveInputId,
    ) -> Result<(), InteractiveError> {
        let queue = self.same_queue(id, anchor)?;
        let ids = self.queue_mut(queue);
        remove_id(ids, id);
        let index = ids
            .iter()
            .position(|candidate| *candidate == anchor)
            .ok_or(InteractiveError::UnknownInput { id: anchor })?;
        ids.insert(index, id);
        Ok(())
    }

    pub(crate) fn move_after(
        &mut self,
        id: InteractiveInputId,
        anchor: InteractiveInputId,
    ) -> Result<(), InteractiveError> {
        let queue = self.same_queue(id, anchor)?;
        let ids = self.queue_mut(queue);
        remove_id(ids, id);
        let index = ids
            .iter()
            .position(|candidate| *candidate == anchor)
            .ok_or(InteractiveError::UnknownInput { id: anchor })?;
        ids.insert(index + 1, id);
        Ok(())
    }

    pub(crate) fn suspend_next(&mut self) {
        while let Some(id) = self.next.pop_front() {
            if let Some(item) = self.items.get_mut(&id) {
                item.queue = QueueKind::Suspended;
            }
            self.suspended.push_back(id);
        }
    }

    pub(crate) fn discard_suspended(&mut self) -> Vec<InteractiveInputId> {
        self.suspended
            .drain(..)
            .inspect(|id| {
                if let Some(item) = self.items.get_mut(id) {
                    item.state = QueuedInputState::Removed;
                }
            })
            .collect()
    }

    pub(crate) fn accept_next_burst(&mut self) -> Vec<QueuedInputSnapshot> {
        self.accept_queue_burst(QueueKind::Next)
    }

    pub(crate) fn accept_suspended_burst(&mut self) -> Vec<QueuedInputSnapshot> {
        self.accept_queue_burst(QueueKind::Suspended)
    }

    pub(crate) fn accept_one_backlog(&mut self) -> Vec<QueuedInputSnapshot> {
        let Some(id) = self.backlog.pop_front() else {
            return Vec::new();
        };
        self.mark_accepted(id, QueueKind::Backlog, 0)
            .into_iter()
            .collect()
    }

    pub(crate) fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            next: self.snapshots_for(QueueKind::Next),
            suspended: self.snapshots_for(QueueKind::Suspended),
            backlog: self.snapshots_for(QueueKind::Backlog),
        }
    }

    fn push(&mut self, text: &str, queue: QueueKind) -> Result<InputReceipt, InteractiveError> {
        validate_interactive_text(text)?;
        let id = InteractiveInputId(self.next_id);
        self.next_id += 1;
        let ids = self.queue_mut(queue);
        let position = ids.len();
        ids.push_back(id);
        self.items.insert(
            id,
            QueuedInput {
                text: text.to_owned(),
                queue,
                state: QueuedInputState::Pending,
            },
        );
        Ok(InputReceipt {
            id,
            queue,
            position,
        })
    }

    fn pending_item(&self, id: InteractiveInputId) -> Result<&QueuedInput, InteractiveError> {
        let item = self
            .items
            .get(&id)
            .ok_or(InteractiveError::UnknownInput { id })?;

        match item.state {
            QueuedInputState::Pending => Ok(item),
            QueuedInputState::Accepted => Err(InteractiveError::AlreadyAccepted { id }),
            QueuedInputState::Removed => Err(InteractiveError::AlreadyRemoved { id }),
        }
    }

    fn pending_item_mut(
        &mut self,
        id: InteractiveInputId,
    ) -> Result<&mut QueuedInput, InteractiveError> {
        let item = self
            .items
            .get_mut(&id)
            .ok_or(InteractiveError::UnknownInput { id })?;

        match item.state {
            QueuedInputState::Pending => Ok(item),
            QueuedInputState::Accepted => Err(InteractiveError::AlreadyAccepted { id }),
            QueuedInputState::Removed => Err(InteractiveError::AlreadyRemoved { id }),
        }
    }

    fn queue_mut(&mut self, queue: QueueKind) -> &mut VecDeque<InteractiveInputId> {
        match queue {
            QueueKind::Next => &mut self.next,
            QueueKind::Suspended => &mut self.suspended,
            QueueKind::Backlog => &mut self.backlog,
        }
    }

    fn queue(&self, queue: QueueKind) -> &VecDeque<InteractiveInputId> {
        match queue {
            QueueKind::Next => &self.next,
            QueueKind::Suspended => &self.suspended,
            QueueKind::Backlog => &self.backlog,
        }
    }

    fn same_queue(
        &self,
        id: InteractiveInputId,
        anchor: InteractiveInputId,
    ) -> Result<QueueKind, InteractiveError> {
        let queue = self.pending_item(id)?.queue;
        let anchor_queue = self.pending_item(anchor)?.queue;
        if queue != anchor_queue {
            return Err(InteractiveError::WrongQueue {
                id,
                expected: anchor_queue,
                actual: queue,
            });
        }
        Ok(queue)
    }

    fn accept_queue_burst(&mut self, queue: QueueKind) -> Vec<QueuedInputSnapshot> {
        let ids = std::mem::take(self.queue_mut(queue));
        ids.into_iter()
            .enumerate()
            .filter_map(|(position, id)| self.mark_accepted(id, queue, position))
            .collect()
    }

    fn mark_accepted(
        &mut self,
        id: InteractiveInputId,
        queue: QueueKind,
        position: usize,
    ) -> Option<QueuedInputSnapshot> {
        let item = self.items.get_mut(&id)?;
        if item.state != QueuedInputState::Pending {
            return None;
        }
        item.state = QueuedInputState::Accepted;
        item.queue = queue;
        Some(QueuedInputSnapshot {
            id,
            text: item.text.clone(),
            queue,
            position,
        })
    }

    fn snapshots_for(&self, queue: QueueKind) -> Vec<QueuedInputSnapshot> {
        self.queue(queue)
            .iter()
            .enumerate()
            .filter_map(|(position, id)| {
                let item = self.items.get(id)?;
                (item.state == QueuedInputState::Pending).then(|| QueuedInputSnapshot {
                    id: *id,
                    text: item.text.clone(),
                    queue,
                    position,
                })
            })
            .collect()
    }

    fn remove_from_queue(
        &mut self,
        id: InteractiveInputId,
        queue: QueueKind,
    ) -> Result<(), InteractiveError> {
        if remove_id(self.queue_mut(queue), id) {
            Ok(())
        } else {
            Err(InteractiveError::UnknownInput { id })
        }
    }
}

fn remove_id(ids: &mut VecDeque<InteractiveInputId>, id: InteractiveInputId) -> bool {
    let Some(index) = ids.iter().position(|candidate| *candidate == id) else {
        return false;
    };
    ids.remove(index);
    true
}

fn validate_interactive_text(text: &str) -> Result<(), InteractiveError> {
    if text.trim().is_empty() {
        return Err(InteractiveError::InvalidInput {
            reason: "input text must not be blank",
        });
    }

    if text
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(InteractiveError::InvalidInput {
            reason: "input text must not contain control characters other than newline or tab",
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_submit_next_preempts_backlog_without_reordering_backlog() {
        let mut queue = InteractiveInputQueue::default();
        let backlog = queue.enqueue("backlog").expect("valid backlog");
        let next = queue.submit_next("next").expect("valid next");

        assert_eq!(queue.snapshot().next[0].id, next.id);
        assert_eq!(queue.snapshot().backlog[0].id, backlog.id);
    }

    #[test]
    fn queue_update_remove_and_reorder_pending_items() {
        let mut queue = InteractiveInputQueue::default();
        let first = queue.enqueue("first").expect("valid first").id;
        let second = queue.enqueue("second").expect("valid second").id;

        queue
            .update(first, "updated")
            .expect("pending item updates");
        queue
            .move_before(second, first)
            .expect("pending item reorders");
        queue.remove(first).expect("pending item removes");

        let snapshot = queue.snapshot();
        assert_eq!(snapshot.backlog[0].id, second);
        assert_eq!(snapshot.backlog[0].text, "second");
        assert_eq!(snapshot.backlog.len(), 1);
    }

    #[test]
    fn queue_interrupt_moves_next_to_suspended_and_leaves_backlog() {
        let mut queue = InteractiveInputQueue::default();
        let first = queue.submit_next("x").expect("valid next").id;
        let second = queue.submit_next("y").expect("valid next").id;
        let backlog = queue.enqueue("later").expect("valid backlog").id;

        queue.suspend_next();

        let snapshot = queue.snapshot();
        assert!(snapshot.next.is_empty());
        assert_eq!(
            snapshot
                .suspended
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert_eq!(snapshot.backlog[0].id, backlog);
    }

    #[test]
    fn queue_rejects_edit_after_acceptance() {
        let mut queue = InteractiveInputQueue::default();
        let id = queue.submit_next("x").expect("valid next").id;
        let accepted = queue.accept_next_burst();
        assert_eq!(
            accepted.iter().map(|item| item.id).collect::<Vec<_>>(),
            vec![id]
        );

        let err = queue
            .update(id, "changed")
            .expect_err("accepted item should not update");
        assert!(matches!(err, InteractiveError::AlreadyAccepted { .. }));
    }
}
