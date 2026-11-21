use super::TuiState;
use std::time::{Duration, Instant};

const TURN_SCROLL_ANIMATION_DURATION: Duration = Duration::from_millis(220);

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

/// Short ease-out glide used when a newly sent user turn anchors to the top of
/// the timeline, avoiding an abrupt full-screen jump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineScrollAnimation {
    from_row: usize,
    to_row: usize,
    started_at: Instant,
    duration: Duration,
}

impl TimelineScrollAnimation {
    pub(crate) fn new(from_row: usize, to_row: usize, started_at: Instant) -> Self {
        Self {
            from_row,
            to_row,
            started_at,
            duration: TURN_SCROLL_ANIMATION_DURATION,
        }
    }

    pub(crate) fn row_at(&self, elapsed: Duration) -> usize {
        if elapsed >= self.duration || self.duration.is_zero() || self.from_row == self.to_row {
            return self.to_row;
        }
        let progress = (elapsed.as_secs_f64() / self.duration.as_secs_f64()).clamp(0.0, 1.0);
        // Ease-out cubic: starts brisk so input feels responsive, then settles softly.
        let eased = 1.0 - (1.0 - progress).powi(3);
        let delta = self.to_row as f64 - self.from_row as f64;
        (self.from_row as f64 + delta * eased).round().max(0.0) as usize
    }

    pub(crate) fn current_row(&self, now: Instant) -> usize {
        self.row_at(now.saturating_duration_since(self.started_at))
    }

    pub(crate) fn is_finished(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= self.duration
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

    pub(crate) fn record_timeline_last_scroll(&mut self, scroll: usize) {
        self.timeline_last_scroll = Some(scroll);
    }

    pub(crate) fn timeline_scroll_animation_row(&self) -> Option<usize> {
        if !self.animations {
            return None;
        }
        let anim = self.timeline_scroll_anim?;
        let now = Instant::now();
        (!anim.is_finished(now)).then(|| anim.current_row(now))
    }

    pub(crate) fn has_active_timeline_scroll_animation(&self) -> bool {
        if !self.animations {
            return false;
        }
        self.pending_turn_scroll_animation
            || self
                .timeline_scroll_anim
                .is_some_and(|anim| !anim.is_finished(Instant::now()))
    }

    /// Advances or starts the turn-anchoring scroll glide while following the tail.
    pub(crate) fn sync_followed_timeline_scroll(&mut self, target_scroll: usize) {
        let now = Instant::now();
        let current_visual = self
            .timeline_scroll_anim
            .filter(|anim| !anim.is_finished(now))
            .map(|anim| anim.current_row(now))
            .or(self.timeline_last_scroll);

        if self.pending_turn_scroll_animation {
            self.pending_turn_scroll_animation = false;
            if self.animations
                && let Some(from_row) = current_visual
                && target_scroll > from_row.saturating_add(1)
            {
                self.timeline_scroll_anim =
                    Some(TimelineScrollAnimation::new(from_row, target_scroll, now));
                self.timeline_last_scroll = Some(from_row);
                return;
            }
        }

        if let Some(anim) = &mut self.timeline_scroll_anim {
            if anim.is_finished(now) || !self.animations {
                self.timeline_scroll_anim = None;
                self.timeline_last_scroll = Some(target_scroll);
            } else {
                // If streaming output grew past the anchored turn during the glide,
                // keep the target synchronized so the glide never undershoots.
                anim.to_row = target_scroll;
                self.timeline_last_scroll = Some(anim.current_row(now));
            }
        } else {
            self.timeline_last_scroll = Some(target_scroll);
        }
    }

    pub(super) fn clear_timeline_scroll_animation(&mut self) {
        self.timeline_scroll_anim = None;
        self.pending_turn_scroll_animation = false;
    }

    pub(super) fn note_timeline_update(&mut self) {
        if self.is_timeline_detached() {
            self.timeline_has_updates = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_scroll_animation_glides_monotonically_with_ease_out() {
        let anim = TimelineScrollAnimation::new(10, 40, Instant::now());
        let r0 = anim.row_at(Duration::ZERO);
        let r1 = anim.row_at(Duration::from_millis(55));
        let r2 = anim.row_at(Duration::from_millis(110));
        let r3 = anim.row_at(Duration::from_millis(165));
        let r4 = anim.row_at(TURN_SCROLL_ANIMATION_DURATION);

        assert_eq!(r0, 10);
        assert!(r0 < r1 && r1 < r2 && r2 < r3 && r3 <= r4);
        assert_eq!(r4, 40);
        // Ease-out covers more than half the distance in the first half of the duration.
        assert!(r2 > 25, "expected ease-out midpoint > 25, got {r2}");
    }
}
