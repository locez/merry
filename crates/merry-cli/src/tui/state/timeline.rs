use super::TuiState;

/// Identifies the displayed item and wrapped row, independently of later timeline growth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineAnchor {
    pub(crate) item_index: usize,
    pub(crate) row: usize,
}

impl TimelineAnchor {
    pub(crate) fn new(item_index: usize, row: usize) -> Self {
        Self { item_index, row }
    }
}

impl TuiState {
    pub(crate) fn timeline_anchor(&self) -> Option<TimelineAnchor> {
        self.timeline_anchor
    }

    pub(crate) fn timeline_has_updates(&self) -> bool {
        self.timeline_has_updates
    }

    pub(crate) fn is_timeline_detached(&self) -> bool {
        self.timeline_anchor.is_some()
            || self.timeline_scroll_offset > 0
            || self.is_timeline_reviewing()
    }

    /// Records the actual viewport after layout, keeping subsequent updates anchored.
    pub(crate) fn record_timeline_viewport(&mut self, anchor: TimelineAnchor, offset: usize) {
        self.timeline_anchor = Some(anchor);
        self.timeline_scroll_offset = offset;
    }

    pub(super) fn note_timeline_update(&mut self) {
        if self.is_timeline_detached() {
            self.timeline_has_updates = true;
        }
    }
}
