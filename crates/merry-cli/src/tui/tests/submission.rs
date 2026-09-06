use crate::tui::{
    controller::{ControllerEffect, handle_key_action},
    keymap::{KeyAction, Keymap},
    state::{TimelineItem, TuiState},
    tests::{draft_image, text_submission},
    theme::TuiTheme,
};
use merry_core::QueuedInputLane;

#[test]
fn controller_submit_next_takes_input_text() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.input_mut().insert_char('n');
    state.input_mut().insert_char('o');
    state.input_mut().insert_char('w');

    let effect = handle_key_action(KeyAction::SubmitNext, &mut state);

    assert_eq!(effect, ControllerEffect::SubmitNext(text_submission("now")));
    assert_eq!(state.input_text(), "");
}

#[test]
fn controller_submit_next_preserves_multiline_input_text() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("1234");
    state.insert_input_newline();
    state.insert_input_str("换行测试");

    let effect = handle_key_action(KeyAction::SubmitNext, &mut state);

    assert_eq!(
        effect,
        ControllerEffect::SubmitNext(text_submission("1234\n换行测试"))
    );
}

#[test]
fn controller_submit_carries_images_and_records_text_only_history() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("inspect ");
    state
        .input_mut()
        .insert_image(draft_image(7))
        .expect("image should insert");

    let effect = handle_key_action(KeyAction::SubmitNext, &mut state);
    let ControllerEffect::SubmitNext(submission) = effect else {
        panic!("image submission should produce a next-lane effect");
    };

    assert_eq!(submission.text, "inspect [Image #1]");
    assert_eq!(submission.history_text, "inspect ");
    assert_eq!(submission.images.len(), 1);
    assert_eq!(submission.images[0].label(), "[Image #1]");
    assert_eq!(submission.images[0].png_bytes()[8], 7);
    assert!(state.input_text().is_empty());
    state.record_input_history(&submission.history_text);

    assert_eq!(
        handle_key_action(KeyAction::HistoryPrevious, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "inspect ");
}

#[test]
fn controller_submit_records_shell_like_input_history() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.input_mut().insert_str("first");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("first"))
    );
    state.record_input_history("first");
    state.input_mut().insert_str("second");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("second"))
    );
    state.record_input_history("second");

    assert_eq!(
        handle_key_action(KeyAction::HistoryPrevious, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "second");
    assert_eq!(
        handle_key_action(KeyAction::HistoryPrevious, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "first");
    assert_eq!(
        handle_key_action(KeyAction::HistoryNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "second");
    assert_eq!(
        handle_key_action(KeyAction::HistoryNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "");
}

#[test]
fn controller_history_restores_unsent_draft() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.input_mut().insert_str("sent");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("sent"))
    );
    state.record_input_history("sent");
    state.input_mut().insert_str("draft");

    assert_eq!(
        handle_key_action(KeyAction::HistoryPrevious, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "sent");
    assert_eq!(
        handle_key_action(KeyAction::HistoryNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "draft");
}

#[test]
fn controller_empty_submit_does_nothing() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    let effect = handle_key_action(KeyAction::SubmitNext, &mut state);

    assert_eq!(effect, ControllerEffect::None);
}

#[test]
fn controller_submit_exits_review_mode_before_submitting_input() {
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
        text: "answer".to_owned(),
    });
    state.input_mut().insert_str("draft");
    state.jump_to_previous_user_input();

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_review_user_index(), None);
    assert_eq!(state.timeline_scroll_offset(), 0);
    assert_eq!(state.input_text(), "draft");

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("draft"))
    );
}
