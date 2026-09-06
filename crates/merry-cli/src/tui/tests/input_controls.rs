use crate::tui::{
    controller::{
        ControllerEffect, apply_clipboard_image_completion, handle_key_action, handle_key_event,
    },
    keymap::{KeyAction, KeyBinding, Keymap},
    state::{TimelineItem, TuiState},
    tests::{draft_image, text_submission},
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::InteractiveRunState;

#[test]
fn default_keymap_maps_core_navigation_and_control_keys() {
    let keymap = Keymap::default();

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(KeyAction::SubmitNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL,)),
        Some(KeyAction::SubmitBacklog)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('c'), KeyModifiers::CONTROL,)),
        Some(KeyAction::CancelInputOrQuit)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('j'), KeyModifiers::CONTROL,)),
        Some(KeyAction::InsertNewline)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('v'), KeyModifiers::CONTROL,)),
        Some(KeyAction::PasteImage)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('o'), KeyModifiers::CONTROL,)),
        Some(KeyAction::TogglePlan)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE)),
        Some(KeyAction::Interrupt)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Up, KeyModifiers::NONE)),
        Some(KeyAction::HistoryPrevious)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Down, KeyModifiers::NONE)),
        Some(KeyAction::HistoryNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::PageUp, KeyModifiers::NONE)),
        Some(KeyAction::ScrollUp)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::PageDown, KeyModifiers::NONE)),
        Some(KeyAction::ScrollDown)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        Some(KeyAction::ReviewPreviousUserInput)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        Some(KeyAction::OpenSessionInBrowser)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        None
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        None
    );
}

#[test]
fn configured_image_paste_binding_replaces_the_default() {
    let keymap = Keymap::from_config(&crate::config::TuiKeymapToml {
        paste_image: Some("ctrl+n".to_owned()),
        ..crate::config::TuiKeymapToml::default()
    })
    .expect("configured keymap should validate");

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        Some(KeyAction::PasteImage)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
        None
    );
}

#[test]
fn controller_only_starts_image_paste_from_the_main_composer() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::PasteImage
    );

    state.open_command_palette();
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );
}

#[test]
fn clipboard_image_completion_updates_the_draft_or_reports_a_nonfatal_diagnostic() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("before ");

    apply_clipboard_image_completion(Ok(draft_image(9)), &mut state);
    assert_eq!(state.input_text(), "before [Image #1]");

    apply_clipboard_image_completion(Err("clipboard has no image".to_owned()), &mut state);
    assert_eq!(state.input_text(), "before [Image #1]");
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::Diagnostic { title, body })
            if title == "clipboard_image" && body == "clipboard has no image"
    ));
}

#[test]
fn controller_ctrl_j_inserts_newline_without_submitting() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("first");

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        &mut state,
    );
    state.insert_input_str("second");

    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.input_text(), "first\nsecond");
}

#[test]
fn controller_configured_insert_newline_binding_takes_precedence() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::from_config(&crate::config::TuiKeymapToml {
            insert_newline: Some("ctrl+r".to_owned()),
            ..crate::config::TuiKeymapToml::default()
        })
        .unwrap(),
        TuiTheme::default(),
    );

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        &mut state,
    );

    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.input_text(), "\n");
}

#[test]
fn controller_ctrl_c_clears_input_before_quitting_on_empty_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("draft");

    let first = handle_key_event(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &mut state,
    );

    assert_eq!(first, ControllerEffect::None);
    assert_eq!(state.input_text(), "");

    let second = handle_key_event(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &mut state,
    );

    assert_eq!(second, ControllerEffect::Quit);
}

#[test]
fn controller_ctrl_c_quit_confirmation_resets_after_new_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );

    state.insert_input_str("draft");

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("draft"))
    );
    assert_eq!(state.input_text(), "");

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::Quit
    );
}

#[test]
fn controller_respects_configured_ctrl_c_interrupt_binding() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::from_config(&crate::config::TuiKeymapToml {
            interrupt: Some("ctrl+c".to_owned()),
            ..crate::config::TuiKeymapToml::default()
        })
        .unwrap(),
        TuiTheme::default(),
    );
    state.set_run_state(InteractiveRunState::RunningModel);

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &mut state,
    );

    assert_eq!(effect, ControllerEffect::Interrupt);
}

#[test]
fn configured_navigation_bindings_take_precedence() {
    let keymap = Keymap::from_config(&crate::config::TuiKeymapToml {
        history_previous: Some("ctrl+p".to_owned()),
        history_next: Some("ctrl+n".to_owned()),
        review_previous_user_input: Some("ctrl+u".to_owned()),
        scroll_up: Some("up".to_owned()),
        scroll_down: Some("down".to_owned()),
        resume_suspended: Some("ctrl+r".to_owned()),
        discard_suspended: Some("ctrl+d".to_owned()),
        ..crate::config::TuiKeymapToml::default()
    })
    .unwrap();

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL,)),
        Some(KeyAction::HistoryPrevious)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL,)),
        Some(KeyAction::HistoryNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Up, KeyModifiers::NONE)),
        Some(KeyAction::ScrollUp)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Down, KeyModifiers::NONE)),
        Some(KeyAction::ScrollDown)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('u'), KeyModifiers::CONTROL,)),
        Some(KeyAction::ReviewPreviousUserInput)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('r'), KeyModifiers::CONTROL,)),
        Some(KeyAction::ResumeSuspended)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('d'), KeyModifiers::CONTROL,)),
        Some(KeyAction::DiscardSuspended)
    );
}
