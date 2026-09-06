use crate::tui::{
    keymap::Keymap,
    render::{render_to_buffer, render_to_buffer_and_cursor, render_to_text},
    state::{QueuePreview, TimelineItem, TuiState},
    tests::{
        draft_image, find_cell_color, find_cell_style, find_text_position, rendered_buffer_text,
    },
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{QueuedInputLane, QueuedInputView};
use ratatui::style::{Color, Modifier};

#[test]
fn renderer_highlights_complete_image_placeholders_in_the_composer() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("inspect ");
    state
        .input_mut()
        .insert_image(draft_image(1))
        .expect("image should insert");
    state.insert_input_str(" now");

    let buffer = render_to_buffer(&state, 80, 16);

    assert_eq!(
        find_cell_color(&buffer, "[Image #1]"),
        Some(Color::LightMagenta)
    );
}

#[test]
fn queue_preview_keeps_actual_text_and_truncates_for_display() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "short next".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![QueuedInputView {
            text: "a very long backlog item that should be truncated".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });

    assert_eq!(state.queue_preview().next[0].text, "short next");
    assert_eq!(
        state.queue_preview().backlog[0].display_text(18),
        "a very long bac..."
    );
}

#[test]
fn renderer_shows_status_timeline_queue_and_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "assistant says hello".to_owned(),
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![QueuedInputView {
            text: "backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });
    state.input_mut().insert_char('h');
    state.input_mut().insert_char('i');

    let text = render_to_text(&state, 79, 24);

    assert!(text.contains("Ready"));
    assert!(text.contains("gpt-test"));
    assert!(text.contains("assistant says hello"));
    assert!(text.contains("Next"));
    assert!(text.contains("next item"));
    assert!(text.contains("Backlog"));
    assert!(text.contains("backlog item"));
    assert!(text.contains("hi"));
}

#[test]
fn renderer_uses_one_timeline_without_permanent_side_rails() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "make the TUI distinct".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran cargo test -p merry-cli".to_owned(),
        body: "ok".to_owned(),
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "queued next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![QueuedInputView {
            text: "queued backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });

    let text = render_to_text(&state, 180, 32);

    assert!(text.contains("merry"));
    assert!(text.contains("make the TUI distinct"));
    assert!(text.contains("Ran cargo test -p merry-cli"));
    assert!(text.contains("queued next item"));
    assert!(text.contains("queued backlog item"));
    assert!(!text.contains("CHAT"));
    assert!(!text.contains("FOCUS"));
    assert!(!text.contains("RUN"));
}

#[test]
fn renderer_keeps_medium_terminal_focused_on_the_timeline() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "review layout".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Muted {
        title: "Read".to_owned(),
        detail: "AGENTS.md".to_owned(),
    });

    let text = render_to_text(&state, 140, 28);

    assert!(text.contains("review layout"));
    assert!(text.contains("Read AGENTS.md"));
    assert!(!text.contains("FOCUS"));
    assert!(!text.contains("RUN"));
}

#[test]
fn renderer_keeps_bottom_queue_on_narrow_terminal() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "narrow next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });

    let text = render_to_text(&state, 79, 24);

    assert!(!text.contains("FOCUS"));
    assert!(!text.contains("RUN"));
    assert!(text.contains("queue"));
    assert!(text.contains("narrow next item"));
    assert!(text.contains("M"));
    assert!(!text.contains("input"));
}

#[test]
fn narrow_chat_shows_read_result_preview() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Read hello_world.py".to_owned(),
        body: "print(\"Hello, Merry!\")".to_owned(),
    });

    let text = render_to_text(&state, 79, 24);

    assert!(text.contains("Read hello_world.py"));
    assert!(text.contains("print(\"Hello, Merry!\")"));
}

#[test]
fn standard_width_keeps_timeline_as_the_content_surface() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Read hello_world.py".to_owned(),
        body: "print(\"Hello, Merry!\")".to_owned(),
    });
    let text = render_to_text(&state, 100, 24);

    assert!(text.contains("Read hello_world.py"));
    assert!(text.contains("print(\"Hello, Merry!\")"));
    assert!(!text.contains("CHAT"));
    assert!(!text.contains("FOCUS"));
    assert!(!text.contains("RUN"));
}

#[test]
fn renderer_preserves_user_message_newlines() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "1234\n换行测试".to_owned(),
        lane: QueuedInputLane::Next,
    });

    let text = render_to_text(&state, 80, 16);

    assert!(text.contains("▌ 1234"));
    assert!(text.contains("▌ 换 行 测 试"));
    assert!(!text.contains("user:"));
}

#[test]
fn renderer_places_terminal_cursor_inside_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.input_mut().insert_str("a你b");
    state
        .input_mut()
        .handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

    let (_buffer, cursor) = render_to_buffer_and_cursor(&state, 80, 16);

    assert_eq!(cursor.x, 4);
    assert_eq!(cursor.y, 13);
}

#[test]
fn renderer_places_terminal_cursor_on_multiline_input_row() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("first");
    state.insert_input_newline();
    state.insert_input_str("second");

    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 80, 16);

    assert!(rendered_buffer_text(&buffer).contains("first"));
    assert!(rendered_buffer_text(&buffer).contains("second"));
    assert_eq!(cursor.x, 7);
    assert_eq!(cursor.y, 13);
}

#[test]
fn renderer_shows_user_input_in_timeline() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "查一下 baidu.com".to_owned(),
        lane: QueuedInputLane::Next,
    });

    let buffer = render_to_buffer(&state, 80, 16);
    let text = rendered_buffer_text(&buffer);

    assert!(text.contains("▌ 查"));
    assert!(!text.contains("user:"));
    assert!(text.contains("baidu.com"));
    let accent_style = find_cell_style(&buffer, "▌").expect("user accent should render");
    let body_style = find_cell_style(&buffer, "baidu.com").expect("user body should render");
    assert_eq!(accent_style.fg, Some(Color::LightMagenta));
    assert!(accent_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(body_style.fg, Some(Color::White));
}

#[test]
fn renderer_hides_empty_queue_panel_to_preserve_timeline_space() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "latest local echo".to_owned(),
        lane: QueuedInputLane::Next,
    });

    let text = render_to_text(&state, 80, 16);

    assert!(text.contains("latest local echo"));
    assert!(!text.contains("queue"));
    assert!(!text.contains("Next"));
    assert!(!text.contains("Suspended"));
    assert!(!text.contains("Backlog"));
}

#[test]
fn renderer_scrolls_timeline_viewport() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    for index in 0..12 {
        state.push_timeline_item(TimelineItem::Assistant {
            text: format!("line {index}"),
        });
    }

    let bottom = render_to_text(&state, 48, 12);
    assert!(bottom.contains("line 11"));
    assert!(!bottom.contains("line 0"));

    state.scroll_timeline_up();
    state.scroll_timeline_up();
    state.scroll_timeline_up();
    let scrolled = render_to_text(&state, 48, 12);
    assert!(scrolled.contains("line 10"));
    assert!(!scrolled.contains("line 11"));
}

#[test]
fn cockpit_wide_timeline_scroll_still_changes_chat_viewport() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    for index in 0..20 {
        state.push_timeline_item(TimelineItem::Assistant {
            text: format!("assistant line {index}"),
        });
    }

    let bottom = render_to_text(&state, 180, 24);
    state.scroll_timeline_up_by(10);
    let scrolled = render_to_text(&state, 180, 24);

    assert!(bottom.contains("assistant line 19"));
    assert_ne!(bottom, scrolled);
    assert!(state.timeline_scroll_offset() >= 10);
}

#[test]
fn renderer_review_user_input_starts_viewport_at_selected_user_turn() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "first answer".to_owned(),
    });
    state.push_timeline_item(TimelineItem::User {
        text: "second request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "second answer".to_owned(),
    });

    state.jump_to_previous_user_input();
    let second = render_to_text(&state, 80, 18);
    assert!(second.contains("▌ second request"));
    assert!(!second.contains("first request"));

    state.jump_to_previous_user_input();
    let first = render_to_text(&state, 80, 18);
    assert!(first.contains("▌ first request"));
    assert!(first.contains("first answer"));

    state.exit_timeline_review();
    let bottom = render_to_text(&state, 80, 18);
    assert!(bottom.contains("second answer"));
}

#[test]
fn cockpit_wide_cursor_remains_inside_input_with_cjk_text() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("你好 cockpit");

    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 180, 24);
    let input_label = find_text_position(&buffer, "M").expect("input brand should render");

    assert!(cursor.y > input_label.1);
    assert!(cursor.x > input_label.0);
    assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), " ");
}

#[test]
fn renderer_does_not_underline_cjk_strong_text_in_nested_lists() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "- **橙子**\n  - **血橙**\n  - **脐橙**\n    - **赣南脐橙**".to_owned(),
    });

    let rendered = render_to_text(&state, 80, 18);
    let buffer = render_to_buffer(&state, 80, 18);
    for text in ["橙", "血", "脐", "赣"] {
        let style = find_cell_style(&buffer, text)
            .unwrap_or_else(|| panic!("strong CJK list item should render: {text}\n{rendered}"));
        assert_eq!(style.fg, Some(Color::LightMagenta));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(!style.add_modifier.contains(Modifier::UNDERLINED));
    }
}

#[test]
fn renderer_ellipsizes_queue_items_to_region_width() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "abcdefghijklmnopqrstuvwxyz".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });

    let text = render_to_text(&state, 18, 12);

    assert!(
        text.lines()
            .any(|line| line.contains("  1. ") && line.contains("..."))
    );
    assert!(!text.contains("abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn queue_preview_truncates_long_content_on_narrow_terminal() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "this queue item is intentionally long enough to exceed the right rail width"
                .to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });

    let text = render_to_text(&state, 50, 24);

    assert!(text.contains("this queue item"));
    assert!(text.contains("..."));
    assert!(!text.contains("exceed the right rail width"));
}

#[test]
fn very_short_terminal_keeps_input_visible() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("short terminal input");

    let text = render_to_text(&state, 100, 8);

    assert!(text.contains("M"));
    assert!(text.contains("gpt-test"));
}

#[test]
fn renderer_keeps_input_region_stable_when_queue_count_changes() {
    let mut one_item_state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    one_item_state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });
    let mut three_lane_state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    three_lane_state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![QueuedInputView {
            text: "suspended item".to_owned(),
            lane: QueuedInputLane::Suspended,
            position: 0,
        }],
        backlog: vec![QueuedInputView {
            text: "backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });

    let one_item_text = render_to_text(&one_item_state, 80, 18);
    let three_lane_text = render_to_text(&three_lane_state, 80, 18);
    let one_item_input_row = one_item_text
        .lines()
        .position(|line| line.contains("M"))
        .expect("one item queue render should show input");
    let three_lane_input_row = three_lane_text
        .lines()
        .position(|line| line.contains("M"))
        .expect("three lane queue render should show input");

    assert_eq!(one_item_input_row, three_lane_input_row);
}

#[test]
fn renderer_keeps_timeline_visible_when_bottom_panes_are_taller_than_short_window() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "latest assistant output".to_owned(),
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![QueuedInputView {
            text: "suspended item".to_owned(),
            lane: QueuedInputLane::Suspended,
            position: 0,
        }],
        backlog: vec![QueuedInputView {
            text: "backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });
    state.insert_input_str("line one\nline two\nline three\nline four\nline five");

    let text = render_to_text(&state, 79, 10);

    assert!(text.contains("latest assistant output"));
    assert!(text.contains("line five"));
    assert!(text.contains("M"));
    assert!(!text.contains("input"));
    assert!(text.contains("gpt-test"));
    let assistant_row = text
        .lines()
        .position(|line| line.contains("latest assistant output"))
        .expect("assistant output should render");
    let input_row = text
        .lines()
        .position(|line| line.contains("M"))
        .expect("input panel should render");
    assert!(input_row > assistant_row, "{text}");
}
