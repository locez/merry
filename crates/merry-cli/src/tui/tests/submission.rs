use crate::tui::{
    controller::{ControllerEffect, handle_key_action},
    keymap::{KeyAction, Keymap},
    state::{TimelineItem, TuiState},
    tests::{draft_image, text_submission},
    theme::TuiTheme,
};

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

    let effect = handle_key_action(KeyAction::Submit, &mut state);

    assert_eq!(effect, ControllerEffect::Submit(text_submission("now")));
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

    let effect = handle_key_action(KeyAction::Submit, &mut state);

    assert_eq!(
        effect,
        ControllerEffect::Submit(text_submission("1234\n换行测试"))
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

    let effect = handle_key_action(KeyAction::Submit, &mut state);
    let ControllerEffect::Submit(submission) = effect else {
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
        handle_key_action(KeyAction::Submit, &mut state),
        ControllerEffect::Submit(text_submission("first"))
    );
    state.record_input_history("first");
    state.input_mut().insert_str("second");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut state),
        ControllerEffect::Submit(text_submission("second"))
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
        handle_key_action(KeyAction::Submit, &mut state),
        ControllerEffect::Submit(text_submission("sent"))
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

    let effect = handle_key_action(KeyAction::Submit, &mut state);

    assert_eq!(effect, ControllerEffect::None);
}

#[test]
fn controller_submit_immediately_exits_review_mode_and_submits_input() {
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
        text: "answer".to_owned(),
    });
    state.input_mut().insert_str("draft");
    state.jump_to_previous_user_input();

    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut state),
        ControllerEffect::Submit(text_submission("draft"))
    );
    assert_eq!(state.timeline_review_user_index(), None);
    assert_eq!(state.timeline_scroll_offset(), 0);
    assert!(!state.is_timeline_detached());
    assert_eq!(state.input_text(), "");
}

#[test]
fn controller_submit_exits_scrolled_review_and_jumps_to_bottom_when_idle_or_running() {
    use crate::tui::{
        controller::project_local_effect,
        render::{prepare_viewport, render_to_text},
    };
    use merry_core::InteractiveRunState;
    use ratatui::layout::Size;

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    for index in 0..40 {
        state.push_timeline_item(TimelineItem::Muted {
            title: format!("entry-{index:02}"),
            detail: "scroll history".to_owned(),
        });
    }
    let size = Size::new(80, 24);
    handle_key_action(KeyAction::ScrollUp, &mut state);
    prepare_viewport(&mut state, size);
    assert!(state.is_timeline_detached());
    assert!(state.timeline_anchor().is_some());

    state.insert_input_str("fresh prompt");
    let effect = handle_key_action(KeyAction::Submit, &mut state);
    assert_eq!(
        effect,
        ControllerEffect::Submit(text_submission("fresh prompt"))
    );
    assert!(!state.is_timeline_detached());
    assert_eq!(state.timeline_anchor(), None);
    assert_eq!(state.timeline_scroll_offset(), 0);

    project_local_effect(&effect, &mut state);
    prepare_viewport(&mut state, size);
    let rendered = render_to_text(&state, size.width, size.height);
    assert!(rendered.contains("fresh prompt"));
    assert!(!rendered.contains("Reviewing"));
    assert!(!rendered.contains("New content"));

    // Submitting a follow-up while the model is actively running also exits review immediately.
    state.set_run_state(InteractiveRunState::RunningModel);
    handle_key_action(KeyAction::ScrollUp, &mut state);
    prepare_viewport(&mut state, size);
    assert!(state.is_timeline_detached());

    state.insert_input_str("queued follow-up");
    let effect = handle_key_action(KeyAction::Submit, &mut state);
    assert_eq!(
        effect,
        ControllerEffect::Submit(text_submission("queued follow-up"))
    );
    assert!(!state.is_timeline_detached());
    assert_eq!(state.timeline_anchor(), None);
    assert_eq!(state.timeline_scroll_offset(), 0);
}

#[test]
fn controller_submit_while_following_latest_keeps_turn_scroll_animation() {
    use crate::tui::{controller::project_local_effect, render::prepare_viewport};
    use ratatui::layout::Size;

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    for index in 0..30 {
        state.push_timeline_item(TimelineItem::Muted {
            title: format!("entry-{index:02}"),
            detail: "history".to_owned(),
        });
    }
    let size = Size::new(80, 24);
    prepare_viewport(&mut state, size);
    assert!(!state.is_timeline_detached());

    state.insert_input_str("next turn");
    let effect = handle_key_action(KeyAction::Submit, &mut state);
    assert_eq!(
        effect,
        ControllerEffect::Submit(text_submission("next turn"))
    );
    project_local_effect(&effect, &mut state);
    assert!(state.has_active_timeline_scroll_animation());
}

#[test]
fn controller_empty_enter_and_idle_esc_exit_scrolled_review_to_latest() {
    use crate::tui::render::prepare_viewport;
    use ratatui::layout::Size;

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    for index in 0..40 {
        state.push_timeline_item(TimelineItem::Muted {
            title: format!("entry-{index:02}"),
            detail: "scroll history".to_owned(),
        });
    }
    let size = Size::new(80, 24);
    handle_key_action(KeyAction::ScrollUp, &mut state);
    prepare_viewport(&mut state, size);
    assert!(state.is_timeline_detached());

    // Empty Enter exits scrolled review and jumps back to latest.
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut state),
        ControllerEffect::None
    );
    assert!(!state.is_timeline_detached());
    assert_eq!(state.timeline_scroll_offset(), 0);
    assert_eq!(state.timeline_anchor(), None);

    // Idle Esc also exits scrolled review (outside comment mode).
    handle_key_action(KeyAction::ScrollUp, &mut state);
    prepare_viewport(&mut state, size);
    assert!(state.is_timeline_detached());
    assert_eq!(
        handle_key_action(KeyAction::Interrupt, &mut state),
        ControllerEffect::None
    );
    assert!(!state.is_timeline_detached());
}
