//! On-demand upward scrollback paging of archived session segments (`archive/seg_NNNN.jsonl`).

use super::{StopFeedbackState, TimelineItem, TuiState};

impl TuiState {
    /// Registers surviving archived segment numbers (sorted ascending) that can
    /// be lazily prepended when the user scrolls to the top of the timeline.
    pub(crate) fn set_unloaded_archive_segments(&mut self, mut segments: Vec<u32>) {
        segments.sort_unstable();
        segments.dedup();
        self.unloaded_archive_segments = segments;
        self.loading_archive_segment = None;
    }

    /// Scrolls the timeline upward and, if the viewport is already near or at
    /// the top with unloaded archived segments available, marks and returns the
    /// newest unloaded segment number to fetch asynchronously.
    pub(crate) fn scroll_timeline_up_and_maybe_load_archive(
        &mut self,
        lines: usize,
    ) -> Option<u32> {
        let near_top = self.is_near_timeline_top();
        self.scroll_timeline_up_by(lines);
        if !near_top || self.loading_archive_segment.is_some() {
            return None;
        }
        let next_segment = *self.unloaded_archive_segments.last()?;
        self.loading_archive_segment = Some(next_segment);
        Some(next_segment)
    }

    fn is_near_timeline_top(&self) -> bool {
        if self.timeline.is_empty() {
            return true;
        }
        if let Some(anchor) = self.timeline_anchor {
            return anchor.item_index == 0 && anchor.row <= 8;
        }
        self.timeline_last_scroll.is_some_and(|scroll| scroll <= 8)
    }

    /// Prepends projected items from an archived segment onto the timeline while
    /// shifting anchors, review indices, comments, and layout cache slots so the
    /// visible reading position stays anchored without rebuilding existing items.
    pub(crate) fn prepend_archived_segment_items(
        &mut self,
        segment: u32,
        items: Vec<TimelineItem>,
    ) -> usize {
        self.unloaded_archive_segments
            .retain(|&candidate| candidate != segment);
        if self.loading_archive_segment == Some(segment) {
            self.loading_archive_segment = None;
        }
        let shift = items.len();
        if shift == 0 {
            return 0;
        }
        let mut prepended_versions = Vec::with_capacity(shift);
        for _ in 0..shift {
            prepended_versions.push(self.alloc_timeline_version());
        }
        let mut next_timeline = items;
        next_timeline.append(&mut self.timeline);
        self.timeline = next_timeline;
        prepended_versions.append(&mut self.timeline_versions);
        self.timeline_versions = prepended_versions;

        self.timeline_cache.prepend_unmeasured(shift);
        if let Some(anchor) = &mut self.timeline_anchor {
            anchor.item_index = anchor.item_index.saturating_add(shift);
        }
        if let Some(review_index) = &mut self.timeline_review_user_index {
            *review_index = review_index.saturating_add(shift);
        }
        self.comments.shift_item_indices(shift);
        if let StopFeedbackState::Pending(index) = &mut self.stop_feedback {
            *index = index.saturating_add(shift);
        }
        self.text_selection = None;
        self.hovered_link = None;
        shift
    }

    /// Clears in-flight load state when reading an archived segment fails so
    /// subsequent scroll events do not spin on a broken segment.
    pub(crate) fn fail_archive_segment_load(&mut self, segment: u32) {
        self.unloaded_archive_segments
            .retain(|&candidate| candidate != segment);
        if self.loading_archive_segment == Some(segment) {
            self.loading_archive_segment = None;
        }
    }
}
