//! Pins the active user turn to the top of the timeline.
//!
//! Once a user message scrolls above the viewport it stays there, up to three
//! rows plus a gap, until the next user message reaches that band. The body
//! then starts below the header so the bottom line of a followed transcript
//! stays put.

use super::timeline_cache::TimelineLayoutSnapshot;
use crate::tui::{
    state::{TimelineItem, TuiState},
    transcript::TranscriptRow,
};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};

const MAX_STICKY_USER_LINES: usize = 5;

fn max_sticky_rows(viewport_height: u16) -> usize {
    if viewport_height >= 12 {
        MAX_STICKY_USER_LINES
    } else if viewport_height >= 8 {
        3
    } else {
        2
    }
}

pub(super) struct TimelineScrollSplit {
    /// Header rows, including the blank gap under a pinned message.
    pub(super) header_rows: u16,
    /// Wrapped rows of the pinned message. The gap is the remainder of `header_rows`.
    pub(super) content_rows: u16,
    /// Document row of the first pinned line. Absent when nothing is pinned.
    pub(super) pinned_origin: Option<usize>,
    pub(super) body_scroll: usize,
    pub(super) lines: Vec<Line<'static>>,
}

pub(super) fn timeline_scroll_split(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    viewport_height: u16,
    scroll_start: usize,
) -> TimelineScrollSplit {
    let Some(pin) = pin_user(state, timeline, viewport_height, scroll_start) else {
        return TimelineScrollSplit {
            header_rows: 0,
            content_rows: 0,
            pinned_origin: None,
            body_scroll: scroll_start,
            lines: Vec::new(),
        };
    };
    TimelineScrollSplit {
        header_rows: pin.header_rows,
        content_rows: pin.content_rows,
        pinned_origin: Some(pin.origin),
        body_scroll: scroll_start.saturating_add(usize::from(pin.header_rows)),
        lines: pin.lines,
    }
}

pub(super) fn split_timeline_content(content: Rect, header_rows: u16) -> (Rect, Rect) {
    let header_rows = header_rows.min(content.height);
    let header = Rect {
        x: content.x,
        y: content.y,
        width: content.width,
        height: header_rows,
    };
    let body = Rect {
        x: content.x,
        y: content.y.saturating_add(header_rows),
        width: content.width,
        height: content.height.saturating_sub(header_rows),
    };
    (header, body)
}

pub(super) fn paint_sticky_header(
    frame: &mut Frame<'_>,
    header: Rect,
    split: &TimelineScrollSplit,
) {
    let height = split.content_rows.min(header.height);
    if height == 0 || header.width == 0 || split.lines.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(split.lines.clone()).wrap(Wrap { trim: false }),
        Rect {
            x: header.x,
            y: header.y,
            width: header.width,
            height,
        },
    );
}

struct PinnedUser {
    header_rows: u16,
    content_rows: u16,
    origin: usize,
    lines: Vec<Line<'static>>,
}

struct UserSection {
    y: usize,
    height: usize,
    lines: Vec<Line<'static>>,
}

fn pin_user(
    state: &TuiState,
    timeline: &TimelineLayoutSnapshot,
    viewport_height: u16,
    scroll_start: usize,
) -> Option<PinnedUser> {
    if viewport_height <= 1 {
        return None;
    }
    let sections = user_sections(state, timeline);
    let position = sections
        .iter()
        .rposition(|section| section.y < scroll_start)?;
    let section = sections.get(position)?;
    let budget = usize::from(viewport_height).saturating_sub(2);
    if budget == 0 || section.height == 0 {
        return None;
    }
    let mut show = section
        .height
        .min(max_sticky_rows(viewport_height))
        .min(budget);
    let mut gap = usize::from(show < budget);
    if let Some(next) = sections.get(position + 1) {
        let next_row = next.y.saturating_sub(scroll_start);
        if next_row <= show + gap {
            if next_row <= 1 {
                return None;
            }
            gap = 1;
            show = next_row.saturating_sub(gap);
            if show == 0 {
                return None;
            }
        }
    }
    let header_rows = u16::try_from(show + gap).unwrap_or(u16::MAX);
    let content_rows = u16::try_from(show).unwrap_or(header_rows);
    let mut lines = section.lines.clone();
    // Preserve the symmetric bottom hairline when a long multi-line user prompt
    // is clipped to fit the sticky header budget.
    if section.height > show
        && show >= 3
        && let Some(bottom_rule) = section.lines.last().cloned()
    {
        lines.truncate(show - 1);
        lines.push(bottom_rule);
    }
    Some(PinnedUser {
        header_rows,
        content_rows,
        origin: section.y,
        lines,
    })
}

fn user_sections(state: &TuiState, timeline: &TimelineLayoutSnapshot) -> Vec<UserSection> {
    let mut sections = Vec::new();
    for (index, item) in state.timeline().iter().enumerate() {
        if !matches!(item, TimelineItem::User { .. }) {
            continue;
        }
        let Some(layout) = timeline.items.get(index) else {
            continue;
        };
        let Some(&y) = timeline.item_visual_starts.get(index) else {
            continue;
        };
        let mut content_end = layout.rows.len();
        while content_end > 0 && layout.rows.get(content_end - 1).is_some_and(row_is_blank) {
            content_end -= 1;
        }
        if content_end == 0 {
            continue;
        }
        let mut lines = Vec::new();
        let mut height = 0usize;
        for (row_idx, row) in layout.rows[..content_end].iter().enumerate() {
            let row_start = layout.local_row_starts.get(row_idx).copied().unwrap_or(0);
            lines.push(row.display.clone());
            let row_end = layout
                .local_row_starts
                .get(row_idx + 1)
                .copied()
                .unwrap_or(row_start.saturating_add(1));
            height = height.saturating_add(row_end.saturating_sub(row_start));
        }
        if height == 0 || lines.is_empty() {
            continue;
        }
        sections.push(UserSection { y, height, lines });
    }
    sections
}

fn row_is_blank(row: &TranscriptRow) -> bool {
    row.display.spans.iter().all(|span| span.content.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{
        keymap::Keymap,
        render::timeline_cache::CachedItemLayout,
        state::{TimelineItem, TuiState},
        theme::TuiTheme,
        transcript::{SelectionPolicy, TranscriptRow},
    };
    use ratatui::text::Line;
    use std::sync::Arc;

    fn state_with_users(count: usize) -> TuiState {
        let mut state = TuiState::new(
            "/repo".into(),
            "test".to_owned(),
            Keymap::default(),
            TuiTheme::default(),
        );
        for index in 0..count {
            state.push_timeline_item(TimelineItem::User {
                text: format!("user-{index}"),
            });
        }
        state
    }

    fn item_layout(rows: Vec<TranscriptRow>) -> Arc<CachedItemLayout> {
        let local_row_starts = (0..=rows.len()).collect();
        Arc::new(CachedItemLayout::new(rows, local_row_starts, Vec::new()))
    }

    fn line(text: &str) -> TranscriptRow {
        TranscriptRow::new(Line::from(text.to_owned()), SelectionPolicy::Keep)
    }

    fn blank() -> TranscriptRow {
        TranscriptRow::new(Line::from(""), SelectionPolicy::Keep)
    }

    #[test]
    fn no_pin_when_the_user_message_is_still_inside_the_viewport() {
        let state = state_with_users(1);
        let timeline = TimelineLayoutSnapshot::new(vec![
            item_layout(vec![line("user-0"), blank()]),
            item_layout(vec![line("answer")]),
        ]);
        let split = timeline_scroll_split(&state, &timeline, 10, 0);
        assert_eq!(split.header_rows, 0);
        assert_eq!(split.body_scroll, 0);
        assert_eq!(split.pinned_origin, None);
        assert!(split.lines.is_empty());
    }

    #[test]
    fn pins_the_latest_user_message_above_the_viewport_and_keeps_a_gap() {
        let state = state_with_users(2);
        let timeline = TimelineLayoutSnapshot::new(vec![
            item_layout(vec![line("user-0"), blank()]),
            item_layout(vec![line("user-1"), blank()]),
            item_layout(vec![line("tail")]),
        ]);
        let split = timeline_scroll_split(&state, &timeline, 10, 4);
        assert_eq!(split.content_rows, 1);
        assert_eq!(split.header_rows, 2);
        assert_eq!(split.pinned_origin, Some(2));
        assert_eq!(split.body_scroll, 6);
        assert_eq!(split.lines.len(), 1);
    }

    #[test]
    fn the_next_user_message_releases_the_pin_when_it_reaches_the_top() {
        let state = state_with_users(2);
        let timeline = TimelineLayoutSnapshot::new(vec![
            item_layout(vec![line("user-0"), blank()]),
            item_layout(vec![line("user-1")]),
        ]);
        let split = timeline_scroll_split(&state, &timeline, 10, 2);
        assert_eq!(split.header_rows, 0);
        assert_eq!(split.body_scroll, 2);
    }
}
