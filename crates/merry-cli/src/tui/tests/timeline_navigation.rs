use crate::tui::{
    controller::{ControllerEffect, handle_key_action},
    keymap::{KeyAction, Keymap},
    render::render_to_text,
    state::{TimelineItem, TuiState},
    theme::TuiTheme,
};
use merry_core::QueuedInputLane;

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
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "answer one".to_owned(),
    });
    state.push_timeline_item(TimelineItem::User {
        text: "second".to_owned(),
        lane: QueuedInputLane::Next,
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
        lane: QueuedInputLane::Next,
    });
    state.scroll_timeline_up_by(25);
    state.jump_to_previous_user_input();
    state.follow_latest();

    assert_eq!(state.timeline_scroll_offset(), 0);
    assert_eq!(state.timeline_review_user_index(), None);
}

#[test]
fn controller_suspended_actions_emit_runtime_effects() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    assert_eq!(
        handle_key_action(KeyAction::ResumeSuspended, &mut state),
        ControllerEffect::ResumeSuspended
    );
    assert_eq!(
        handle_key_action(KeyAction::DiscardSuspended, &mut state),
        ControllerEffect::DiscardSuspended
    );
}

#[test]
fn cockpit_ctrl_u_review_still_jumps_between_user_turns() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
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

    handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state);
    let second = render_to_text(&state, 180, 24);
    handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state);
    let first = render_to_text(&state, 180, 24);
    handle_key_action(KeyAction::SubmitNext, &mut state);
    let bottom = render_to_text(&state, 180, 24);

    assert!(second.contains("second request"));
    assert!(first.contains("first request"));
    assert!(bottom.contains("second answer"));
    assert_eq!(state.timeline_review_user_index(), None);
}
