//! Lazy per-item cached timeline layout, width-independent height estimation,
//! O(viewport) virtualized slicing, and bounded off-screen render-cache eviction.
//!
//! Historical timeline items are immutable once settled. Instead of parsing
//! Markdown and running `syntect` syntax highlighting across every item `0..len`
//! on initial draw or terminal resize, the cache separates:
//!
//! 1. **Width-independent line-width profiles (`WidthProfile`)**: cheap
//!    `Vec<u32>` display widths that survive terminal resizes and estimate item
//!    heights in O(lines) integer division.
//! 2. **Lightweight height map (`visual_height`, `content_visual_height`,
//!    `measured`)**: kept for every item so scrolling, scrollbar math, and
//!    binary-search hit testing stay exact without holding heavyweight render
//!    trees off-screen.
//! 3. **Materialized `Arc<CachedItemLayout>`**: computed lazily only for the
//!    visible viewport plus a small below-margin (`MEASURE_MARGIN_ITEMS`),
//!    optional bottom-anchored warm pages (`RESUME_WARM_PAGES`), and required
//!    anchors (pinned sticky user header, active comments). Off-screen layouts
//!    outside `±EVICT_KEEP_MARGIN_ITEMS` are periodically evicted while keeping
//!    their exact measured heights intact.

mod snapshot;

pub(crate) use snapshot::{
    CachedItemLayout, NextItemKind, TimelineItemMeta, TimelineLayoutSnapshot, UserPromptDescriptor,
    WidthProfile, shift_usize,
};
use snapshot::{item_at_visual_in_starts, prefix_sum_starts};

use crate::{
    config::{BrandMarkMode, CopyButtonPosition},
    tui::{preferences::CodeTheme, theme::TuiTheme},
};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

/// Extra items measured exactly below the visible viewport so downward scrolling
/// lands on already-measured heights. Deliberately no above-margin during normal
/// detached browsing so measuring above-viewport items never shifts the top row.
pub(crate) const MEASURE_MARGIN_ITEMS: usize = 8;

/// Items kept materialized on each side of the active measurement window before
/// sweeping off-screen render caches.
pub(crate) const EVICT_KEEP_MARGIN_ITEMS: usize = 128;

/// Extra viewport heights measured above the bottom when following the tail so
/// an immediate scroll-up after resume reveals exact heights without jumping.
pub(crate) const RESUME_WARM_PAGES: usize = 3;

/// Slack above the keep window before running an off-screen eviction sweep.
const EVICT_SLACK_ITEMS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimelineCacheContext {
    pub(crate) width: u16,
    pub(crate) max_reading_width: u16,
    pub(crate) animations: bool,
    pub(crate) code_theme: CodeTheme,
    pub(crate) show_successful_command_output: bool,
    pub(crate) brand_mark: BrandMarkMode,
    pub(crate) copy_button_position: CopyButtonPosition,
    pub(crate) theme: TuiTheme,
}

#[derive(Debug, Clone)]
struct CachedLayoutSlot {
    version: u64,
    next_kind: NextItemKind,
    visual_height: usize,
    content_visual_height: usize,
    measured: bool,
    layout: Option<Arc<CachedItemLayout>>,
}

#[derive(Debug, Clone)]
struct CachedWidthProfile {
    version: u64,
    profile: WidthProfile,
}

#[derive(Debug, Clone, Default)]
struct TimelineRenderCacheInner {
    context: Option<TimelineCacheContext>,
    slots: Vec<Option<CachedLayoutSlot>>,
    /// Width-independent per-item line-width profiles; preserved across terminal resizes.
    width_profiles: Vec<Option<CachedWidthProfile>>,
    misses: u64,
}

/// Interior-mutable lazy per-item layout cache attached to `TuiState`.
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

/// Inputs describing the viewport and anchoring mode for [`TimelineRenderCache::resolve_for_viewport`].
pub(crate) struct ViewportResolveRequest<'a> {
    pub(crate) context: TimelineCacheContext,
    pub(crate) len: usize,
    pub(crate) viewport_height: u16,
    pub(crate) is_follow_bottom: bool,
    pub(crate) extra_required: &'a [usize],
}

/// Callbacks supplying per-item metadata, width profiles, scroll geometry, and
/// full item rendering to [`TimelineRenderCache::resolve_for_viewport`].
pub(crate) trait TimelineLayoutProvider {
    fn item_meta(&self, index: usize) -> TimelineItemMeta;
    fn build_width_profile(&self, index: usize) -> WidthProfile;
    fn compute_scroll(&self, starts: &[usize], heights: &[usize]) -> usize;
    fn sticky_user_for_scroll(
        &self,
        prompts: &[UserPromptDescriptor],
        scroll: usize,
    ) -> Option<usize>;
    fn compute_item(&self, index: usize) -> CachedItemLayout;
}

impl TimelineRenderCache {
    /// Resolves a `TimelineLayoutSnapshot` lazily for the active viewport.
    ///
    /// Only items overlapping the visible viewport, its below-margin
    /// (`MEASURE_MARGIN_ITEMS`), optional bottom-anchored warm pages
    /// (`RESUME_WARM_PAGES`), the active pinned user header, and `extra_required`
    /// are materialized via `provider.compute_item`. All other items use cheap
    /// width-independent height estimates or previously measured heights, and
    /// off-screen layouts beyond `±EVICT_KEEP_MARGIN_ITEMS` are swept to bound RAM.
    pub(crate) fn resolve_for_viewport(
        &self,
        request: ViewportResolveRequest<'_>,
        provider: &impl TimelineLayoutProvider,
    ) -> TimelineLayoutSnapshot {
        let len = request.len;
        if len == 0 {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.context = Some(request.context);
            guard.slots.clear();
            guard.width_profiles.clear();
            return TimelineLayoutSnapshot {
                items: Vec::new(),
                item_heights: Vec::new(),
                item_content_heights: Vec::new(),
                item_visual_starts: vec![0],
                user_prompts: Vec::new(),
            };
        }

        let metas = (0..len)
            .map(|index| provider.item_meta(index))
            .collect::<Vec<_>>();

        // Step 1: check which width profiles are missing or stale (survives resizes!).
        let missing_profiles = {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.context.as_ref() != Some(&request.context) {
                guard.context = Some(request.context.clone());
                guard.slots.clear();
            }
            if guard.width_profiles.len() > len {
                guard.width_profiles.truncate(len);
            } else if guard.width_profiles.len() < len {
                guard.width_profiles.resize(len, None);
            }
            let mut missing = Vec::new();
            for (index, meta) in metas.iter().enumerate() {
                let stale = !matches!(
                    guard.width_profiles.get(index),
                    Some(Some(cached)) if cached.version == meta.version
                );
                if stale {
                    missing.push((index, meta.version));
                }
            }
            missing
        };

        let built_profiles = missing_profiles
            .into_iter()
            .map(|(index, version)| (index, version, provider.build_width_profile(index)))
            .collect::<Vec<_>>();

        // Step 2: populate missing slots with O(1) height estimates and collect working state.
        let (mut heights, mut content_heights, mut measured, mut layouts) = {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for (index, version, profile) in built_profiles {
                if index < guard.width_profiles.len() {
                    guard.width_profiles[index] = Some(CachedWidthProfile { version, profile });
                }
            }
            if guard.slots.len() > len {
                guard.slots.truncate(len);
            } else if guard.slots.len() < len {
                guard.slots.resize(len, None);
            }

            let mut heights = Vec::with_capacity(len);
            let mut content_heights = Vec::with_capacity(len);
            let mut measured = Vec::with_capacity(len);
            let mut layouts = Vec::with_capacity(len);

            for (index, meta) in metas.iter().enumerate() {
                let slot_valid = matches!(
                    guard.slots.get(index),
                    Some(Some(slot))
                        if slot.version == meta.version && slot.next_kind == meta.next_kind
                );
                let slot = if slot_valid {
                    guard.slots[index].as_ref()
                } else {
                    let content_h = guard
                        .width_profiles
                        .get(index)
                        .and_then(|entry| entry.as_ref())
                        .map_or(1, |cached| {
                            cached.profile.estimate_content_height(
                                request.context.width,
                                request.context.max_reading_width,
                            )
                        });
                    let visual_h =
                        content_h.saturating_add(usize::from(meta.has_trailing_blank_estimate));
                    Some(&*guard.slots[index].insert(CachedLayoutSlot {
                        version: meta.version,
                        next_kind: meta.next_kind,
                        visual_height: visual_h,
                        content_visual_height: content_h,
                        measured: false,
                        layout: None,
                    }))
                };
                if let Some(slot) = slot {
                    heights.push(slot.visual_height);
                    content_heights.push(slot.content_visual_height);
                    measured.push(slot.measured && !meta.volatile);
                    layouts.push(if meta.volatile {
                        None
                    } else {
                        slot.layout.clone()
                    });
                }
            }
            (heights, content_heights, measured, layouts)
        };

        let mut item_visual_starts = prefix_sum_starts(&heights);
        let mut user_prompts =
            build_user_prompts(&metas, &item_visual_starts, &heights, &content_heights);

        // Step 3: lazy viewport settling loop (measures [first_visible ..= last_visible + margin]).
        let mut newly_computed: Vec<(usize, Arc<CachedItemLayout>, bool)> = Vec::new();
        let mut computed_this_frame: HashSet<usize> = HashSet::new();
        let max_iters = len.min(64).saturating_add(2);

        for _ in 0..max_iters {
            let scroll = provider.compute_scroll(&item_visual_starts, &heights);
            let (first_visible, last_visible) = visible_item_bounds(
                &item_visual_starts,
                len,
                scroll,
                request.viewport_height.max(1),
            );
            let measure_end = last_visible
                .saturating_add(MEASURE_MARGIN_ITEMS)
                .min(len - 1);
            let pinned_user = provider.sticky_user_for_scroll(&user_prompts, scroll);

            let mut to_compute = Vec::new();
            for index in first_visible..=measure_end {
                if (!measured[index] || layouts[index].is_none())
                    && computed_this_frame.insert(index)
                {
                    to_compute.push(index);
                }
            }
            if let Some(user_idx) = pinned_user
                && user_idx < len
                && (!measured[user_idx] || layouts[user_idx].is_none())
                && computed_this_frame.insert(user_idx)
            {
                to_compute.push(user_idx);
            }
            for &extra in request.extra_required {
                if extra < len
                    && (!measured[extra] || layouts[extra].is_none())
                    && computed_this_frame.insert(extra)
                {
                    to_compute.push(extra);
                }
            }

            if to_compute.is_empty() {
                break;
            }

            let mut height_changed = false;
            for index in to_compute {
                let layout = Arc::new(provider.compute_item(index));
                let new_h = layout.visual_height();
                let new_ch = layout.content_visual_height;
                if new_h != heights[index] || new_ch != content_heights[index] {
                    height_changed = true;
                }
                heights[index] = new_h;
                content_heights[index] = new_ch;
                measured[index] = true;
                layouts[index] = Some(Arc::clone(&layout));
                newly_computed.push((index, layout, metas[index].volatile));
            }

            if height_changed {
                item_visual_starts = prefix_sum_starts(&heights);
                user_prompts =
                    build_user_prompts(&metas, &item_visual_starts, &heights, &content_heights);
            } else {
                break;
            }
        }

        // Step 4: bottom-anchored warm-up above the viewport so immediate scroll-up
        // after resume lands on exact heights without snapping.
        if request.is_follow_bottom && request.viewport_height > 0 {
            let scroll = provider.compute_scroll(&item_visual_starts, &heights);
            let (first_visible, _) =
                visible_item_bounds(&item_visual_starts, len, scroll, request.viewport_height);
            let warm_rows = RESUME_WARM_PAGES.saturating_mul(usize::from(request.viewport_height));
            let warm_top = scroll.saturating_sub(warm_rows);
            let warm_start = item_at_visual_in_starts(&item_visual_starts, len, warm_top)
                .unwrap_or(0)
                .min(first_visible);
            let mut warm_changed = false;
            for index in warm_start..first_visible {
                if (!measured[index] || layouts[index].is_none())
                    && computed_this_frame.insert(index)
                {
                    let layout = Arc::new(provider.compute_item(index));
                    let new_h = layout.visual_height();
                    let new_ch = layout.content_visual_height;
                    if new_h != heights[index] || new_ch != content_heights[index] {
                        warm_changed = true;
                    }
                    heights[index] = new_h;
                    content_heights[index] = new_ch;
                    measured[index] = true;
                    layouts[index] = Some(Arc::clone(&layout));
                    newly_computed.push((index, layout, metas[index].volatile));
                }
            }
            if warm_changed {
                item_visual_starts = prefix_sum_starts(&heights);
                user_prompts =
                    build_user_prompts(&metas, &item_visual_starts, &heights, &content_heights);
            }
        }

        // Step 5: commit newly materialized items and evict far off-screen render trees.
        let final_scroll = provider.compute_scroll(&item_visual_starts, &heights);
        let (first_visible, last_visible) = visible_item_bounds(
            &item_visual_starts,
            len,
            final_scroll,
            request.viewport_height.max(1),
        );
        let sticky_user_idx = provider.sticky_user_for_scroll(&user_prompts, final_scroll);
        let keep_start = first_visible.saturating_sub(EVICT_KEEP_MARGIN_ITEMS);
        let keep_end = last_visible
            .saturating_add(MEASURE_MARGIN_ITEMS)
            .saturating_add(EVICT_KEEP_MARGIN_ITEMS)
            .min(len - 1);

        {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.context.as_ref() == Some(&request.context) {
                for (index, layout, volatile) in newly_computed {
                    guard.misses = guard.misses.saturating_add(1);
                    if let Some(Some(slot)) = guard.slots.get_mut(index) {
                        slot.visual_height = layout.visual_height();
                        slot.content_visual_height = layout.content_visual_height;
                        slot.measured = !volatile;
                        slot.layout = (!volatile).then_some(layout);
                    }
                }

                let materialized_count = guard
                    .slots
                    .iter()
                    .filter(|slot| slot.as_ref().is_some_and(|s| s.layout.is_some()))
                    .count();
                let keep_budget = keep_end
                    .saturating_sub(keep_start)
                    .saturating_add(1)
                    .saturating_add(EVICT_SLACK_ITEMS);
                if materialized_count > keep_budget {
                    for (index, slot_opt) in guard.slots.iter_mut().enumerate() {
                        if index >= keep_start && index <= keep_end {
                            continue;
                        }
                        if Some(index) == sticky_user_idx || request.extra_required.contains(&index)
                        {
                            continue;
                        }
                        if let Some(slot) = slot_opt
                            && slot.layout.is_some()
                        {
                            // Keep exact visual_height / measured = true; drop heavyweight spans.
                            slot.layout = None;
                        }
                    }
                }
            }
        }

        TimelineLayoutSnapshot {
            items: layouts,
            item_heights: heights,
            item_content_heights: content_heights,
            item_visual_starts,
            user_prompts,
        }
    }

    /// Computes or fetches one item layout on demand (used when an active mouse
    /// drag selection autoscrolls into previously evicted cold items).
    pub(crate) fn materialize_item(
        &self,
        context: &TimelineCacheContext,
        index: usize,
        meta: TimelineItemMeta,
        compute_item: impl FnOnce(usize) -> CachedItemLayout,
    ) -> Arc<CachedItemLayout> {
        {
            let guard = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.context.as_ref() == Some(context)
                && !meta.volatile
                && let Some(Some(slot)) = guard.slots.get(index)
                && slot.version == meta.version
                && slot.next_kind == meta.next_kind
                && let Some(layout) = &slot.layout
            {
                return Arc::clone(layout);
            }
        }

        let layout = Arc::new(compute_item(index));
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.context.as_ref() == Some(context) {
            guard.misses = guard.misses.saturating_add(1);
            if let Some(Some(slot)) = guard.slots.get_mut(index)
                && slot.version == meta.version
                && slot.next_kind == meta.next_kind
            {
                slot.visual_height = layout.visual_height();
                slot.content_visual_height = layout.content_visual_height;
                slot.measured = !meta.volatile;
                slot.layout = (!meta.volatile).then(|| Arc::clone(&layout));
            }
        }
        layout
    }

    /// Shifts cached slots and width profiles right when older archived items
    /// are prepended to the start of the timeline, keeping already-measured
    /// items warm at their new indices without rebuilding them.
    pub(crate) fn prepend_unmeasured(&self, count: usize) {
        if count == 0 {
            return;
        }
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.slots.is_empty() {
            for slot in guard.slots.iter_mut().flatten() {
                if let Some(layout) = slot.layout.as_mut()
                    && layout.copy_targets.iter().any(|target| {
                        matches!(
                            target.content,
                            crate::tui::copy_controls::CopyContent::AssistantMessage(_)
                        )
                    })
                {
                    let layout_mut = Arc::make_mut(layout);
                    for target in &mut layout_mut.copy_targets {
                        if let crate::tui::copy_controls::CopyContent::AssistantMessage(index) =
                            &mut target.content
                        {
                            *index = index.saturating_add(count);
                        }
                    }
                }
            }
            let mut next = vec![None; count];
            next.append(&mut guard.slots);
            guard.slots = next;
        }
        if !guard.width_profiles.is_empty() {
            let mut next = vec![None; count];
            next.append(&mut guard.width_profiles);
            guard.width_profiles = next;
        }
    }

    #[cfg(test)]
    pub(crate) fn misses(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .misses
    }

    #[cfg(test)]
    pub(crate) fn materialized_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .slots
            .iter()
            .filter(|slot| slot.as_ref().is_some_and(|s| s.layout.is_some()))
            .count()
    }

    #[cfg(test)]
    pub(crate) fn width_profile_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .width_profiles
            .iter()
            .filter(|entry| entry.is_some())
            .count()
    }
}

fn build_user_prompts(
    metas: &[TimelineItemMeta],
    item_visual_starts: &[usize],
    heights: &[usize],
    content_heights: &[usize],
) -> Vec<UserPromptDescriptor> {
    let mut prompts = Vec::new();
    for (index, meta) in metas.iter().enumerate() {
        if !meta.is_user {
            continue;
        }
        let content_height = content_heights[index];
        if content_height == 0 {
            continue;
        }
        let y = item_visual_starts[index];
        let next_item_y = item_visual_starts
            .get(index + 1)
            .copied()
            .unwrap_or_else(|| y.saturating_add(heights[index]));
        prompts.push(UserPromptDescriptor {
            item_index: index,
            y,
            next_item_y,
            content_height,
        });
    }
    prompts
}

fn visible_item_bounds(
    starts: &[usize],
    len: usize,
    scroll: usize,
    viewport_height: u16,
) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    let total = starts.last().copied().unwrap_or(0);
    if total == 0 {
        return (0, len - 1);
    }
    let clamped_scroll = scroll.min(total.saturating_sub(1));
    let first = item_at_visual_in_starts(starts, len, clamped_scroll).unwrap_or(0);
    let last_row = clamped_scroll
        .saturating_add(usize::from(viewport_height).saturating_sub(1))
        .min(total.saturating_sub(1));
    let last = item_at_visual_in_starts(starts, len, last_row)
        .unwrap_or(len - 1)
        .max(first);
    (first, last)
}
