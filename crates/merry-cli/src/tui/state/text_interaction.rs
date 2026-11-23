use crate::tui::{
    copy_controls::CopyTextError,
    links::HoveredLink,
    overlay::Overlay,
    state::TuiState,
    text_interaction::{CapturedText, SelectionViewport, TextSelection},
};
use ratatui::layout::{Position, Rect};

impl TuiState {
    pub(crate) fn hovered_link(&self) -> Option<&HoveredLink> {
        self.hovered_link.as_ref()
    }

    pub(crate) fn set_hovered_link(&mut self, hovered: Option<HoveredLink>) {
        self.hovered_link = hovered;
    }

    pub(crate) fn clear_hovered_link(&mut self) {
        self.hovered_link = None;
    }

    /// Clears hover state on the main timeline and any embedded overlay timeline.
    pub(crate) fn clear_all_hovered_links(&mut self) {
        self.hovered_link = None;
        if let Some(Overlay::PlanView(view)) = self.overlay_mut() {
            view.child_state_mut().clear_hovered_link();
        } else if let Some(Overlay::SubagentInspect(takeover)) = self.overlay_mut() {
            takeover.child_state_mut().clear_hovered_link();
        }
    }

    pub(crate) fn text_selection(&self) -> Option<&TextSelection> {
        self.text_selection.as_ref()
    }

    pub(crate) fn begin_text_selection(&mut self, selection: TextSelection) {
        self.clipboard_feedback = None;
        self.hovered_link = None;
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
    ) -> Result<Option<CapturedText>, CopyTextError> {
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
