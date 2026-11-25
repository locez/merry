use crate::{
    config::{BrandMarkMode, CopyButtonPosition},
    tui::{
        controller::{ControllerEffect, handle_key_event, handle_mouse_input},
        keymap::Keymap,
        overlay::{Overlay, SettingItem},
        render::{prepare_viewport, render_to_buffer},
        state::{TimelineItem, TuiState},
        tests::{find_text_position, rendered_buffer_text},
        text_interaction::MouseInput,
        theme::TuiTheme,
    },
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
    let targets = buttons(&buffer, "⧉ Copy");
    assert_eq!(targets.len(), 3);
    assert_eq!(
        click(&state, size, targets[0]),
        ControllerEffect::CopyText(markdown.into())
    );
    assert_eq!(
        click(&state, size, targets[1]),
        ControllerEffect::CopyText(
            "printf 'one'\n  printf 'two'\n\n\tprintf '三🙂e\u{301}'\n".into()
        )
    );
    assert_eq!(
        click(&state, size, targets[2]),
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
        let button = *buttons(&buffer, "⧉ Copy").last().unwrap();
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
    let target = *buttons(&buffer, "⧉ Copy").last().unwrap();
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
        click(&state, size, *buttons(&buffer, "⧉ Copy").last().unwrap(),),
        ControllerEffect::CopyText("TAIL\n".into())
    );
}

#[test]
fn literal_copy_labels_and_hidden_controls_are_not_click_targets() {
    let mut state = state("Text contains [Copy code] but this is not a control.\n\n```\nreal\n```");
    let size = Size::new(80, 24);
    let buffer = draw(&mut state, size);
    let labels = buttons(&buffer, "[Copy code]");
    assert_eq!(labels.len(), 1);
    assert_eq!(click(&state, size, labels[0]), ControllerEffect::None);
    let control = point(&buffer, "⧉ Copy");
    state.show_info_dialog("Modal", "Do not copy behind this dialog".into());
    assert_eq!(click(&state, size, control), ControllerEffect::None);
    state.close_overlay();
    let narrow = draw(&mut state, Size::new(5, 24));
    assert!(buttons(&narrow, "⧉").is_empty());
}

#[test]
fn streamed_code_copy_uses_current_source_without_rendered_wrapping() {
    let mut state = state("```sh\nfirst\n");
    let size = Size::new(80, 24);
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&state, size, *buttons(&buffer, "⧉ Copy").last().unwrap(),),
        ControllerEffect::CopyText("first\n".into())
    );
    state.append_assistant_delta(Some(0), "  second\n```\n");
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&state, size, *buttons(&buffer, "⧉ Copy").last().unwrap(),),
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
    assert_eq!(copied, "Request\n\nTitle\n\nprintf 'hello'\n\nnext");
    assert!(!copied.contains('❯'));
    assert!(!copied.contains('▌'));
    assert!(!copied.contains('▎'));
    assert!(!copied.contains('▔'));
    assert!(!copied.contains('▁'));
    assert!(!copied.contains('━'));
    assert!(!copied.contains("[Copy"));
    assert!(!copied.contains("⧉ Copy"));
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
        click(&state, size, point(&buffer, "⧉ Copy")),
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
        click(&state, size, *buttons(&buffer, "⧉ Copy").last().unwrap(),),
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

#[test]
fn copy_buttons_respect_all_four_configurable_corners_and_invalidate_cache_live() {
    let markdown = "# Title\n\nBody text.\n\n```rust\nfn main() {}\n```\n";
    let size = Size::new(80, 24);

    for position in [
        CopyButtonPosition::TopLeft,
        CopyButtonPosition::TopRight,
        CopyButtonPosition::BottomLeft,
        CopyButtonPosition::BottomRight,
    ] {
        let mut state = state(markdown).with_copy_button_position(position);
        let buffer = draw(&mut state, size);
        let targets = buttons(&buffer, "⧉ Copy");
        assert_eq!(
            targets.len(),
            2,
            "expected reply and code copy buttons for {position:?}:\n{}",
            rendered_buffer_text(&buffer)
        );

        let title_pos = point(&buffer, "Title");
        let code_pos = point(&buffer, "fn main()");
        let anchor_pos = point(&buffer, "Merry");
        assert!(
            anchor_pos.y < title_pos.y,
            "reply anchor header should sit above content for {position:?}"
        );
        let rendered = rendered_buffer_text(&buffer);
        assert!(
            rendered.contains("🎀") && rendered.contains("Merry") && rendered.contains('┄'),
            "reply anchor header should include brand mark and dashed rule for {position:?}:\n{rendered}"
        );
        let is_top = matches!(
            position,
            CopyButtonPosition::TopLeft | CopyButtonPosition::TopRight
        );
        let (reply_btn, code_btn) = if is_top {
            (targets[0], targets[1])
        } else {
            (targets[1], targets[0])
        };

        if is_top {
            assert!(
                reply_btn.y < title_pos.y,
                "reply button should sit above content for {position:?}"
            );
            assert!(
                code_btn.y < code_pos.y,
                "code button should sit above code body for {position:?}"
            );
        } else {
            assert!(
                code_btn.y > code_pos.y,
                "code button should sit below code body for {position:?}"
            );
            assert!(
                reply_btn.y > code_btn.y,
                "reply button should sit below the entire reply for {position:?}"
            );
        }

        if position.is_left() {
            assert!(
                reply_btn.x < size.width / 2 && code_btn.x < size.width / 2,
                "left-aligned copy buttons should sit on the left side for {position:?}: reply={reply_btn:?}, code={code_btn:?}"
            );
        } else {
            assert!(
                reply_btn.x > size.width / 2 && code_btn.x > size.width / 2,
                "right-aligned copy buttons should sit on the right side for {position:?}: reply={reply_btn:?}, code={code_btn:?}"
            );
        }

        assert_eq!(
            click(&state, size, reply_btn),
            ControllerEffect::CopyText(markdown.into()),
            "reply copy button should copy full markdown for {position:?}"
        );
        assert_eq!(
            click(&state, size, code_btn),
            ControllerEffect::CopyText("fn main() {}\n".into()),
            "code copy button should copy exact code for {position:?}"
        );

        // Dragging across the message must skip copy and anchor bars in every corner configuration.
        let mut drag_state = state.clone();
        let _ = draw(&mut drag_state, size);
        let start = Position::new(0, anchor_pos.y.min(reply_btn.y));
        let end = Position::new(size.width.saturating_sub(1), reply_btn.y.max(code_pos.y));
        let copied = match drag_copy(&mut drag_state, size, start, end) {
            ControllerEffect::CopyText(text) => text,
            effect => panic!("expected dragged text for {position:?}, got {effect:?}"),
        };
        assert!(
            !copied.contains("⧉ Copy"),
            "leaked copy label for {position:?}: {copied:?}"
        );
        assert!(
            !copied.contains("Merry") && !copied.contains('┄'),
            "leaked reply anchor header for {position:?}: {copied:?}"
        );
        assert!(
            !copied.contains('▎'),
            "leaked code rail for {position:?}: {copied:?}"
        );
        assert!(copied.contains("Title") && copied.contains("fn main() {}"));
    }

    // Switching brand_mark to Snowflake updates the reply anchor glyph live.
    let mut snowflake = state(markdown).with_brand_mark(BrandMarkMode::Snowflake);
    let snowflake_text = rendered_buffer_text(&draw(&mut snowflake, size));
    assert!(
        snowflake_text.contains("❆ Merry"),
        "snowflake mode should render ❆ Merry anchor:\n{snowflake_text}"
    );
    assert!(!snowflake_text.contains("🎀"));

    // Live Settings change must invalidate timeline cache and move copy buttons immediately.
    let mut live = state(markdown);
    let initial_buf = draw(&mut live, size);
    let initial_reply = buttons(&initial_buf, "⧉ Copy")[0];
    assert!(initial_reply.x > size.width / 2);

    live.open_settings();
    if let Some(Overlay::Settings(settings)) = live.overlay_mut() {
        settings.select_item(SettingItem::CopyButtonPosition);
    }
    let _ = handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut live);
    assert_eq!(live.copy_button_position(), CopyButtonPosition::BottomRight);
    live.close_overlay();

    let updated_buf = draw(&mut live, size);
    let updated_targets = buttons(&updated_buf, "⧉ Copy");
    assert_eq!(updated_targets.len(), 2);
    let title_y = point(&updated_buf, "Title").y;
    assert!(
        updated_targets[1].y > title_y && updated_targets[1].x > size.width / 2,
        "live setting update should re-render reply copy button at bottom-right"
    );
    assert_eq!(
        click(&live, size, updated_targets[1]),
        ControllerEffect::CopyText(markdown.into())
    );
}
