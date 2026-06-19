use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CockpitLayoutMode {
    Wide,
    Medium,
    Narrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BottomPaneHeights {
    pub(crate) queue: u16,
    pub(crate) completion: u16,
    pub(crate) interaction: u16,
    pub(crate) input: u16,
    pub(crate) status: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CockpitRects {
    pub(crate) mode: CockpitLayoutMode,
    pub(crate) chat: Rect,
    pub(crate) focus: Option<Rect>,
    pub(crate) plan: Option<Rect>,
    pub(crate) queue: Option<Rect>,
    pub(crate) completion: Rect,
    pub(crate) interaction: Rect,
    pub(crate) input: Rect,
    pub(crate) status: Rect,
}

pub(crate) fn layout_mode(width: u16) -> CockpitLayoutMode {
    match width {
        width if width >= 170 => CockpitLayoutMode::Wide,
        width if width >= 120 => CockpitLayoutMode::Medium,
        _ => CockpitLayoutMode::Narrow,
    }
}

pub(crate) fn cockpit_layout(area: Rect, bottom: BottomPaneHeights) -> CockpitRects {
    let mode = layout_mode(area.width);
    let bottom_queue_height = if mode.uses_bottom_queue() {
        bottom.queue
    } else {
        0
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(bottom_queue_height),
            Constraint::Length(bottom.completion),
            Constraint::Length(bottom.interaction),
            Constraint::Length(bottom.input),
            Constraint::Length(bottom.status),
        ])
        .split(area);

    let content = rows[0];
    let queue = (bottom_queue_height > 0).then_some(rows[1]);
    let completion = rows[2];
    let interaction = rows[3];
    let input = rows[4];
    let status = rows[5];

    let (chat, focus, plan) = match mode {
        CockpitLayoutMode::Wide => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(40),
                    Constraint::Percentage(38),
                    Constraint::Percentage(22),
                ])
                .split(content);
            (columns[0], Some(columns[1]), Some(columns[2]))
        }
        CockpitLayoutMode::Medium => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(content);
            let rail = split_medium_rail(columns[1]);
            (columns[0], Some(rail.0), Some(rail.1))
        }
        CockpitLayoutMode::Narrow => (content, None, None),
    };

    CockpitRects {
        mode,
        chat,
        focus,
        plan,
        queue,
        completion,
        interaction,
        input,
        status,
    }
}

impl CockpitLayoutMode {
    pub(crate) fn uses_bottom_queue(self) -> bool {
        matches!(self, Self::Narrow)
    }
}

fn split_medium_rail(region: Rect) -> (Rect, Rect) {
    let plan_height = medium_plan_height(region.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(plan_height)])
        .split(region);
    (rows[0], rows[1])
}

fn medium_plan_height(height: u16) -> u16 {
    if height <= 8 {
        return height / 2;
    }
    let third = height / 3;
    third.clamp(6, 14).min(height.saturating_sub(3))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bottom() -> BottomPaneHeights {
        BottomPaneHeights {
            queue: 5,
            completion: 0,
            interaction: 1,
            input: 3,
            status: 1,
        }
    }

    #[test]
    fn mode_thresholds_match_spec() {
        assert_eq!(layout_mode(180), CockpitLayoutMode::Wide);
        assert_eq!(layout_mode(170), CockpitLayoutMode::Wide);
        assert_eq!(layout_mode(169), CockpitLayoutMode::Medium);
        assert_eq!(layout_mode(120), CockpitLayoutMode::Medium);
        assert_eq!(layout_mode(119), CockpitLayoutMode::Narrow);
    }

    #[test]
    fn wide_layout_has_three_columns_and_no_bottom_queue() {
        let rects = cockpit_layout(Rect::new(0, 0, 180, 40), bottom());

        assert_eq!(rects.mode, CockpitLayoutMode::Wide);
        assert!(rects.focus.is_some());
        assert!(rects.plan.is_some());
        assert!(rects.queue.is_none());
        assert_eq!(rects.chat.y, 0);
        assert_eq!(rects.chat.height, rects.focus.unwrap().height);
        assert_eq!(rects.chat.height, rects.plan.unwrap().height);
        assert!(rects.chat.width >= 48);
        assert!(rects.focus.unwrap().width >= 50);
        assert!(rects.plan.unwrap().width >= 28);
        assert!(rects.chat.x < rects.focus.unwrap().x);
        assert!(rects.focus.unwrap().x < rects.plan.unwrap().x);
        assert!(rects.interaction.y < rects.input.y);
        assert!(rects.input.y < rects.status.y);
    }

    #[test]
    fn medium_layout_has_chat_and_stacked_work_rail() {
        let rects = cockpit_layout(Rect::new(0, 0, 140, 36), bottom());
        let focus = rects.focus.expect("medium should render focus");
        let plan = rects.plan.expect("medium should render plan");

        assert_eq!(rects.mode, CockpitLayoutMode::Medium);
        assert!(rects.queue.is_none());
        assert_eq!(focus.x, plan.x);
        assert_eq!(focus.width, plan.width);
        assert!(focus.y < plan.y);
        assert_eq!(rects.chat.y, focus.y);
        assert_eq!(rects.chat.height, focus.height + plan.height);
        assert!(rects.chat.width > focus.width);
    }

    #[test]
    fn narrow_layout_keeps_single_chat_column_and_bottom_queue() {
        let rects = cockpit_layout(Rect::new(0, 0, 100, 32), bottom());

        assert_eq!(rects.mode, CockpitLayoutMode::Narrow);
        assert!(rects.focus.is_none());
        assert!(rects.plan.is_none());
        assert!(rects.queue.is_some());
        assert_eq!(rects.chat.x, 0);
        assert_eq!(rects.chat.width, 100);
        assert!(rects.chat.y < rects.queue.unwrap().y);
        assert!(rects.queue.unwrap().y < rects.input.y);
    }

    #[test]
    fn tiny_height_preserves_input_and_status_regions() {
        let rects = cockpit_layout(Rect::new(0, 0, 100, 8), bottom());

        assert_eq!(rects.mode, CockpitLayoutMode::Narrow);
        assert!(rects.chat.height >= 1);
        assert!(rects.input.height >= 1);
        assert_eq!(rects.status.height, 1);
        assert!(rects.input.y < rects.status.y);
    }
}
