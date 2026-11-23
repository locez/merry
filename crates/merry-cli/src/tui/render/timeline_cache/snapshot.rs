//! Pure layout data structures, width-independent height estimation profiles,
//! and O(viewport) virtualized timeline slicing.

use crate::tui::{
    copy_controls::CopyTarget,
    links::LinkTarget,
    state::TimelineAnchor,
    transcript::{SelectionPolicy, TranscriptRow},
};
use ratatui::text::Line;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

/// Whether the item immediately following a timeline entry affects its trailing
/// spacing (adjacent single-line commands pack without an extra blank line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextItemKind {
    None,
    Command,
    Other,
}

/// Lightweight metadata describing one timeline item for cache resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineItemMeta {
    pub(crate) version: u64,
    pub(crate) next_kind: NextItemKind,
    pub(crate) volatile: bool,
    pub(crate) is_user: bool,
    pub(crate) has_trailing_blank_estimate: bool,
}

/// Width-independent per-line display widths for cheap height estimation across resizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WidthProfile {
    line_widths: Vec<u32>,
    overhead_rows: usize,
    horizontal_inset: u16,
    respect_max_reading_width: bool,
}

impl WidthProfile {
    pub(crate) fn fixed(rows: usize) -> Self {
        Self {
            line_widths: Vec::new(),
            overhead_rows: rows.max(1),
            horizontal_inset: 0,
            respect_max_reading_width: false,
        }
    }

    pub(crate) fn from_text(
        text: &str,
        overhead_rows: usize,
        horizontal_inset: u16,
        respect_max_reading_width: bool,
    ) -> Self {
        let line_widths = if text.is_empty() {
            vec![0]
        } else {
            text.split('\n')
                .map(|line| u32::try_from(UnicodeWidthStr::width(line)).unwrap_or(u32::MAX))
                .collect()
        };
        Self {
            line_widths,
            overhead_rows,
            horizontal_inset,
            respect_max_reading_width,
        }
    }

    pub(crate) fn estimate_content_height(
        &self,
        region_width: u16,
        max_reading_width: u16,
    ) -> usize {
        if self.line_widths.is_empty() {
            return self.overhead_rows.max(1);
        }
        let effective_width = if self.respect_max_reading_width && max_reading_width > 0 {
            region_width.min(max_reading_width)
        } else {
            region_width
        };
        let wrap_width = usize::from(effective_width.saturating_sub(self.horizontal_inset).max(1));
        let text_rows = self.line_widths.iter().fold(0_usize, |total, &width| {
            let rows = if width == 0 {
                1
            } else {
                (width as usize).div_ceil(wrap_width)
            };
            total.saturating_add(rows)
        });
        self.overhead_rows.saturating_add(text_rows).max(1)
    }
}

/// Lightweight 1D position descriptor for a `TimelineItem::User` entry, used by
/// sticky header calculation without scanning or cloning historical user rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UserPromptDescriptor {
    pub(crate) item_index: usize,
    pub(crate) y: usize,
    pub(crate) next_item_y: usize,
    pub(crate) content_height: usize,
}

/// Pre-measured layout for a single `TimelineItem` at a fixed width and theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CachedItemLayout {
    /// Logical rows (after oversized-line splitting, including any trailing blank row).
    pub(crate) rows: Vec<TranscriptRow>,
    /// Visual row start of each logical row relative to the item start.
    /// Length is always `rows.len() + 1`; the last element equals `visual_height`.
    pub(crate) local_row_starts: Vec<usize>,
    /// Visual height excluding trailing blank spacing rows.
    pub(crate) content_visual_height: usize,
    /// Copy controls with `line_index` relative to `self.rows`.
    pub(crate) copy_targets: Vec<CopyTarget>,
    /// Hyperlink hit spans with `line_index` relative to `self.rows`.
    pub(crate) link_targets: Vec<LinkTarget>,
}

impl CachedItemLayout {
    pub(crate) fn new(
        rows: Vec<TranscriptRow>,
        local_row_starts: Vec<usize>,
        copy_targets: Vec<CopyTarget>,
        link_targets: Vec<LinkTarget>,
    ) -> Self {
        debug_assert_eq!(local_row_starts.len(), rows.len().saturating_add(1));
        let mut content_end = rows.len();
        while content_end > 0
            && rows
                .get(content_end - 1)
                .is_some_and(|row| row.display.spans.iter().all(|span| span.content.is_empty()))
        {
            content_end -= 1;
        }
        let content_visual_height = local_row_starts.get(content_end).copied().unwrap_or(0);
        Self {
            rows,
            local_row_starts,
            content_visual_height,
            copy_targets,
            link_targets,
        }
    }

    pub(crate) fn visual_height(&self) -> usize {
        self.local_row_starts.last().copied().unwrap_or(0)
    }

    /// Maps a visual row offset relative to this item to its logical row index.
    pub(crate) fn logical_at_local_visual(&self, local_visual: usize) -> Option<usize> {
        let end = self.visual_height();
        if local_visual >= end {
            return None;
        }
        Some(
            self.local_row_starts
                .partition_point(|start| *start <= local_visual)
                .saturating_sub(1),
        )
    }
}

/// Bounded slice of logical lines overlapping a viewport, plus the small
/// residual scroll within the first included logical line.
pub(crate) struct TimelineViewport {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) scroll: u16,
}

/// Immutable snapshot of per-item heights, prefix-summed visual starts, lightweight
/// user prompt descriptors, and materialized `Arc<CachedItemLayout>`s near the viewport.
///
/// Cold or evicted off-screen items retain exact or estimated heights in
/// `item_heights` / `item_visual_starts` while keeping `items[index] == None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimelineLayoutSnapshot {
    pub(crate) items: Vec<Option<Arc<CachedItemLayout>>>,
    pub(crate) item_heights: Vec<usize>,
    pub(crate) item_content_heights: Vec<usize>,
    /// Visual start row of each item; length is always `items.len() + 1`.
    pub(crate) item_visual_starts: Vec<usize>,
    /// Sorted descriptors for `TimelineItem::User` entries with non-zero content height.
    pub(crate) user_prompts: Vec<UserPromptDescriptor>,
}

impl TimelineLayoutSnapshot {
    /// Builds a fully materialized snapshot for tests and direct callers.
    #[cfg(test)]
    pub(crate) fn with_user_indices(
        layouts: Vec<Arc<CachedItemLayout>>,
        user_indices: &[usize],
    ) -> Self {
        let item_heights = layouts
            .iter()
            .map(|item| item.visual_height())
            .collect::<Vec<_>>();
        let item_content_heights = layouts
            .iter()
            .map(|item| item.content_visual_height)
            .collect::<Vec<_>>();
        let item_visual_starts = prefix_sum_starts(&item_heights);
        let user_prompts = user_indices
            .iter()
            .copied()
            .filter_map(|index| {
                let y = *item_visual_starts.get(index)?;
                let next_item_y = item_visual_starts
                    .get(index + 1)
                    .copied()
                    .unwrap_or_else(|| y.saturating_add(item_heights[index]));
                let content_height = *item_content_heights.get(index)?;
                (content_height > 0).then_some(UserPromptDescriptor {
                    item_index: index,
                    y,
                    next_item_y,
                    content_height,
                })
            })
            .collect();
        Self {
            items: layouts.into_iter().map(Some).collect(),
            item_heights,
            item_content_heights,
            item_visual_starts,
            user_prompts,
        }
    }

    pub(crate) fn item_layout(&self, index: usize) -> Option<&Arc<CachedItemLayout>> {
        self.items.get(index)?.as_ref()
    }

    pub(crate) fn item_height(&self, index: usize) -> usize {
        self.item_heights.get(index).copied().unwrap_or(0)
    }

    pub(crate) fn total_rows(&self) -> usize {
        self.item_visual_starts.last().copied().unwrap_or(0)
    }

    pub(crate) fn item_at_visual(&self, visual: usize) -> Option<usize> {
        item_at_visual_in_starts(&self.item_visual_starts, self.items.len(), visual)
    }

    /// Returns `(visual_start, visual_end)` for non-blank content rows of `index`.
    pub(crate) fn item_content_visual_range(&self, index: usize) -> Option<(usize, usize)> {
        let start = *self.item_visual_starts.get(index)?;
        let content_height = *self.item_content_heights.get(index)?;
        (content_height > 0).then_some((start, start.saturating_add(content_height)))
    }

    pub(crate) fn anchor_at(&self, visual: usize) -> Option<TimelineAnchor> {
        let index = self.item_at_visual(visual)?;
        let start = self.item_visual_starts[index];
        Some(TimelineAnchor::new(index, visual.saturating_sub(start)))
    }

    pub(crate) fn selection_policy_at(&self, visual: usize) -> Option<SelectionPolicy> {
        let index = self.item_at_visual(visual)?;
        let item = self.item_layout(index)?;
        let local = visual.saturating_sub(self.item_visual_starts[index]);
        let logical = item.logical_at_local_visual(local)?;
        item.rows.get(logical).map(|row| row.selection)
    }

    /// Returns item indices overlapping `[start_row .. end_row)` whose layout is not yet materialized.
    pub(crate) fn unmaterialized_in_row_range(
        &self,
        start_row: usize,
        end_row: usize,
    ) -> Vec<usize> {
        let total = self.total_rows();
        if self.items.is_empty() || start_row >= end_row || start_row >= total {
            return Vec::new();
        }
        let clamped_end = end_row.min(total);
        let first_item = self
            .item_visual_starts
            .partition_point(|start| *start <= start_row)
            .saturating_sub(1);
        let mut missing = Vec::new();
        for index in first_item..self.items.len() {
            let item_start = self.item_visual_starts[index];
            if item_start >= clamped_end {
                break;
            }
            if self.items[index].is_none() {
                missing.push(index);
            }
        }
        missing
    }

    /// Installs a newly materialized layout into an existing snapshot (used when
    /// mouse drag selection autoscrolls into previously evicted cold items).
    ///
    /// Returns `(old_item_end, height_delta)` so the caller can shift document
    /// coordinates strictly below this item without disturbing content inside it.
    pub(crate) fn patch_materialized_item(
        &mut self,
        index: usize,
        layout: Arc<CachedItemLayout>,
    ) -> Option<(usize, isize)> {
        let old_height = *self.item_heights.get(index)?;
        let old_start = *self.item_visual_starts.get(index)?;
        let old_end = old_start.saturating_add(old_height);
        let new_height = layout.visual_height();
        let new_content_height = layout.content_visual_height;
        self.items[index] = Some(layout);
        self.item_heights[index] = new_height;
        self.item_content_heights[index] = new_content_height;

        let delta = new_height as isize - old_height as isize;
        if delta != 0 {
            for start in &mut self.item_visual_starts[index + 1..] {
                *start = shift_usize(*start, delta);
            }
            for prompt in &mut self.user_prompts {
                if prompt.item_index == index {
                    prompt.content_height = new_content_height;
                    prompt.next_item_y = shift_usize(prompt.next_item_y, delta);
                } else if prompt.item_index > index {
                    prompt.y = shift_usize(prompt.y, delta);
                    prompt.next_item_y = shift_usize(prompt.next_item_y, delta);
                }
            }
        }
        Some((old_end, delta))
    }

    /// Returns only the logical lines overlapping `[scroll .. scroll + height]`.
    ///
    /// Cost is O(log N_items + log N_rows_in_item + visible_rows), avoiding
    /// whole-document wrapping inside `ratatui::Paragraph` and keeping `scroll`
    /// well below `u16::MAX` even on transcripts with >65,535 rows.
    pub(crate) fn viewport_slice(&self, scroll: usize, height: u16) -> TimelineViewport {
        let total = self.total_rows();
        if height == 0 || scroll >= total || self.items.is_empty() {
            return TimelineViewport {
                lines: Vec::new(),
                scroll: 0,
            };
        }

        let range_end = scroll.saturating_add(usize::from(height)).min(total);
        let first_item = self
            .item_visual_starts
            .partition_point(|start| *start <= scroll)
            .saturating_sub(1);

        let mut lines = Vec::with_capacity(usize::from(height).saturating_add(4));
        let mut first_row_visual_start: Option<usize> = None;

        for index in first_item..self.items.len() {
            let item_start = self.item_visual_starts[index];
            if item_start >= range_end {
                break;
            }
            let Some(item) = self.item_layout(index) else {
                continue;
            };
            let item_height = item.visual_height();
            if item_height == 0 {
                continue;
            }
            let local_start = scroll.saturating_sub(item_start);
            let local_end = range_end.saturating_sub(item_start).min(item_height);
            if local_start >= local_end {
                continue;
            }
            let first_logical = item
                .local_row_starts
                .partition_point(|start| *start <= local_start)
                .saturating_sub(1);
            let last_logical = item
                .local_row_starts
                .partition_point(|start| *start < local_end)
                .min(item.rows.len());
            if first_logical >= last_logical {
                continue;
            }
            if first_row_visual_start.is_none() {
                first_row_visual_start =
                    Some(item_start.saturating_add(item.local_row_starts[first_logical]));
            }
            for row in &item.rows[first_logical..last_logical] {
                lines.push(row.display.clone());
            }
        }

        let local_scroll = first_row_visual_start
            .map(|origin| scroll.saturating_sub(origin))
            .unwrap_or(0);
        TimelineViewport {
            lines,
            scroll: u16::try_from(local_scroll).unwrap_or(u16::MAX),
        }
    }
}

pub(super) fn prefix_sum_starts(heights: &[usize]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(heights.len().saturating_add(1));
    starts.push(0);
    let mut acc = 0_usize;
    for &height in heights {
        acc = acc.saturating_add(height);
        starts.push(acc);
    }
    starts
}

pub(super) fn item_at_visual_in_starts(
    starts: &[usize],
    len: usize,
    visual: usize,
) -> Option<usize> {
    let end = starts.last().copied().unwrap_or(0);
    if len == 0 || visual >= end {
        return None;
    }
    let index = starts
        .partition_point(|start| *start <= visual)
        .saturating_sub(1);
    (index < len).then_some(index)
}

pub(crate) fn shift_usize(value: usize, delta: isize) -> usize {
    if delta >= 0 {
        value.saturating_add(delta as usize)
    } else {
        value.saturating_sub(delta.unsigned_abs())
    }
}
