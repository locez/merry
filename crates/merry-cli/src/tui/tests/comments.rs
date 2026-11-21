use crate::tui::{
    controller::{ControllerEffect, handle_key_action, handle_key_event, handle_mouse_input},
    input::TuiSubmission,
    keymap::{KeyAction, Keymap},
    render::{prepare_viewport, render_to_buffer},
    state::{TimelineItem, TuiState},
    tests::{find_text_position, rendered_buffer_text},
    text_interaction::MouseInput,
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Position,
    layout::Size,
    style::{Color, Modifier},
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

fn drag(state: &mut TuiState, size: Size, start: Position, end: Position) -> ControllerEffect {
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

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

fn review(quote: &str, note: &str) -> String {
    format!("Review comments:\n\n> {quote}\n\n{note}")
}

#[test]
fn selecting_text_outside_comment_mode_still_copies() {
    let mut state = state("alpha beta");
    let size = Size::new(80, 20);
    let buffer = draw(&mut state, size);
    let start = point(&buffer, "alpha");
    assert_eq!(
        drag(&mut state, size, start, Position::new(start.x + 4, start.y)),
        ControllerEffect::CopyText("alpha".into())
    );
}

#[test]
fn comment_mode_collects_quotes_and_sends_them_together() {
    let mut state = state("alpha beta\nomega tail");
    let size = Size::new(80, 20);
    handle_key_action(KeyAction::ToggleComment, &mut state);
    assert!(state.is_commenting());

    let buffer = draw(&mut state, size);
    let rendered = rendered_buffer_text(&buffer);
    assert!(rendered.contains(&format!(
        "[{} commenting]",
        crate::tui::keymap::alt_chord("C")
    )));
    assert!(rendered.contains("Comment · select text"));
    let start = point(&buffer, "alpha");
    assert_eq!(
        drag(&mut state, size, start, Position::new(start.x + 4, start.y)),
        ControllerEffect::None
    );
    assert_eq!(state.pending_comment_quote(), Some("alpha"));

    state.insert_input_str("rename it");
    assert_eq!(
        handle_key_event(enter(), &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.comment_count(), 1);
    assert_eq!(state.pending_comment_quote(), None);
    assert!(state.input_text().is_empty());

    let buffer = draw(&mut state, size);
    let start = point(&buffer, "omega");
    assert_eq!(
        drag(&mut state, size, start, Position::new(start.x + 4, start.y)),
        ControllerEffect::None
    );
    state.insert_input_str("check this");
    assert_eq!(
        handle_key_event(enter(), &mut state),
        ControllerEffect::None
    );

    state.insert_input_str("and the docs");
    assert_eq!(
        handle_key_event(enter(), &mut state),
        ControllerEffect::Submit(TuiSubmission {
            text:
                "Review comments:\n\n> alpha\n\nrename it\n\n> omega\n\ncheck this\n\nand the docs"
                    .into(),
            history_text:
                "Review comments:\n\n> alpha\n\nrename it\n\n> omega\n\ncheck this\n\nand the docs"
                    .into(),
            images: Vec::new(),
        })
    );
    assert!(!state.is_commenting());
    assert_eq!(state.comment_count(), 0);
}

#[test]
fn empty_comment_does_not_pin_or_send_a_captured_quote() {
    let mut state = state("alpha");
    handle_key_action(KeyAction::ToggleComment, &mut state);
    state.capture_comment_quote("alpha".into());
    assert_eq!(
        handle_key_event(enter(), &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.comment_count(), 0);
    assert_eq!(state.pending_comment_quote(), Some("alpha"));
}

#[test]
fn status_comment_chip_enters_the_mode_and_send_submits() {
    let mut state = state("alpha");
    let size = Size::new(100, 24);
    let buffer = draw(&mut state, size);
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(point(
                &buffer,
                &format!("[{} comment]", crate::tui::keymap::alt_chord("C")),
            )),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    assert!(state.is_commenting());
    state.capture_comment_quote("alpha".into());
    state.insert_input_str("rename it");
    let buffer = draw(&mut state, size);
    assert_eq!(
        handle_mouse_input(MouseInput::Down(point(&buffer, "[send]")), size, &mut state),
        ControllerEffect::Submit(TuiSubmission {
            text: review("alpha", "rename it"),
            history_text: review("alpha", "rename it"),
            images: Vec::new(),
        })
    );
}

#[test]
fn escape_drops_the_quote_then_leaves_the_mode_and_backspace_restores_a_comment() {
    let mut state = state("alpha");
    handle_key_action(KeyAction::ToggleComment, &mut state);
    state.capture_comment_quote("alpha".into());
    state.insert_input_str("rename it");
    assert_eq!(
        handle_key_event(enter(), &mut state),
        ControllerEffect::None
    );

    state.capture_comment_quote("beta".into());
    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.pending_comment_quote(), None);
    assert!(state.is_commenting());
    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert!(!state.is_commenting());
    assert_eq!(state.comment_count(), 1);

    handle_key_action(KeyAction::ToggleComment, &mut state);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(state.pending_comment_quote(), Some("alpha"));
    assert_eq!(state.input_text(), "rename it");
    assert_eq!(state.comment_count(), 0);
}

#[test]
fn ctrl_c_clears_a_saved_comment_draft_without_quitting() {
    let mut state = state("alpha");
    handle_key_action(KeyAction::ToggleComment, &mut state);
    state.capture_comment_quote("alpha".into());
    state.insert_input_str("rename it");
    handle_key_event(enter(), &mut state);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(state.comment_count(), 0);
    assert!(!state.is_commenting());
}

#[test]
fn saved_quote_stays_highlighted_and_a_click_edits_the_note() {
    let mut state = state("alpha beta");
    let size = Size::new(80, 20);
    handle_key_action(KeyAction::ToggleComment, &mut state);
    let buffer = draw(&mut state, size);
    let start = point(&buffer, "alpha");
    assert_eq!(
        drag(&mut state, size, start, Position::new(start.x + 4, start.y)),
        ControllerEffect::None
    );
    state.insert_input_str("rename it");
    assert_eq!(
        handle_key_event(enter(), &mut state),
        ControllerEffect::None
    );

    let buffer = draw(&mut state, size);
    let start = point(&buffer, "alpha");
    let style = buffer[(start.x, start.y)].style();
    assert!(
        style.add_modifier.contains(Modifier::UNDERLINED),
        "quoted text should stay underlined"
    );
    assert_eq!(style.bg, Some(Color::Magenta));

    assert_eq!(
        handle_mouse_input(MouseInput::Down(start), size, &mut state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Up(start), size, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.comment_count(), 1);
    assert_eq!(state.pending_comment_quote(), Some("alpha"));
    assert_eq!(state.input_text(), "rename it");
    assert!(state.is_commenting());

    state.insert_input_str(" now");
    assert_eq!(
        handle_key_event(enter(), &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.comment_count(), 1);
    assert_eq!(state.pending_comment_quote(), None);
    assert_eq!(
        handle_key_event(enter(), &mut state),
        ControllerEffect::Submit(TuiSubmission {
            text: review("alpha", "rename it now"),
            history_text: review("alpha", "rename it now"),
            images: Vec::new(),
        })
    );
}

#[test]
fn each_saved_quote_gets_its_own_mark_and_a_click_works_after_leaving() {
    let mut state = state("alpha\nomega");
    let size = Size::new(80, 24);
    handle_key_action(KeyAction::ToggleComment, &mut state);
    let buffer = draw(&mut state, size);
    let alpha = point(&buffer, "alpha");
    drag(&mut state, size, alpha, Position::new(alpha.x + 4, alpha.y));
    state.insert_input_str("first");
    handle_key_event(enter(), &mut state);
    let buffer = draw(&mut state, size);
    let omega = point(&buffer, "omega");
    drag(&mut state, size, omega, Position::new(omega.x + 4, omega.y));
    state.insert_input_str("second");
    handle_key_event(enter(), &mut state);

    let buffer = draw(&mut state, size);
    let alpha = point(&buffer, "alpha");
    let omega = point(&buffer, "omega");
    assert!(
        buffer[(alpha.x, alpha.y)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert!(
        buffer[(omega.x, omega.y)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert_eq!(buffer[(omega.x + 5, omega.y)].symbol(), "2");

    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert!(!state.is_commenting());
    assert_eq!(
        handle_mouse_input(MouseInput::Down(omega), size, &mut state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Up(omega), size, &mut state),
        ControllerEffect::None
    );
    assert!(state.is_commenting());
    assert_eq!(state.input_text(), "second");
    assert_eq!(state.comment_count(), 2);
}

#[test]
fn a_pinned_user_quote_stays_highlighted_and_can_be_edited() {
    let mut state = TuiState::new(
        "/repo".into(),
        "test".into(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "keep this".into(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "tail".into(),
    });
    let size = Size::new(80, 16);
    handle_key_action(KeyAction::ToggleComment, &mut state);
    let buffer = draw(&mut state, size);
    let start = point(&buffer, "keep");
    assert_eq!(
        drag(&mut state, size, start, Position::new(start.x + 4, start.y)),
        ControllerEffect::None
    );
    state.insert_input_str("wording");
    handle_key_event(enter(), &mut state);
    for index in 0..30 {
        state.push_timeline_item(TimelineItem::Assistant {
            text: format!("filler {index}"),
        });
    }

    let buffer = draw(&mut state, size);
    let start = point(&buffer, "keep");
    assert!(
        buffer[(start.x, start.y)]
            .style()
            .add_modifier
            .contains(Modifier::UNDERLINED),
        "pinned quote should stay marked:\n{}",
        rendered_buffer_text(&buffer)
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Down(start), size, &mut state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_input(MouseInput::Up(start), size, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "wording");
    assert_eq!(state.comment_count(), 1);
}
