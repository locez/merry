//! Inline pending-queue editing state helpers for [`TuiState`].

use super::{PendingQueueEditor, QueuePreviewState, TuiState};
use crate::tui::input::TextInput;
use crossterm::event::KeyEvent;
use merry::events::QueuedInputsView;

/// Outcome of committing an active pending-queue edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingEditCommit {
    Update { id: u64, text: String },
    Remove { id: u64 },
}

impl TuiState {
    pub(crate) fn queue_preview(&self) -> &QueuePreviewState {
        &self.queue_preview
    }

    pub(crate) fn has_queue_preview_items(&self) -> bool {
        !self.queue_preview.is_empty()
    }

    pub(crate) fn update_queue_preview(&mut self, inputs: QueuedInputsView) {
        self.queue_preview = QueuePreviewState::from_inputs(inputs);
        if let Some(editor) = self.pending_editor.as_ref()
            && !self
                .queue_preview
                .items
                .iter()
                .any(|item| item.id == editor.id)
        {
            self.pending_editor = None;
        }
    }

    pub(crate) fn pending_editor(&self) -> Option<&PendingQueueEditor> {
        self.pending_editor.as_ref()
    }

    pub(crate) fn is_editing_pending(&self) -> bool {
        self.pending_editor.is_some()
    }

    pub(crate) fn editing_pending_id(&self) -> Option<u64> {
        self.pending_editor.as_ref().map(|editor| editor.id)
    }

    /// Starts inline editing for pending item `id`.
    ///
    /// Returns `Some(true)` if the runtime queue was not yet paused and should be
    /// paused immediately, `Some(false)` if editing switched between items while
    /// already paused, or `None` if `id` is missing or already being edited.
    pub(crate) fn begin_pending_edit(&mut self, id: u64) -> Option<bool> {
        if self.editing_pending_id() == Some(id) {
            return None;
        }
        let text = self
            .queue_preview
            .items
            .iter()
            .find(|item| item.id == id)?
            .text
            .clone();
        let paused_before_edit = self
            .pending_editor
            .as_ref()
            .map_or(self.queue_preview.paused, |editor| {
                editor.paused_before_edit
            });
        let already_paused = self.pending_editor.is_some() || self.queue_preview.paused;
        let mut input = TextInput::default();
        input.replace_text(text);
        self.pending_editor = Some(PendingQueueEditor {
            id,
            input,
            paused_before_edit,
        });
        self.queue_preview.paused = true;
        Some(!already_paused)
    }

    /// Cancels the active pending-queue edit and returns the `paused` state to restore.
    pub(crate) fn cancel_pending_edit(&mut self) -> Option<bool> {
        let editor = self.pending_editor.take()?;
        self.queue_preview.paused = editor.paused_before_edit;
        Some(editor.paused_before_edit)
    }

    /// Commits the current pending edit and immediately switches inline editing to
    /// `next_id` while preserving the original `paused_before_edit` state.
    pub(crate) fn switch_pending_edit(&mut self, next_id: u64) -> Option<PendingEditCommit> {
        let paused_before_edit = self.pending_editor.as_ref()?.paused_before_edit;
        let commit = self.commit_pending_edit()?;
        if let Some(text) = self
            .queue_preview
            .items
            .iter()
            .find(|item| item.id == next_id)
            .map(|item| item.text.clone())
        {
            let mut input = TextInput::default();
            input.replace_text(text);
            self.pending_editor = Some(PendingQueueEditor {
                id: next_id,
                input,
                paused_before_edit,
            });
            self.queue_preview.paused = true;
        }
        Some(commit)
    }

    /// Commits the active pending-queue edit and clears the editor.
    ///
    /// Optimistically updates the local preview text so the UI reflects the edit
    /// immediately before the runtime's `QueuedInputsChanged` arrives.
    pub(crate) fn commit_pending_edit(&mut self) -> Option<PendingEditCommit> {
        let editor = self.pending_editor.take()?;
        let text = editor.input.text().trim().to_owned();
        if text.is_empty() {
            self.queue_preview.items.retain(|item| item.id != editor.id);
            if self.queue_preview.items.is_empty() {
                self.queue_preview.paused = false;
            }
            return Some(PendingEditCommit::Remove { id: editor.id });
        }
        if let Some(item) = self
            .queue_preview
            .items
            .iter_mut()
            .find(|item| item.id == editor.id)
        {
            item.text = text.clone();
        }
        Some(PendingEditCommit::Update {
            id: editor.id,
            text,
        })
    }

    pub(crate) fn handle_pending_edit_key(&mut self, key: KeyEvent) {
        if let Some(editor) = self.pending_editor.as_mut() {
            editor.input.handle_key(key);
        }
    }

    pub(crate) fn paste_into_pending_edit(&mut self, text: &str) -> bool {
        let Some(editor) = self.pending_editor.as_mut() else {
            return false;
        };
        editor.input.insert_paste(text);
        true
    }

    pub(crate) fn selected_pending_edit_text(&self) -> Option<String> {
        self.pending_editor
            .as_ref()
            .and_then(|editor| editor.input.selected_text())
    }

    pub(crate) fn cut_pending_edit_selection(&mut self) -> Option<String> {
        self.pending_editor
            .as_mut()
            .and_then(|editor| editor.input.cut_selection())
    }

    pub(crate) fn clear_pending_edit_selection(&mut self) -> bool {
        self.pending_editor
            .as_mut()
            .is_some_and(|editor| editor.input.clear_selection())
    }
}
