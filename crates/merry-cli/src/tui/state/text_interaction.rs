use crate::tui::{
    copy_controls::CopyTextError,
    links::HoveredLink,
    overlay::Overlay,
    render::timeline_layout::materialize_snapshot_rows,
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
        let Some(mut selection) = self.text_selection.take() else {
            return Ok(None);
        };
        let (start_row, end_row) = selection.selected_row_range(position);
        let width = selection.area().width;
        let mut adjustments = Vec::new();
        materialize_snapshot_rows(
            self,
            selection.snapshot_mut(),
            width,
            start_row,
            end_row,
            |old_end, delta| adjustments.push((old_end, delta)),
        );
        for (old_end, delta) in adjustments {
            selection.adjust_rows_after_materialization(old_end, delta);
        }
        selection.release(position)
    }

    pub(crate) fn text_selection_is_autoscrolling(&self) -> bool {
        self.text_selection
            .as_ref()
            .is_some_and(TextSelection::is_autoscrolling)
    }

    pub(crate) fn autoscroll_text_selection(&mut self) {
        let Some(mut selection) = self.text_selection.take() else {
            return;
        };
        let viewport = selection.autoscroll();
        if viewport.is_some() {
            let (start_row, end_row) = selection.visible_row_range();
            let width = selection.area().width;
            let mut adjustments = Vec::new();
            materialize_snapshot_rows(
                self,
                selection.snapshot_mut(),
                width,
                start_row,
                end_row,
                |old_end, delta| adjustments.push((old_end, delta)),
            );
            for (old_end, delta) in adjustments {
                selection.adjust_rows_after_materialization(old_end, delta);
            }
        }
        let updated_viewport = if viewport.is_some() {
            let (scroll, _) = selection.visible_row_range();
            let max_scroll = selection
                .snapshot_mut()
                .total_rows()
                .saturating_sub(usize::from(selection.area().height));
            selection
                .snapshot_mut()
                .anchor_at(scroll)
                .map(|anchor| SelectionViewport {
                    anchor,
                    offset: max_scroll.saturating_sub(scroll),
                })
        } else {
            None
        };
        self.text_selection = Some(selection);
        if let Some(SelectionViewport { anchor, offset }) = updated_viewport {
            self.timeline_review_target = None;
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
