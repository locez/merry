use super::super::{
    ControllerEffect, TUI_REFRESH_INTERVAL, cockpit_rects, handle_key_event, handle_mouse_input,
    handle_mouse_scroll_up, needs_refresh, new_refresh_interval, refresh_interactions,
};
use crate::tui::{
    keymap::Keymap,
    render::{prepare_viewport, render, timeline_content_region},
    state::{TimelineItem, TuiState},
    text_interaction::MouseInput,
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect, Size},
    style::Modifier,
};
use tokio::time;

fn label(index: usize) -> String {
    format!("row-{index:05}")
}

fn transcript(rows: usize) -> String {
    (0..rows).map(label).collect::<Vec<_>>().join("\n")
}

fn state(markdown: &str, start_at_top: bool) -> TuiState {
    let mut state = TuiState::new(
        "/repo".into(),
        "test".into(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: markdown.into(),
    });
    if start_at_top {
        state.scroll_timeline_up_by(usize::MAX);
    }
    state
}

fn draw(state: &mut TuiState, size: Size) -> Buffer {
    prepare_viewport(state, size);
    let backend = ratatui::backend::TestBackend::new(size.width, size.height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render(frame, state))
        .unwrap()
        .buffer
        .clone()
}

fn area(state: &TuiState, size: Size) -> Rect {
    timeline_content_region(cockpit_rects(size, state).timeline)
}

fn row_text(buffer: &Buffer, area: Rect, row: u16) -> String {
    (area.x..area.right())
        .map(|column| buffer[(column, row)].symbol())
        .collect()
}

fn visible_text(buffer: &Buffer, area: Rect) -> String {
    (area.y..area.bottom())
        .map(|row| row_text(buffer, area, row))
        .collect::<Vec<_>>()
        .join("\n")
}

fn point(buffer: &Buffer, text: &str) -> Position {
    for row in buffer.area.y..buffer.area.bottom() {
        for column in buffer.area.x..buffer.area.right() {
            if text.chars().enumerate().all(|(offset, character)| {
                let column = column.saturating_add(u16::try_from(offset).unwrap());
                column < buffer.area.right()
                    && buffer[(column, row)].symbol() == character.to_string()
            }) {
                return Position::new(column, row);
            }
        }
    }
    panic!("missing {text} in {}", visible_text(buffer, buffer.area));
}

fn row_index(buffer: &Buffer, area: Rect, row: u16) -> usize {
    row_text(buffer, area, row)
        .trim()
        .strip_prefix("row-")
        .unwrap()
        .parse()
        .unwrap()
}

fn begin_drag(state: &mut TuiState, size: Size, start: Position, end: Position) {
    assert_eq!(
        handle_mouse_input(MouseInput::Down(start), size, state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Drag(end), size, state),
        ControllerEffect::None
    );
}

fn expected_rows(start: usize, end: usize) -> String {
    (start..=end).map(label).collect::<Vec<_>>().join("\n")
}

#[tokio::test(start_paused = true)]
async fn stationary_edges_scroll_idle_transcripts_and_copy_every_offscreen_row() {
    for upwards in [true, false] {
        let size = Size::new(32, 18);
        let mut state = state(&transcript(120), !upwards);
        let buffer = draw(&mut state, size);
        let area = area(&state, size);
        let anchor_index = if upwards { 117 } else { 2 };
        let mut anchor = point(&buffer, &label(anchor_index));
        let edge = if upwards {
            anchor.x += 8;
            Position::new(area.x, area.y)
        } else {
            Position::new(area.x + 8, area.bottom() - 1)
        };
        begin_drag(&mut state, size, anchor, edge);
        assert!(!state.is_active_run());
        assert!(state.clipboard_feedback().is_none());
        assert!(needs_refresh(&state));
        let mut interval = new_refresh_interval();
        interval.tick().await;
        let mut previous = visible_text(&buffer, area);
        for _ in 0..8 {
            time::advance(TUI_REFRESH_INTERVAL).await;
            interval.tick().await;
            refresh_interactions(&mut state, size);
            let current = visible_text(&draw(&mut state, size), area);
            assert_ne!(current, previous);
            previous = current;
        }
        assert!(!previous.contains(&label(anchor_index)));
        let buffer = draw(&mut state, size);
        let edge_index = row_index(&buffer, area, edge.y);
        let expected = if upwards {
            assert!(anchor_index - edge_index > usize::from(area.height));
            expected_rows(edge_index, anchor_index)
        } else {
            assert!(edge_index - anchor_index > usize::from(area.height));
            expected_rows(anchor_index, edge_index)
        };
        assert_eq!(
            handle_mouse_input(MouseInput::Up(edge), size, &mut state),
            ControllerEffect::CopyText(expected)
        );
        assert!(!needs_refresh(&state));
        assert_eq!(visible_text(&draw(&mut state, size), area), previous);
        refresh_interactions(&mut state, size);
        assert_eq!(visible_text(&draw(&mut state, size), area), previous);
        assert_eq!(
            handle_mouse_input(MouseInput::Up(edge), size, &mut state),
            ControllerEffect::None
        );
    }
}

#[test]
fn returning_inside_stops_autoscroll_and_the_opposite_edge_reverses_it() {
    let size = Size::new(32, 18);
    let mut state = state(&transcript(120), false);
    let buffer = draw(&mut state, size);
    let area = area(&state, size);
    let anchor = point(&buffer, &label(117));
    let top = Position::new(area.x, area.y);
    begin_drag(&mut state, size, anchor, top);
    refresh_interactions(&mut state, size);
    let after_up = row_index(&draw(&mut state, size), area, area.y);
    let inside = Position::new(area.x, area.y + 4);
    assert_eq!(
        handle_mouse_input(MouseInput::Drag(inside), size, &mut state),
        ControllerEffect::None
    );
    assert!(!needs_refresh(&state));
    for _ in 0..4 {
        refresh_interactions(&mut state, size);
    }
    assert_eq!(row_index(&draw(&mut state, size), area, area.y), after_up);
    let bottom = Position::new(area.x, area.bottom() - 1);
    assert_eq!(
        handle_mouse_input(MouseInput::Drag(bottom), size, &mut state),
        ControllerEffect::None
    );
    assert!(needs_refresh(&state));
    refresh_interactions(&mut state, size);
    assert!(row_index(&draw(&mut state, size), area, area.y) > after_up);
}

#[test]
fn edge_clicks_do_not_scroll_and_dragging_stops_at_both_history_limits() {
    for upwards in [true, false] {
        let size = Size::new(32, 18);
        let mut state = state(&transcript(120), !upwards);
        let buffer = draw(&mut state, size);
        let area = area(&state, size);
        let edge = Position::new(
            area.x + 20,
            if upwards { area.y } else { area.bottom() - 1 },
        );
        assert_eq!(
            handle_mouse_input(MouseInput::Down(edge), size, &mut state),
            ControllerEffect::None
        );
        assert!(!needs_refresh(&state));
        refresh_interactions(&mut state, size);
        assert_eq!(
            visible_text(&draw(&mut state, size), area),
            visible_text(&buffer, area)
        );
        assert_eq!(
            handle_mouse_input(MouseInput::Up(edge), size, &mut state),
            ControllerEffect::None
        );
        let anchor = point(&buffer, &label(if upwards { 117 } else { 2 }));
        begin_drag(&mut state, size, anchor, edge);
        for _ in 0..60 {
            refresh_interactions(&mut state, size);
        }
        assert!(!needs_refresh(&state));
        let at_limit = visible_text(&draw(&mut state, size), area);
        assert!(at_limit.contains(&label(if upwards { 0 } else { 119 })));
        for _ in 0..4 {
            refresh_interactions(&mut state, size);
        }
        assert_eq!(visible_text(&draw(&mut state, size), area), at_limit);
        let opposite = Position::new(edge.x, if upwards { area.bottom() - 1 } else { area.y });
        assert_eq!(
            handle_mouse_input(MouseInput::Drag(opposite), size, &mut state),
            ControllerEffect::None
        );
        assert!(needs_refresh(&state));
        refresh_interactions(&mut state, size);
        assert_ne!(visible_text(&draw(&mut state, size), area), at_limit);
    }
}

#[test]
fn cancellation_removes_edge_scrolling_and_prevents_a_late_release_from_copying() {
    enum Cancel {
        Escape,
        Resize,
        Overlay,
        Click,
        Wheel,
    }
    for cancel in [
        Cancel::Escape,
        Cancel::Resize,
        Cancel::Overlay,
        Cancel::Click,
        Cancel::Wheel,
    ] {
        let size = Size::new(32, 18);
        let mut state = state(&transcript(120), false);
        let buffer = draw(&mut state, size);
        let area = area(&state, size);
        let edge = Position::new(area.x, area.y);
        begin_drag(&mut state, size, point(&buffer, &label(117)), edge);
        refresh_interactions(&mut state, size);
        assert!(state.text_selection_is_autoscrolling());
        match cancel {
            Cancel::Escape => assert_eq!(
                handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state),
                ControllerEffect::None
            ),
            Cancel::Resize => refresh_interactions(&mut state, Size::new(24, 18)),
            Cancel::Overlay => {
                state.show_info_dialog("Modal", "cancel selection".into());
                refresh_interactions(&mut state, size);
            }
            Cancel::Click => assert_eq!(
                handle_mouse_input(MouseInput::Down(Position::new(0, 0)), size, &mut state),
                ControllerEffect::None
            ),
            Cancel::Wheel => handle_mouse_scroll_up(edge, size, &mut state),
        }
        assert!(!state.text_selection_is_autoscrolling());
        assert_eq!(
            handle_mouse_input(MouseInput::Up(edge), size, &mut state),
            ControllerEffect::None
        );
        assert!(!needs_refresh(&state));
    }
}

#[test]
fn streaming_during_autoscroll_keeps_the_copied_snapshot_and_the_final_reading_position() {
    let size = Size::new(32, 18);
    let original = transcript(120);
    let mut state = state(&original, true);
    let buffer = draw(&mut state, size);
    let area = area(&state, size);
    let edge = Position::new(area.x + 8, area.bottom() - 1);
    begin_drag(&mut state, size, point(&buffer, &label(2)), edge);
    refresh_interactions(&mut state, size);
    state.replace_timeline_item(
        0,
        TimelineItem::Assistant {
            text: original.replace("row-", "new-"),
        },
    );
    for _ in 0..8 {
        refresh_interactions(&mut state, size);
    }
    let buffer = draw(&mut state, size);
    let first = row_index(&buffer, area, area.y);
    let last = row_index(&buffer, area, edge.y);
    assert!(!visible_text(&buffer, area).contains("new-"));
    assert_eq!(
        handle_mouse_input(MouseInput::Up(edge), size, &mut state),
        ControllerEffect::CopyText(expected_rows(2, last))
    );
    let after_copy = visible_text(&draw(&mut state, size), area);
    assert!(after_copy.starts_with(&format!("new-{first:05}")));
    assert!(!after_copy.contains("row-"));
    assert!(state.is_timeline_detached());
}

#[test]
fn autoscroll_and_copy_cross_u16_offsets_and_multiple_copy_chunks() {
    let size = Size::new(32, 18);
    let mut state = state(&transcript(66_100), false);
    let buffer = draw(&mut state, size);
    let area = area(&state, size);
    let anchor = point(&buffer, &label(66_097));
    let edge = Position::new(area.x, area.y);
    begin_drag(
        &mut state,
        size,
        Position::new(anchor.x + 8, anchor.y),
        edge,
    );
    for _ in 0..200 {
        refresh_interactions(&mut state, size);
    }
    let buffer = draw(&mut state, size);
    let first = row_index(&buffer, area, area.y);
    assert!(first < usize::from(u16::MAX));
    assert_eq!(
        handle_mouse_input(MouseInput::Up(edge), size, &mut state),
        ControllerEffect::CopyText(expected_rows(first, 66_097))
    );
}

#[test]
fn wrapped_unicode_snapshot_matches_the_visible_transcript_and_copies_whole_glyphs() {
    let size = Size::new(24, 18);
    let mut state = state(
        &format!(
            "{}\nprefix 界🙂e\u{301} suffix",
            "wrapped 界 word ".repeat(500)
        ),
        false,
    );
    let buffer = draw(&mut state, size);
    let area = area(&state, size);
    let prefix = point(&buffer, "prefix");
    let first = Position::new(prefix.x + 7, prefix.y);
    assert_eq!(buffer[(first.x, first.y)].symbol(), "界");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(first.x + 1, first.y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(
        visible_text(&draw(&mut state, size), area),
        visible_text(&buffer, area)
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Drag(first), size, &mut state),
        ControllerEffect::None
    );
    let selected = draw(&mut state, size);
    assert!(
        selected[(first.x, first.y)]
            .modifier
            .contains(Modifier::REVERSED)
    );
    assert!(
        selected[(first.x + 1, first.y)]
            .modifier
            .contains(Modifier::REVERSED)
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Up(first), size, &mut state),
        ControllerEffect::CopyText("界".into())
    );
}
