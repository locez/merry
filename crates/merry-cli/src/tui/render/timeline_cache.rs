//! Per-item cached timeline layout and O(viewport) virtualized slicing.
//!
//! Historical timeline items are immutable once settled. Caching each item's
//! pre-measured logical rows behind an `Arc<CachedItemLayout>` avoids
//! re-running Markdown parsing, `syntect` syntax highlighting, and line-count
//! measurement on scroll, resize-free repaints, streaming updates, or mouse
//! drag selection.

use crate::tui::{
    copy_controls::CopyTarget,
    preferences::CodeTheme,
    state::TimelineAnchor,
    theme::TuiTheme,
    transcript::{SelectionPolicy, TranscriptRow},
};
use ratatui::text::Line;
use std::sync::{Arc, Mutex};

/// Whether the item immediately following a timeline entry affects its trailing
/// spacing (adjacent single-line commands pack without an extra blank line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NextItemKind {
    None,
    Command,
    Other,
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
}

impl CachedItemLayout {
    pub(crate) fn new(
        rows: Vec<TranscriptRow>,
        local_row_starts: Vec<usize>,
        copy_targets: Vec<CopyTarget>,
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

/// Immutable snapshot of per-item cached layouts plus prefix-summed visual starts.
///
/// Cloning or snapshotting this struct only bumps `Arc` reference counts, so
/// starting a mouse text selection is O(N_items) pointer copies while remaining
/// completely insulated from live streaming mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimelineLayoutSnapshot {
    pub(crate) items: Vec<Arc<CachedItemLayout>>,
    /// Visual start row of each item; length is always `items.len() + 1`.
    pub(crate) item_visual_starts: Vec<usize>,
}

impl TimelineLayoutSnapshot {
    pub(crate) fn new(items: Vec<Arc<CachedItemLayout>>) -> Self {
        let mut item_visual_starts: Vec<usize> = Vec::with_capacity(items.len().saturating_add(1));
        item_visual_starts.push(0);
        for item in &items {
            let next = item_visual_starts
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(item.visual_height());
            item_visual_starts.push(next);
        }
        Self {
            items,
            item_visual_starts,
        }
    }

    pub(crate) fn total_rows(&self) -> usize {
        self.item_visual_starts.last().copied().unwrap_or(0)
    }

    pub(crate) fn item_at_visual(&self, visual: usize) -> Option<usize> {
        let end = self.total_rows();
        if visual >= end {
            return None;
        }
        let index = self
            .item_visual_starts
            .partition_point(|start| *start <= visual)
            .saturating_sub(1);
        (index < self.items.len()).then_some(index)
    }

    /// Returns `(visual_start, visual_end)` for non-blank content rows of `index`.
    pub(crate) fn item_content_visual_range(&self, index: usize) -> Option<(usize, usize)> {
        let start = *self.item_visual_starts.get(index)?;
        let content_height = self.items.get(index)?.content_visual_height;
        (content_height > 0).then_some((start, start.saturating_add(content_height)))
    }

    pub(crate) fn anchor_at(&self, visual: usize) -> Option<TimelineAnchor> {
        let index = self.item_at_visual(visual)?;
        let start = self.item_visual_starts[index];
        Some(TimelineAnchor::new(index, visual.saturating_sub(start)))
    }

    pub(crate) fn selection_policy_at(&self, visual: usize) -> Option<SelectionPolicy> {
        let index = self.item_at_visual(visual)?;
        let item = self.items.get(index)?;
        let local = visual.saturating_sub(self.item_visual_starts[index]);
        let logical = item.logical_at_local_visual(local)?;
        item.rows.get(logical).map(|row| row.selection)
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
            let item = &self.items[index];
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimelineCacheContext {
    pub(crate) width: u16,
    pub(crate) max_reading_width: u16,
    pub(crate) animations: bool,
    pub(crate) code_theme: CodeTheme,
    pub(crate) show_successful_command_output: bool,
    pub(crate) theme: TuiTheme,
}

#[derive(Debug, Clone)]
struct CachedEntry {
    version: u64,
    next_kind: NextItemKind,
    layout: Arc<CachedItemLayout>,
}

#[derive(Debug, Clone, Default)]
struct TimelineRenderCacheInner {
    context: Option<TimelineCacheContext>,
    entries: Vec<Option<CachedEntry>>,
    misses: u64,
}

/// Interior-mutable per-item layout cache attached to `TuiState`.
///
/// Equality ignores cached data so semantic state comparisons in tests are
/// unaffected. Cloning shares immutable `Arc<CachedItemLayout>` payloads.
#[derive(Debug, Default)]
pub(crate) struct TimelineRenderCache {
    inner: Mutex<TimelineRenderCacheInner>,
}

impl Clone for TimelineRenderCache {
    fn clone(&self) -> Self {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        Self {
            inner: Mutex::new(inner),
        }
    }
}

impl PartialEq for TimelineRenderCache {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for TimelineRenderCache {}

impl TimelineRenderCache {
    /// Resolves a full `TimelineLayoutSnapshot` for the current context, computing
    /// only items whose `(version, next_kind)` changed or that are marked volatile.
    pub(crate) fn resolve_snapshot(
        &self,
        context: TimelineCacheContext,
        len: usize,
        item_key: impl Fn(usize) -> (u64, NextItemKind, bool),
        compute_item: impl Fn(usize) -> CachedItemLayout,
    ) -> TimelineLayoutSnapshot {
        // Step 1: check context & collect cache hits under a short lock.
        let mut resolved: Vec<Option<Arc<CachedItemLayout>>> = vec![None; len];
        let mut keys: Vec<(u64, NextItemKind, bool)> = Vec::with_capacity(len);
        for index in 0..len {
            keys.push(item_key(index));
        }
        {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.context.as_ref() != Some(&context) {
                guard.context = Some(context.clone());
                guard.entries.clear();
            }
            if guard.entries.len() > len {
                guard.entries.truncate(len);
            }
            for (index, &(version, next_kind, volatile)) in keys.iter().enumerate() {
                if volatile {
                    continue;
                }
                if let Some(Some(entry)) = guard.entries.get(index)
                    && entry.version == version
                    && entry.next_kind == next_kind
                {
                    resolved[index] = Some(Arc::clone(&entry.layout));
                }
            }
        }

        // Step 2: compute only missed or volatile items outside the lock.
        let mut newly_computed: Vec<(usize, u64, NextItemKind, bool, Arc<CachedItemLayout>)> =
            Vec::new();
        for (index, slot) in resolved.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            let (version, next_kind, volatile) = keys[index];
            let layout = Arc::new(compute_item(index));
            *slot = Some(Arc::clone(&layout));
            newly_computed.push((index, version, next_kind, volatile, layout));
        }

        // Step 3: store non-volatile newly computed items back into the cache.
        if !newly_computed.is_empty() {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.context.as_ref() == Some(&context) {
                if guard.entries.len() < len {
                    guard.entries.resize(len, None);
                }
                for (index, version, next_kind, volatile, layout) in newly_computed {
                    guard.misses = guard.misses.saturating_add(1);
                    if !volatile && index < guard.entries.len() {
                        guard.entries[index] = Some(CachedEntry {
                            version,
                            next_kind,
                            layout,
                        });
                    }
                }
            }
        }

        let items = resolved
            .into_iter()
            .map(|slot| slot.expect("every timeline item resolved"))
            .collect();
        TimelineLayoutSnapshot::new(items)
    }

    #[cfg(test)]
    pub(crate) fn misses(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .misses
    }
}
