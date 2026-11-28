use super::TuiState;
use std::time::{Duration, Instant};

const TURN_SCROLL_ANIMATION_DURATION: Duration = Duration::from_millis(220);
/// Idle duration while a run is active before detached timeline review
/// automatically returns to following the latest output.
pub(crate) const TIMELINE_REVIEW_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

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
            || self.timeline_review_target.is_some()
    }

    /// Exits detached timeline review when active.
    /// Preserves `timeline_last_scroll` (and turn-scroll animations) when already following.
    pub(crate) fn follow_latest_if_detached(&mut self) {
        if self.is_timeline_detached() {
            self.follow_latest();
        }
    }

    /// Records recent user interaction to extend detached timeline review while a run is active.
    pub(crate) fn note_timeline_review_activity(&mut self) {
        if self.is_timeline_detached() {
            self.timeline_review_last_activity = Some(tokio::time::Instant::now());
        }
    }

    /// Returns to following the latest output once detached timeline review has
    /// remained idle for [`TIMELINE_REVIEW_IDLE_TIMEOUT`] during an active run.
    pub(crate) fn expire_idle_timeline_review(&mut self) -> bool {
        if !self.is_timeline_detached() {
            self.timeline_review_last_activity = None;
            return false;
        }
        if !self.is_active_run() {
            return false;
        }
        let now = tokio::time::Instant::now();
        if self.is_timeline_review_auto_follow_paused() {
            // Keep the deadline fresh while a modal, comment session, or text
            // selection holds focus so closing it never snaps the viewport away
            // immediately.
            self.timeline_review_last_activity = Some(now);
            return false;
        }
        let last_activity = *self.timeline_review_last_activity.get_or_insert(now);
        if now.saturating_duration_since(last_activity) >= TIMELINE_REVIEW_IDLE_TIMEOUT {
            self.follow_latest();
            return true;
        }
        false
    }

    fn is_timeline_review_auto_follow_paused(&self) -> bool {
        self.overlay().is_some() || self.text_selection().is_some() || self.is_commenting()
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

    fn detached_state() -> TuiState {
        let mut state = TuiState::new(
            "/repo".into(),
            "gpt-test".into(),
            crate::tui::keymap::Keymap::default(),
            crate::tui::theme::TuiTheme::default(),
        );
        state.push_timeline_item(crate::tui::state::TimelineItem::User {
            text: "question".into(),
        });
        state.push_timeline_item(crate::tui::state::TimelineItem::Assistant {
            text: "answer".into(),
        });
        state.scroll_timeline_up_by(10);
        state
    }

    #[tokio::test(start_paused = true)]
    async fn idle_review_auto_follows_after_timeout_only_while_run_is_active() {
        let mut state = detached_state();

        // Idle sessions remain detached indefinitely.
        tokio::time::advance(Duration::from_secs(600)).await;
        assert!(!state.expire_idle_timeline_review());
        assert!(state.is_timeline_detached());

        // Starting an active run grants a fresh timeout window from run start.
        state.set_run_state(merry::events::InteractiveRunState::RunningModel);
        tokio::time::advance(TIMELINE_REVIEW_IDLE_TIMEOUT - Duration::from_millis(1)).await;
        assert!(!state.expire_idle_timeline_review());
        assert!(state.is_timeline_detached());

        // User activity extends the timeout window.
        state.note_timeline_review_activity();
        tokio::time::advance(TIMELINE_REVIEW_IDLE_TIMEOUT - Duration::from_millis(1)).await;
        assert!(!state.expire_idle_timeline_review());
        assert!(state.is_timeline_detached());

        // Reaching the timeout without activity exits detached review and follows latest.
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(state.expire_idle_timeline_review());
        assert!(!state.is_timeline_detached());
        assert_eq!(state.timeline_scroll_offset(), 0);
        assert_eq!(state.timeline_review_user_index(), None);
        assert_eq!(state.timeline_anchor(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_review_auto_follow_pauses_during_overlays_and_comment_mode() {
        let mut state = detached_state();
        state.set_run_state(merry::events::InteractiveRunState::RunningTool);

        // Opening an overlay pauses auto-follow and refreshes the deadline.
        state.open_command_palette();
        tokio::time::advance(TIMELINE_REVIEW_IDLE_TIMEOUT + Duration::from_secs(15)).await;
        assert!(!state.expire_idle_timeline_review());
        assert!(state.is_timeline_detached());

        // Closing the overlay grants a fresh window before expiring.
        state.close_overlay();
        tokio::time::advance(TIMELINE_REVIEW_IDLE_TIMEOUT - Duration::from_millis(1)).await;
        assert!(!state.expire_idle_timeline_review());
        assert!(state.is_timeline_detached());

        // Entering comment mode also pauses auto-follow until exited.
        state.toggle_comment_mode();
        tokio::time::advance(TIMELINE_REVIEW_IDLE_TIMEOUT + Duration::from_secs(10)).await;
        assert!(!state.expire_idle_timeline_review());
        assert!(state.is_timeline_detached());

        state.toggle_comment_mode();
        tokio::time::advance(TIMELINE_REVIEW_IDLE_TIMEOUT).await;
        assert!(state.expire_idle_timeline_review());
        assert!(!state.is_timeline_detached());
    }
}
