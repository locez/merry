use super::{
    InteractiveError,
    types::{InputReceipt, InputRecord, InputRecords, QueuedInputId},
};
use crate::StepInput;
use merry_core::{QueuedInputLane, QueuedInputView, QueuedInputsView};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuedInputState {
    Pending,
    Accepted,
    Removed,
}

#[derive(Debug, Clone)]
struct QueuedInput {
    text: String,
    lane: QueuedInputLane,
    state: QueuedInputState,
}

#[derive(Debug, Default)]
pub(super) struct InteractiveInputQueue {
    next_id: u64,
    next: VecDeque<QueuedInputId>,
    suspended: VecDeque<QueuedInputId>,
    backlog: VecDeque<QueuedInputId>,
    items: HashMap<QueuedInputId, QueuedInput>,
}

impl InteractiveInputQueue {
    pub(super) fn has_next(&self) -> bool {
        !self.next.is_empty()
    }

    pub(super) fn submit_next(&mut self, text: &str) -> Result<InputReceipt, InteractiveError> {
        self.push(text, QueuedInputLane::Next)
    }

    pub(super) fn enqueue(&mut self, text: &str) -> Result<InputReceipt, InteractiveError> {
        self.push(text, QueuedInputLane::Backlog)
    }

    pub(super) fn update(&mut self, id: QueuedInputId, text: &str) -> Result<(), InteractiveError> {
        validate_interactive_text(text)?;
        let item = self.pending_item_mut(id)?;
        item.text = text.to_owned();
        Ok(())
    }

    pub(super) fn remove(&mut self, id: QueuedInputId) -> Result<(), InteractiveError> {
        let lane = self.pending_item(id)?.lane;
        self.remove_from_lane(id, lane)?;
        self.pending_item_mut(id)?.state = QueuedInputState::Removed;
        Ok(())
    }

    pub(super) fn replace_pending_order(
        &mut self,
        lane: QueuedInputLane,
        ordered: Vec<QueuedInputId>,
    ) -> Result<(), InteractiveError> {
        let current = self.lane(lane).iter().copied().collect::<Vec<_>>();
        if ordered.len() != current.len() {
            return Err(InteractiveError::StalePendingOrder {
                lane,
                reason: "input count changed",
            });
        }

        let mut seen = HashSet::with_capacity(ordered.len());
        for id in &ordered {
            if !seen.insert(*id) {
                return Err(InteractiveError::InvalidPendingOrder {
                    lane,
                    reason: "input order contains a duplicate item",
                });
            }
            let item = self.pending_item(*id)?;
            if item.lane != lane {
                return Err(InteractiveError::WrongQueue {
                    expected: lane,
                    actual: item.lane,
                });
            }
        }

        let current_set = current.into_iter().collect::<HashSet<_>>();
        if seen != current_set {
            return Err(InteractiveError::StalePendingOrder {
                lane,
                reason: "input set changed",
            });
        }

        *self.lane_mut(lane) = VecDeque::from(ordered);
        Ok(())
    }

    pub(super) fn suspend_next(&mut self) {
        while let Some(id) = self.next.pop_front() {
            if let Some(item) = self.items.get_mut(&id) {
                item.lane = QueuedInputLane::Suspended;
            }
            self.suspended.push_back(id);
        }
    }

    pub(super) fn discard_suspended(&mut self) -> Vec<QueuedInputId> {
        self.suspended
            .drain(..)
            .inspect(|id| {
                if let Some(item) = self.items.get_mut(id) {
                    item.state = QueuedInputState::Removed;
                }
            })
            .collect()
    }

    pub(super) fn accept_next_burst(&mut self) -> Vec<QueuedInputView> {
        self.accept_queue_burst(QueuedInputLane::Next)
    }

    pub(super) fn accept_suspended_burst(&mut self) -> Vec<QueuedInputView> {
        self.accept_queue_burst(QueuedInputLane::Suspended)
    }

    pub(super) fn accept_one_backlog(&mut self) -> Vec<QueuedInputView> {
        let Some(id) = self.backlog.pop_front() else {
            return Vec::new();
        };
        self.mark_accepted(id, QueuedInputLane::Backlog, 0)
            .into_iter()
            .collect()
    }

    pub(super) fn snapshot(&self) -> QueuedInputsView {
        QueuedInputsView {
            next: self.snapshots_for(QueuedInputLane::Next),
            suspended: self.snapshots_for(QueuedInputLane::Suspended),
            backlog: self.snapshots_for(QueuedInputLane::Backlog),
        }
    }

    pub(super) fn input_records(&self) -> InputRecords {
        InputRecords {
            next: self.records_for(QueuedInputLane::Next),
            suspended: self.records_for(QueuedInputLane::Suspended),
            backlog: self.records_for(QueuedInputLane::Backlog),
        }
    }

    fn push(
        &mut self,
        text: &str,
        lane: QueuedInputLane,
    ) -> Result<InputReceipt, InteractiveError> {
        validate_interactive_text(text)?;
        let id = QueuedInputId::from_u64(self.next_id);
        self.next_id += 1;
        let ids = self.lane_mut(lane);
        let position = ids.len();
        ids.push_back(id);
        self.items.insert(
            id,
            QueuedInput {
                text: text.to_owned(),
                lane,
                state: QueuedInputState::Pending,
            },
        );
        Ok(InputReceipt { id, lane, position })
    }

    fn pending_item(&self, id: QueuedInputId) -> Result<&QueuedInput, InteractiveError> {
        let item = self.items.get(&id).ok_or(InteractiveError::UnknownInput)?;

        match item.state {
            QueuedInputState::Pending => Ok(item),
            QueuedInputState::Accepted => Err(InteractiveError::AlreadyAccepted),
            QueuedInputState::Removed => Err(InteractiveError::AlreadyRemoved),
        }
    }

    fn pending_item_mut(
        &mut self,
        id: QueuedInputId,
    ) -> Result<&mut QueuedInput, InteractiveError> {
        let item = self
            .items
            .get_mut(&id)
            .ok_or(InteractiveError::UnknownInput)?;

        match item.state {
            QueuedInputState::Pending => Ok(item),
            QueuedInputState::Accepted => Err(InteractiveError::AlreadyAccepted),
            QueuedInputState::Removed => Err(InteractiveError::AlreadyRemoved),
        }
    }

    fn lane_mut(&mut self, lane: QueuedInputLane) -> &mut VecDeque<QueuedInputId> {
        match lane {
            QueuedInputLane::Next => &mut self.next,
            QueuedInputLane::Suspended => &mut self.suspended,
            QueuedInputLane::Backlog => &mut self.backlog,
        }
    }

    fn lane(&self, lane: QueuedInputLane) -> &VecDeque<QueuedInputId> {
        match lane {
            QueuedInputLane::Next => &self.next,
            QueuedInputLane::Suspended => &self.suspended,
            QueuedInputLane::Backlog => &self.backlog,
        }
    }

    fn accept_queue_burst(&mut self, lane: QueuedInputLane) -> Vec<QueuedInputView> {
        let ids = std::mem::take(self.lane_mut(lane));
        ids.into_iter()
            .enumerate()
            .filter_map(|(position, id)| self.mark_accepted(id, lane, position))
            .collect()
    }

    fn mark_accepted(
        &mut self,
        id: QueuedInputId,
        lane: QueuedInputLane,
        position: usize,
    ) -> Option<QueuedInputView> {
        let item = self.items.get_mut(&id)?;
        if item.state != QueuedInputState::Pending {
            return None;
        }
        item.state = QueuedInputState::Accepted;
        item.lane = lane;
        Some(QueuedInputView {
            text: item.text.clone(),
            lane,
            position,
        })
    }

    fn snapshots_for(&self, lane: QueuedInputLane) -> Vec<QueuedInputView> {
        self.lane(lane)
            .iter()
            .enumerate()
            .filter_map(|(position, id)| {
                let item = self.items.get(id)?;
                (item.state == QueuedInputState::Pending).then(|| QueuedInputView {
                    text: item.text.clone(),
                    lane,
                    position,
                })
            })
            .collect()
    }

    fn records_for(&self, lane: QueuedInputLane) -> Vec<InputRecord> {
        self.lane(lane)
            .iter()
            .enumerate()
            .filter_map(|(position, id)| {
                let item = self.items.get(id)?;
                (item.state == QueuedInputState::Pending).then(|| InputRecord {
                    receipt: InputReceipt {
                        id: *id,
                        lane,
                        position,
                    },
                    text: item.text.clone(),
                })
            })
            .collect()
    }

    fn remove_from_lane(
        &mut self,
        id: QueuedInputId,
        lane: QueuedInputLane,
    ) -> Result<(), InteractiveError> {
        if remove_id(self.lane_mut(lane), id) {
            Ok(())
        } else {
            Err(InteractiveError::UnknownInput)
        }
    }
}

pub(super) fn step_input_from_accepted(accepted: &[QueuedInputView]) -> Option<StepInput> {
    StepInput::user_texts(accepted.iter().map(|item| item.text.as_str())).ok()
}

fn remove_id(ids: &mut VecDeque<QueuedInputId>, id: QueuedInputId) -> bool {
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

        let snapshot = queue.snapshot();
        assert_eq!(snapshot.next[0].text, "next");
        assert_eq!(snapshot.next[0].position, next.position);
        assert_eq!(snapshot.backlog[0].text, "backlog");
        assert_eq!(snapshot.backlog[0].position, backlog.position);
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
            .replace_pending_order(QueuedInputLane::Backlog, vec![second, first])
            .expect("pending item reorders");
        queue.remove(first).expect("pending item removes");

        let snapshot = queue.snapshot();
        assert_eq!(snapshot.backlog[0].text, "second");
        assert_eq!(snapshot.backlog[0].position, 0);
        assert_eq!(snapshot.backlog.len(), 1);
    }

    #[test]
    fn queue_interrupt_moves_next_to_suspended_and_leaves_backlog() {
        let mut queue = InteractiveInputQueue::default();
        queue.submit_next("x").expect("valid next");
        queue.submit_next("y").expect("valid next");
        queue.enqueue("later").expect("valid backlog");

        queue.suspend_next();

        let snapshot = queue.snapshot();
        assert!(snapshot.next.is_empty());
        assert_eq!(
            snapshot
                .suspended
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            vec!["x", "y"]
        );
        assert_eq!(snapshot.backlog[0].text, "later");
    }

    #[test]
    fn queue_rejects_edit_after_acceptance() {
        let mut queue = InteractiveInputQueue::default();
        let id = queue.submit_next("x").expect("valid next").id;
        let accepted = queue.accept_next_burst();
        assert_eq!(accepted[0].text, "x");

        let err = queue
            .update(id, "changed")
            .expect_err("accepted item should not update");
        assert!(matches!(err, InteractiveError::AlreadyAccepted));
    }
}
