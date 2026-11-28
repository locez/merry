use super::{
    InteractiveError,
    types::{InputRecord, InputRecords, QueuedInputId},
};
use crate::{StepInput, UserMessageInput};
use merry_core::{QueuedInputView, QueuedInputsView};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuedInputState {
    Pending,
    Accepted,
    Removed,
}

#[derive(Debug, Clone)]
struct QueuedInput {
    message: Option<UserMessageInput>,
    state: QueuedInputState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AcceptedQueuedInput {
    view: QueuedInputView,
    message: UserMessageInput,
}

impl AcceptedQueuedInput {
    #[must_use]
    pub(super) fn view(&self) -> &QueuedInputView {
        &self.view
    }

    #[must_use]
    pub(super) fn message(&self) -> &UserMessageInput {
        &self.message
    }
}

/// Single runtime-owned pending-input queue with an explicit auto-dispatch pause flag.
///
/// While `paused` is true (for example, while the user is editing a pending item
/// in the UI or after an interrupt), pending items are never auto-dispatched at
/// turn boundaries. Once resumed, all pending items are accepted together in one burst.
#[derive(Debug, Default)]
pub(super) struct InteractiveInputQueue {
    next_id: u64,
    pending: VecDeque<QueuedInputId>,
    items: HashMap<QueuedInputId, QueuedInput>,
    paused: bool,
}

impl InteractiveInputQueue {
    #[must_use]
    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    #[cfg(test)]
    #[must_use]
    pub(super) fn is_paused(&self) -> bool {
        self.paused
    }

    #[must_use]
    pub(super) fn can_auto_dispatch(&self) -> bool {
        !self.paused && !self.pending.is_empty()
    }

    #[cfg(test)]
    pub(super) fn submit(&mut self, text: &str) -> Result<QueuedInputId, InteractiveError> {
        let sanitized = crate::user_input::sanitize_freeform_text(text);
        validate_interactive_text(&sanitized)?;
        self.submit_message(UserMessageInput::text_only(&sanitized)?)
    }

    pub(super) fn submit_message(
        &mut self,
        message: UserMessageInput,
    ) -> Result<QueuedInputId, InteractiveError> {
        self.next_id = self.next_id.saturating_add(1);
        let id = QueuedInputId::from_u64(self.next_id);
        self.pending.push_back(id);
        self.items.insert(
            id,
            QueuedInput {
                message: Some(message),
                state: QueuedInputState::Pending,
            },
        );
        Ok(id)
    }

    pub(super) fn update(&mut self, id: QueuedInputId, text: &str) -> Result<(), InteractiveError> {
        let sanitized = crate::user_input::sanitize_freeform_text(text);
        validate_interactive_text(&sanitized)?;
        let images = self
            .pending_item(id)?
            .message
            .as_ref()
            .expect("pending queued input owns its message")
            .images()
            .to_vec();
        let message = UserMessageInput::new(&sanitized, images)?;
        self.pending_item_mut(id)?.message = Some(message);
        Ok(())
    }

    pub(super) fn remove(&mut self, id: QueuedInputId) -> Result<(), InteractiveError> {
        let _ = self.pending_item(id)?;
        if !remove_id(&mut self.pending, id) {
            return Err(InteractiveError::UnknownInput);
        }
        let item = self.pending_item_mut(id)?;
        item.message.take();
        item.state = QueuedInputState::Removed;
        if self.pending.is_empty() {
            self.paused = false;
        }
        Ok(())
    }

    pub(super) fn replace_pending_order(
        &mut self,
        ordered: Vec<QueuedInputId>,
    ) -> Result<(), InteractiveError> {
        let current = self.pending.iter().copied().collect::<Vec<_>>();
        if ordered.len() != current.len() {
            return Err(InteractiveError::StalePendingOrder {
                reason: "input count changed",
            });
        }

        let mut seen = HashSet::with_capacity(ordered.len());
        for id in &ordered {
            if !seen.insert(*id) {
                return Err(InteractiveError::InvalidPendingOrder {
                    reason: "input order contains a duplicate item",
                });
            }
            let _ = self.pending_item(*id)?;
        }

        let current_set = current.into_iter().collect::<HashSet<_>>();
        if seen != current_set {
            return Err(InteractiveError::StalePendingOrder {
                reason: "input set changed",
            });
        }

        self.pending = VecDeque::from(ordered);
        Ok(())
    }

    /// Sets whether auto-dispatch is paused. Returns `true` if state changed.
    pub(super) fn set_paused(&mut self, paused: bool) -> bool {
        let effective = paused && !self.pending.is_empty();
        if self.paused == effective {
            return false;
        }
        self.paused = effective;
        true
    }

    /// Pauses pending items when a running turn is interrupted so they remain
    /// reviewable/editable instead of auto-firing immediately.
    pub(super) fn pause_on_interrupt(&mut self) -> bool {
        if self.pending.is_empty() || self.paused {
            return false;
        }
        self.paused = true;
        true
    }

    /// Drains all pending items together for one turn and clears the pause flag.
    pub(super) fn accept_all_pending(&mut self) -> Vec<AcceptedQueuedInput> {
        self.paused = false;
        let ids = std::mem::take(&mut self.pending);
        ids.into_iter()
            .enumerate()
            .filter_map(|(position, id)| self.mark_accepted(id, position))
            .collect()
    }

    pub(super) fn snapshot(&self) -> QueuedInputsView {
        QueuedInputsView {
            items: self
                .pending
                .iter()
                .enumerate()
                .filter_map(|(position, id)| {
                    let item = self.items.get(id)?;
                    (item.state == QueuedInputState::Pending).then(|| QueuedInputView {
                        id: id.as_u64(),
                        text: item
                            .message
                            .as_ref()
                            .expect("pending queued input owns its message")
                            .text()
                            .to_owned(),
                        position,
                    })
                })
                .collect(),
            paused: self.paused,
        }
    }

    pub(super) fn input_records(&self) -> InputRecords {
        InputRecords {
            items: self
                .pending
                .iter()
                .filter_map(|id| {
                    let item = self.items.get(id)?;
                    (item.state == QueuedInputState::Pending).then(|| InputRecord {
                        id: *id,
                        text: item
                            .message
                            .as_ref()
                            .expect("pending queued input owns its message")
                            .text()
                            .to_owned(),
                    })
                })
                .collect(),
            paused: self.paused,
        }
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

    fn mark_accepted(&mut self, id: QueuedInputId, position: usize) -> Option<AcceptedQueuedInput> {
        let item = self.items.get_mut(&id)?;
        if item.state != QueuedInputState::Pending {
            return None;
        }
        let message = item
            .message
            .take()
            .expect("pending queued input owns its message");
        item.state = QueuedInputState::Accepted;
        Some(AcceptedQueuedInput {
            view: QueuedInputView {
                id: id.as_u64(),
                text: message.text().to_owned(),
                position,
            },
            message,
        })
    }
}

pub(super) fn step_input_from_accepted(accepted: &[AcceptedQueuedInput]) -> Option<StepInput> {
    StepInput::from_user_messages(accepted.iter().map(|item| item.message().clone())).ok()
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UserImageInput, UserMessageInput};
    use std::sync::Arc;

    fn image_message(bytes: Arc<[u8]>) -> UserMessageInput {
        UserMessageInput::new(
            "inspect [Image #1]",
            vec![UserImageInput::png("[Image #1]", bytes, 2, 3).expect("valid image input")],
        )
        .expect("valid image message")
    }

    #[test]
    fn queue_update_remove_and_reorder_pending_items() {
        let mut queue = InteractiveInputQueue::default();
        let first = queue.submit("first").expect("valid first");
        let second = queue.submit("second").expect("valid second");

        queue
            .update(first, "updated")
            .expect("pending item updates");
        queue
            .replace_pending_order(vec![second, first])
            .expect("pending item reorders");
        queue.remove(first).expect("pending item removes");

        let snapshot = queue.snapshot();
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].id, second.as_u64());
        assert_eq!(snapshot.items[0].text, "second");
        assert_eq!(snapshot.items[0].position, 0);
    }

    #[test]
    fn queue_pause_blocks_auto_dispatch_and_accept_all_flushes_together() {
        let mut queue = InteractiveInputQueue::default();
        let first = queue.submit("first").expect("valid first");
        queue.submit("second").expect("valid second");

        assert!(queue.can_auto_dispatch());
        assert!(queue.set_paused(true));
        assert!(queue.is_paused());
        assert!(!queue.can_auto_dispatch());

        queue.update(first, "first edited").expect("edit succeeds");
        assert!(queue.set_paused(false));
        assert!(queue.can_auto_dispatch());

        let accepted = queue.accept_all_pending();
        assert_eq!(
            accepted
                .iter()
                .map(|item| item.view().text.as_str())
                .collect::<Vec<_>>(),
            vec!["first edited", "second"]
        );
        assert!(!queue.has_pending());
        assert!(!queue.is_paused());
    }

    #[test]
    fn queue_interrupt_pauses_pending_items_and_removing_last_unpauses() {
        let mut queue = InteractiveInputQueue::default();
        let id = queue.submit("follow up").expect("valid submit");

        assert!(queue.pause_on_interrupt());
        assert!(queue.is_paused());
        assert!(!queue.can_auto_dispatch());
        assert_eq!(queue.snapshot().items[0].text, "follow up");
        assert!(queue.snapshot().paused);

        queue.remove(id).expect("pending item removes");
        assert!(!queue.has_pending());
        assert!(!queue.is_paused());
    }

    #[test]
    fn queue_rejects_edit_after_acceptance() {
        let mut queue = InteractiveInputQueue::default();
        let id = queue.submit("x").expect("valid submit");
        let accepted = queue.accept_all_pending();
        assert_eq!(accepted[0].view().text, "x");

        let err = queue
            .update(id, "changed")
            .expect_err("accepted item should not update");
        assert!(matches!(err, InteractiveError::AlreadyAccepted));
    }

    #[test]
    fn queue_accepts_full_image_message_but_projects_text_only_views() {
        let mut queue = InteractiveInputQueue::default();
        let message = image_message(Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]));
        queue
            .submit_message(message.clone())
            .expect("valid image message should queue");

        let snapshot = queue.snapshot();
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].text, message.text());

        let accepted = queue.accept_all_pending();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].view().text, message.text());
        assert_eq!(accepted[0].message(), &message);
        let input = step_input_from_accepted(&accepted).expect("accepted input should compile");
        assert_eq!(input.user_messages(), [message]);
    }

    #[test]
    fn queue_text_update_preserves_images_and_revalidates_labels() {
        let mut queue = InteractiveInputQueue::default();
        let message = image_message(Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]));
        let id = queue
            .submit_message(message.clone())
            .expect("valid image message should queue");

        queue
            .update(id, "updated [Image #1]")
            .expect("label-preserving update should succeed");
        let accepted = queue.accept_all_pending();
        assert_eq!(accepted[0].message().text(), "updated [Image #1]");
        assert_eq!(accepted[0].message().images(), message.images());

        let second_id = queue
            .submit_message(message)
            .expect("second image message should queue");
        assert!(queue.update(second_id, "label removed").is_err());
        assert_eq!(queue.snapshot().items[0].text, "inspect [Image #1]");
    }

    #[test]
    fn removing_pending_image_message_releases_image_bytes() {
        let bytes = Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10]);
        let message = image_message(Arc::clone(&bytes));
        let mut queue = InteractiveInputQueue::default();
        let id = queue
            .submit_message(message)
            .expect("valid image message should queue");
        assert_eq!(Arc::strong_count(&bytes), 2);

        queue.remove(id).expect("pending message should remove");

        assert_eq!(Arc::strong_count(&bytes), 1);
    }

    #[test]
    fn queue_sanitizes_control_characters_and_rejects_control_only_input() {
        let mut queue = InteractiveInputQueue::default();
        let id = queue
            .submit("line1\r\nline2\rline3\x07\0")
            .expect("CR/CRLF normalize and control characters are stripped");
        assert_eq!(queue.snapshot().items[0].text, "line1\nline2\nline3");

        queue
            .update(id, "updated\r\nline2\rline3\x1b")
            .expect("updated CR/CRLF normalize and control characters are stripped");
        assert_eq!(queue.snapshot().items[0].text, "updated\nline2\nline3");

        let err = queue
            .submit("\x07\0\x1b")
            .expect_err("control-only input becomes blank and stays rejected");
        assert!(matches!(err, InteractiveError::InvalidInput { .. }));
    }
}
