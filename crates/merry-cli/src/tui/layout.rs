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
    cockpit_layout(area, bottom)
}

pub(crate) fn cockpit_layout(area: Rect, bottom: BottomPaneHeights) -> TimelineRects {
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

    TimelineRects {
        mode,
        header: rows[0],
        timeline: rows[1],
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
    fn timeline_always_keeps_full_width() {
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
}
