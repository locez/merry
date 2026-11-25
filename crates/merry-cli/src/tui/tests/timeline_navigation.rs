use crate::tui::{
    controller::{ControllerEffect, handle_key_action, handle_mouse_input},
    keymap::{KeyAction, Keymap},
    render::{prepare_viewport, render_to_buffer, render_to_text},
    state::{TimelineItem, TuiState},
    tests::{find_text_position, rendered_buffer_text},
    text_interaction::MouseInput,
    theme::TuiTheme,
};
use ratatui::layout::{Position, Size};

#[test]
fn controller_scroll_actions_move_timeline_viewport() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    let initial = state.timeline_scroll_offset();

    assert_eq!(
        handle_key_action(KeyAction::ScrollUp, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_scroll_offset(), initial.saturating_add(5));

    assert_eq!(
        handle_key_action(KeyAction::ScrollDown, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_scroll_offset(), initial);
}

#[test]
fn controller_review_previous_user_input_steps_between_user_turns() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "answer one".to_owned(),
    });
    state.push_timeline_item(TimelineItem::User {
        text: "second".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "answer two".to_owned(),
    });

    assert_eq!(
        handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_review_user_index(), Some(2));

    assert_eq!(
        handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_review_user_index(), Some(0));
}

#[test]
fn controller_follow_latest_clears_every_review_and_scroll_state() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first request".to_owned(),
    });
    state.scroll_timeline_up_by(25);
    state.jump_to_previous_user_input();
    state.follow_latest();

    assert_eq!(state.timeline_scroll_offset(), 0);
    assert_eq!(state.timeline_review_user_index(), None);
}

#[test]
fn cockpit_review_still_jumps_between_user_turns() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first request".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "first answer".to_owned(),
    });
    state.push_timeline_item(TimelineItem::User {
        text: "second request".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "second answer".to_owned(),
    });

    handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state);
    let second = render_to_text(&state, 180, 24);
    handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state);
    let first = render_to_text(&state, 180, 24);
    handle_key_action(KeyAction::Submit, &mut state);
    let bottom = render_to_text(&state, 180, 24);

    assert!(second.contains("second request"));
    assert!(first.contains("first request"));
    assert!(bottom.contains("second answer"));
    assert_eq!(state.timeline_review_user_index(), None);
}

#[test]
fn reply_top_chip_and_shortcut_jump_to_long_final_output_start_beneath_sticky_header() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let size = Size::new(100, 20);
    state.push_timeline_item(TimelineItem::User {
        text: "Summarize the architecture".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Short reply.".to_owned(),
    });
    prepare_viewport(&mut state, size);
    assert!(
        !state.can_jump_to_latest_response_start(),
        "short reply whose start is already visible should not show the jump chip"
    );
    let short_text = render_to_text(&state, size.width, size.height);
    assert!(!short_text.contains("↑ reply"), "{short_text}");

    // Add tool trace and a long final output whose opening anchor scrolls above the viewport.
    for step in 0..8 {
        state.push_timeline_item(TimelineItem::Muted {
            title: "Read".to_owned(),
            detail: format!("src/module_{step}.rs"),
        });
    }
    let long_reply = (1..=45)
        .map(|i| format!("Final line {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    state.push_timeline_item(TimelineItem::Assistant { text: long_reply });
    prepare_viewport(&mut state, size);
    assert!(
        state.can_jump_to_latest_response_start(),
        "overflowing final reply should enable the jump-to-reply-start chip"
    );

    let at_bottom = render_to_buffer(&state, size.width, size.height);
    let at_bottom_text = rendered_buffer_text(&at_bottom);
    assert!(at_bottom_text.contains("↑ reply"), "{at_bottom_text}");
    assert!(at_bottom_text.contains("Final line 45"), "{at_bottom_text}");
    assert!(
        !at_bottom_text.contains("Final line 01"),
        "{at_bottom_text}"
    );

    // Clicking the status bar chip jumps directly to the reply anchor right under the sticky user header.
    let (chip_x, chip_y) =
        find_text_position(&at_bottom, "↑ reply").expect("reply-top chip should render");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(chip_x, chip_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    let _ = handle_mouse_input(
        MouseInput::Up(Position::new(chip_x, chip_y)),
        size,
        &mut state,
    );
    prepare_viewport(&mut state, size);
    assert!(state.is_timeline_detached());
    assert!(
        !state.can_jump_to_latest_response_start(),
        "once landed on the reply anchor, the jump chip should hide"
    );

    let at_reply_start = render_to_text(&state, size.width, size.height);
    assert!(
        at_reply_start.contains("Summarize the architecture"),
        "sticky user prompt should stay visible:\n{at_reply_start}"
    );
    assert!(
        at_reply_start
            .lines()
            .any(|line| line.contains("🎀") && line.contains("Merry") && line.contains('┄')),
        "reply anchor header should land visibly beneath sticky header:\n{at_reply_start}"
    );
    assert!(
        at_reply_start.contains("Final line 01"),
        "first line of final output should be visible:\n{at_reply_start}"
    );
    assert!(
        at_reply_start.contains("[follow]"),
        "follow chip should allow returning to bottom:\n{at_reply_start}"
    );
    assert!(!at_reply_start.contains("↑ reply"), "{at_reply_start}");

    // Returning to bottom restores the chip, and the keyboard shortcut jumps back cleanly.
    state.follow_latest();
    prepare_viewport(&mut state, size);
    assert!(state.can_jump_to_latest_response_start());

    assert_eq!(
        handle_key_action(KeyAction::JumpToLatestResponse, &mut state),
        ControllerEffect::None
    );
    prepare_viewport(&mut state, size);
    let via_shortcut = render_to_text(&state, size.width, size.height);
    assert!(
        via_shortcut
            .lines()
            .any(|line| line.contains("🎀") && line.contains("Merry") && line.contains('┄')),
        "{via_shortcut}"
    );
    assert!(via_shortcut.contains("Final line 01"), "{via_shortcut}");

    // Jumping to a short final reply from scrolled-up history clamps cleanly to bottom
    // without leaving blank space or a stale detached follow banner.
    state.push_timeline_item(TimelineItem::User {
        text: "Quick follow-up".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Done in one line.".to_owned(),
    });
    state.scroll_timeline_up_by(40);
    prepare_viewport(&mut state, size);
    assert!(state.is_timeline_detached());
    assert_eq!(
        handle_key_action(KeyAction::JumpToLatestResponse, &mut state),
        ControllerEffect::None
    );
    prepare_viewport(&mut state, size);
    assert!(
        !state.is_timeline_detached(),
        "short latest reply that already fits at max_scroll should settle in follow mode"
    );
    let short_tail = render_to_text(&state, size.width, size.height);
    assert!(short_tail.contains("Done in one line."), "{short_tail}");
    assert!(!short_tail.contains("↑ reply"), "{short_tail}");
}
