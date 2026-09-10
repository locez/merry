use crate::tui::{
    copy_controls::CopyTextError,
    state::TuiState,
    text_interaction::{SelectionViewport, TextSelection},
};
use ratatui::layout::{Position, Rect};

impl TuiState {
    pub(crate) fn text_selection(&self) -> Option<&TextSelection> {
        self.text_selection.as_ref()
    }

    pub(crate) fn begin_text_selection(&mut self, selection: TextSelection) {
        self.clipboard_feedback = None;
        self.text_selection = Some(selection);
    }

    pub(crate) fn drag_text_selection(&mut self, position: Position) {
        if let Some(selection) = &mut self.text_selection {
            selection.drag_to(position);
        }
    }

    pub(crate) fn finish_text_selection(
        &mut self,
        position: Position,
    ) -> Result<Option<String>, CopyTextError> {
        self.text_selection
            .take()
            .map_or(Ok(None), |selection| selection.release(position))
    }

    pub(crate) fn text_selection_is_autoscrolling(&self) -> bool {
        self.text_selection
            .as_ref()
            .is_some_and(TextSelection::is_autoscrolling)
    }

    pub(crate) fn autoscroll_text_selection(&mut self) {
        if let Some(SelectionViewport { anchor, offset }) = self
            .text_selection
            .as_mut()
            .and_then(TextSelection::autoscroll)
        {
            self.timeline_review_user_index = None;
            self.pending_empty_input_quit = false;
            self.record_timeline_viewport(anchor, offset);
        }
    }

    pub(crate) fn clear_text_selection(&mut self) -> bool {
        self.text_selection.take().is_some()
    }

    pub(crate) fn validate_text_selection_area(&mut self, area: Rect) {
        if self.overlay().is_some()
            || self
                .text_selection
                .as_ref()
                .is_some_and(|selection| selection.area() != area)
        {
            self.clear_text_selection();
        }
    }
}
