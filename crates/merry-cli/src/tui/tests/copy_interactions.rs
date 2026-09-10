use crate::tui::{
    controller::{ControllerEffect, handle_key_event, handle_mouse_input},
    keymap::Keymap,
    render::{prepare_viewport, render_to_buffer},
    state::{TimelineItem, TuiState},
    tests::{find_text_position, rendered_buffer_text},
    text_interaction::MouseInput,
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Size},
};

fn state(markdown: &str) -> TuiState {
    let mut state = TuiState::new(
        "/repo".into(),
        "test".into(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: markdown.into(),
    });
    state
}

fn draw(state: &mut TuiState, size: Size) -> Buffer {
    prepare_viewport(state, size);
    render_to_buffer(state, size.width, size.height)
}

fn point(buffer: &Buffer, text: &str) -> Position {
    let (column, row) = find_text_position(buffer, text)
        .unwrap_or_else(|| panic!("missing {text}:\n{}", rendered_buffer_text(buffer)));
    Position::new(column, row)
}

fn buttons(buffer: &Buffer, label: &str) -> Vec<Position> {
    let mut positions = Vec::new();
    for row in buffer.area.y..buffer.area.bottom() {
        for column in buffer.area.x..buffer.area.right() {
            if label.chars().enumerate().all(|(offset, character)| {
                let column = column.saturating_add(u16::try_from(offset).unwrap());
                column < buffer.area.right()
                    && buffer[(column, row)].symbol() == character.to_string()
            }) {
                positions.push(Position::new(column, row));
            }
        }
    }
    positions
}

fn click(state: &TuiState, size: Size, position: Position) -> ControllerEffect {
    let mut state = state.clone();
    let effect = handle_mouse_input(MouseInput::Down(position), size, &mut state);
    assert_eq!(
        handle_mouse_input(MouseInput::Up(position), size, &mut state),
        ControllerEffect::None
    );
    effect
}

fn drag_copy(state: &mut TuiState, size: Size, start: Position, end: Position) -> ControllerEffect {
    assert_eq!(
        handle_mouse_input(MouseInput::Down(start), size, state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Drag(end), size, state),
        ControllerEffect::None
    );
    handle_mouse_input(MouseInput::Up(end), size, state)
}

#[test]
fn copy_buttons_preserve_markdown_and_each_code_blocks_undecorated_text() {
    let markdown = "# Steps\n\nRun **both**:\n```sh\nprintf 'one'\n  printf 'two'\n\n\tprintf '三🙂e\u{301}'\n```\n\nThen:\n```\n  final  \n```\n";
    let mut state = state(markdown);
    state.insert_input_str("unfinished draft");
    let size = Size::new(90, 36);
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&state, size, point(&buffer, "[Copy reply]")),
        ControllerEffect::CopyText(markdown.into())
    );
    let targets = buttons(&buffer, "[Copy code]");
    assert_eq!(targets.len(), 2);
    assert_eq!(
        click(&state, size, targets[0]),
        ControllerEffect::CopyText(
            "printf 'one'\n  printf 'two'\n\n\tprintf '三🙂e\u{301}'\n".into()
        )
    );
    assert_eq!(
        click(&state, size, targets[1]),
        ControllerEffect::CopyText("  final  \n".into())
    );
    assert!(rendered_buffer_text(&draw(&mut state, size)).contains("unfinished draft"));
}

#[test]
fn copy_targets_follow_wrapping_scrolling_and_local_markdown_indentation() {
    let code = "  printf '非常长的命令 alpha beta gamma delta epsilon'\nnext\n";
    let mut state = state(&format!(
        "{}\n```sh\n{code}```",
        "earlier context\n".repeat(80)
    ));
    for width in [20, 80, 32] {
        let size = Size::new(width, 24);
        let buffer = draw(&mut state, size);
        let button = point(&buffer, "[Copy code]");
        assert_eq!(
            click(&state, size, button),
            ControllerEffect::CopyText(code.into())
        );
    }
    state.push_timeline_item(TimelineItem::LocalCommand {
        title: "Help".into(),
        body: "```text\n  local text\n```".into(),
    });
    let size = Size::new(80, 24);
    let buffer = draw(&mut state, size);
    let target = *buttons(&buffer, "[Copy code]").last().unwrap();
    assert_eq!(
        click(&state, size, target),
        ControllerEffect::CopyText("  local text\n".into())
    );
}

#[test]
fn copy_target_survives_timeline_offsets_beyond_u16() {
    let mut state = state(&format!("{}\n```\nTAIL\n```", "prefix\n".repeat(66_000)));
    let size = Size::new(32, 16);
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&state, size, point(&buffer, "[Copy code]")),
        ControllerEffect::CopyText("TAIL\n".into())
    );
}

#[test]
fn literal_copy_labels_and_hidden_controls_are_not_click_targets() {
    let mut state = state("Text contains [Copy code] but this is not a control.\n\n```\nreal\n```");
    let size = Size::new(80, 24);
    let buffer = draw(&mut state, size);
    let labels = buttons(&buffer, "[Copy code]");
    assert_eq!(labels.len(), 2);
    assert_eq!(click(&state, size, labels[0]), ControllerEffect::None);
    state.show_info_dialog("Modal", "Do not copy behind this dialog".into());
    assert_eq!(click(&state, size, labels[1]), ControllerEffect::None);
    state.close_overlay();
    let narrow = draw(&mut state, Size::new(5, 24));
    assert!(buttons(&narrow, "[Copy]").is_empty());
}

#[test]
fn streamed_code_copy_uses_current_source_without_rendered_wrapping() {
    let mut state = state("```sh\nfirst\n");
    let size = Size::new(80, 24);
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&state, size, point(&buffer, "[Copy code]")),
        ControllerEffect::CopyText("first\n".into())
    );
    state.append_assistant_delta(Some(0), "  second\n```\n");
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&state, size, point(&buffer, "[Copy code]")),
        ControllerEffect::CopyText("first\n  second\n".into())
    );
}

#[test]
fn dragging_plain_content_copies_on_release_and_clears_selection() {
    let mut state = state("alpha 界🙂e\u{301}\n```text\n  beta\n```");
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    let start = point(&buffer, "alpha");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(start), size, &mut state),
        ControllerEffect::None
    );
    let end = Position::new(start.x + 4, start.y);
    assert_eq!(
        handle_mouse_input(MouseInput::Drag(end), size, &mut state),
        ControllerEffect::None
    );
    let selected = draw(&mut state, size);
    assert!(
        selected[(start.x, start.y)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Up(end), size, &mut state),
        ControllerEffect::CopyText("alpha".into())
    );
    assert!(state.text_selection().is_none());
}

#[test]
fn dragging_multiline_content_copies_visible_text_without_markdown_controls() {
    let mut state = state("alpha\n  beta\nomega");
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    let start = point(&buffer, "alpha");
    let end = point(&buffer, "omega");
    assert_eq!(
        drag_copy(&mut state, size, start, Position::new(end.x + 4, end.y),),
        ControllerEffect::CopyText("alpha\nbeta\nomega".into())
    );
    assert!(state.text_selection().is_none());
}

#[test]
fn dragging_can_start_from_the_timeline_edge() {
    let mut state = state("alpha\nbeta");
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    let end = point(&buffer, "alpha");
    let start = Position::new(0, end.y);
    assert_eq!(
        drag_copy(&mut state, size, start, Position::new(end.x + 4, end.y)),
        ControllerEffect::CopyText("alpha".into())
    );
}

#[test]
fn dragging_strips_timeline_decoration_from_markdown_output() {
    let mut state = TuiState::new(
        "/repo".into(),
        "test".into(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "Request".into(),
        lane: merry_core::QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "# Title\n\n```sh\nprintf 'hello'\n\nnext\n```".into(),
    });
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    let start = point(&buffer, "Request");
    let end = point(&buffer, "next");
    let copied = match drag_copy(
        &mut state,
        size,
        start,
        Position::new(size.width.saturating_sub(1), end.y),
    ) {
        ControllerEffect::CopyText(text) => text,
        effect => panic!("expected copied text, got {effect:?}"),
    };
    assert!(copied.contains("Title"));
    assert_eq!(copied, "Request\n\nTitle\nprintf 'hello'\n\nnext");
    assert!(!copied.contains('▌'));
    assert!(!copied.contains('▎'));
    assert!(!copied.contains("[Copy"));
}

#[test]
fn dragging_preserves_literal_controls_and_rail_glyphs_in_content() {
    let mut state = state("literal [Copy code]\nleft ▎ content\n▌ body");
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    let start = point(&buffer, "literal");
    let end = point(&buffer, "▌ body");
    assert_eq!(
        drag_copy(
            &mut state,
            size,
            start,
            Position::new(size.width.saturating_sub(1), end.y),
        ),
        ControllerEffect::CopyText("literal [Copy code]\nleft ▎ content\n▌ body".into())
    );
}

#[test]
fn dragging_local_code_strips_both_local_gutter_and_code_rail() {
    let mut state = TuiState::new(
        "/repo".into(),
        "test".into(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::LocalCommand {
        title: "Output".into(),
        body: "```text\n  local text\n\nnext\n```".into(),
    });
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    let start = point(&buffer, "local text");
    let end = point(&buffer, "next");
    assert_eq!(
        drag_copy(
            &mut state,
            size,
            start,
            Position::new(size.width.saturating_sub(1), end.y),
        ),
        ControllerEffect::CopyText("local text\n\nnext".into())
    );
}

#[test]
fn reply_copy_preserves_source_indentation() {
    let markdown = "alpha\n  beta\nomega";
    let mut state = state(markdown);
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&state, size, point(&buffer, "[Copy reply]")),
        ControllerEffect::CopyText(markdown.into())
    );
}

#[test]
fn plain_click_does_not_freeze_streaming_or_intercept_copy_buttons() {
    let mut state = state("alpha\n```\nold code\n```");
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&state, size, point(&buffer, "alpha")),
        ControllerEffect::None
    );
    state.replace_timeline_item(
        0,
        TimelineItem::Assistant {
            text: "new text\n```\nnew code\n```".into(),
        },
    );
    let buffer = draw(&mut state, size);
    let rendered = rendered_buffer_text(&buffer);
    assert!(rendered.contains("new text"));
    assert!(!rendered.contains("alpha"));
    assert_eq!(
        click(&state, size, point(&buffer, "[Copy code]")),
        ControllerEffect::CopyText("new code\n".into())
    );
}

#[test]
fn clicking_transcript_does_not_repurpose_control_c_or_tmux_prefix_keys() {
    for key in [
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE),
    ] {
        let mut state = state("alpha beta");
        state.insert_input_str("draft");
        let size = Size::new(80, 20);
        let buffer = draw(&mut state, size);
        assert_eq!(
            click(&state, size, point(&buffer, "alpha")),
            ControllerEffect::None
        );
        assert!(!matches!(
            handle_key_event(key, &mut state),
            ControllerEffect::CopyText(_)
        ));
    }
}

#[test]
fn copy_buttons_do_not_add_keyboard_bindings() {
    for key in [
        KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        KeyEvent::new(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::ALT),
    ] {
        let mut state = state("alpha beta");
        assert_eq!(state.keymap().action_for(key.into()), None);
        let size = Size::new(80, 20);
        let buffer = draw(&mut state, size);
        assert_eq!(
            click(&state, size, point(&buffer, "alpha")),
            ControllerEffect::None
        );
        assert_eq!(handle_key_event(key, &mut state), ControllerEffect::None);
    }
}

#[tokio::test(start_paused = true)]
async fn clipboard_feedback_is_visible_without_an_overlay_and_expires() {
    let mut state = state("body");
    let size = Size::new(80, 20);
    state.show_clipboard_feedback("Copy failed: clipboard unavailable".into(), true);
    assert!(
        rendered_buffer_text(&draw(&mut state, size))
            .contains("Copy failed: clipboard unavailable")
    );
    tokio::time::advance(std::time::Duration::from_secs(4)).await;
    state.expire_clipboard_feedback();
    let rendered = rendered_buffer_text(&draw(&mut state, size));
    assert!(!rendered.contains("Copy failed"));
    assert!(rendered.contains("Ready"));
}
