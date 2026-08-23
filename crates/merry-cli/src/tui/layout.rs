use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineLayoutMode {
    Narrow,
    Standard,
    Wide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BottomPaneHeights {
    pub(crate) queue: u16,
    pub(crate) completion: u16,
    pub(crate) input: u16,
    pub(crate) status: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineRects {
    pub(crate) mode: TimelineLayoutMode,
    pub(crate) header: Rect,
    pub(crate) timeline: Rect,
    pub(crate) plan: Option<Rect>,
    pub(crate) queue: Option<Rect>,
    pub(crate) completion: Rect,
    pub(crate) input: Rect,
    pub(crate) status: Rect,
}

pub(crate) fn layout_mode(width: u16) -> TimelineLayoutMode {
    match width {
        width if width >= 120 => TimelineLayoutMode::Wide,
        width if width >= 80 => TimelineLayoutMode::Standard,
        _ => TimelineLayoutMode::Narrow,
    }
}

#[cfg(test)]
pub(crate) fn timeline_layout(area: Rect, bottom: BottomPaneHeights) -> TimelineRects {
    cockpit_layout(area, bottom, false, false)
}

pub(crate) fn cockpit_layout(
    area: Rect,
    bottom: BottomPaneHeights,
    plan_open: bool,
    plan_focused: bool,
) -> TimelineRects {
    let mode = layout_mode(area.width);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(bottom.queue),
            Constraint::Length(bottom.completion),
            Constraint::Length(bottom.input),
            Constraint::Length(bottom.status),
        ])
        .split(area);

    let (timeline, plan) = match (mode, plan_open, plan_focused) {
        (TimelineLayoutMode::Narrow, true, true) => (Rect::default(), Some(rows[1])),
        (TimelineLayoutMode::Wide, true, _) => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
                .split(rows[1]);
            (columns[0], Some(columns[1]))
        }
        (TimelineLayoutMode::Standard, true, _) => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(rows[1]);
            (columns[0], Some(columns[1]))
        }
        (_, _, _) => (rows[1], None),
    };

    TimelineRects {
        mode,
        header: rows[0],
        timeline,
        plan,
        queue: (bottom.queue > 0).then_some(rows[2]),
        completion: rows[3],
        input: rows[4],
        status: rows[5],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bottom() -> BottomPaneHeights {
        BottomPaneHeights {
            queue: 0,
            completion: 0,
            input: 3,
            status: 1,
        }
    }

    #[test]
    fn mode_thresholds_are_stable() {
        assert_eq!(layout_mode(50), TimelineLayoutMode::Narrow);
        assert_eq!(layout_mode(80), TimelineLayoutMode::Standard);
        assert_eq!(layout_mode(119), TimelineLayoutMode::Standard);
        assert_eq!(layout_mode(120), TimelineLayoutMode::Wide);
    }

    #[test]
    fn timeline_keeps_full_width_without_plan() {
        for area in [
            Rect::new(0, 0, 50, 20),
            Rect::new(0, 0, 80, 24),
            Rect::new(0, 0, 140, 40),
        ] {
            let rects = timeline_layout(area, bottom());
            assert_eq!(rects.timeline.width, area.width);
            assert_eq!(rects.header.height, 1);
            assert_eq!(rects.input.height, 3);
            assert_eq!(rects.status.height, 1);
        }
    }

    #[test]
    fn queue_gets_no_rectangle_when_empty() {
        let rects = timeline_layout(Rect::new(0, 0, 80, 24), bottom());
        assert!(rects.queue.is_none());
    }

    #[test]
    fn plan_layout_is_responsive_without_overlapping_bottom_panes() {
        let wide = cockpit_layout(Rect::new(0, 0, 140, 40), bottom(), true, false);
        let wide_plan = wide.plan.expect("wide plan pane");
        assert!(wide.timeline.width > 0);
        assert_eq!(wide.timeline.right(), wide_plan.x);
        assert_eq!(wide_plan.right(), 140);
        assert_eq!(wide_plan.bottom(), wide.input.y);

        let standard = cockpit_layout(Rect::new(0, 0, 80, 24), bottom(), true, false);
        let standard_plan = standard.plan.expect("standard plan pane");
        assert!(standard.timeline.width >= 44);
        assert_eq!(standard.timeline.right(), standard_plan.x);
        assert_eq!(standard_plan.right(), 80);

        let narrow = cockpit_layout(Rect::new(0, 0, 50, 20), bottom(), true, true);
        assert_eq!(narrow.timeline, Rect::default());
        assert_eq!(narrow.plan, Some(Rect::new(0, 1, 50, 15)));
        assert_eq!(narrow.input, Rect::new(0, 16, 50, 3));
        assert_eq!(narrow.status, Rect::new(0, 19, 50, 1));
    }

    #[test]
    fn narrow_plan_does_not_cover_timeline_until_focused() {
        let rects = cockpit_layout(Rect::new(0, 0, 50, 20), bottom(), true, false);

        assert_eq!(rects.timeline, Rect::new(0, 1, 50, 15));
        assert!(rects.plan.is_none());
    }
}
