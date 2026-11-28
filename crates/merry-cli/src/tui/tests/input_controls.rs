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
use merry::events::InteractiveRunState;

#[test]
fn default_keymap_maps_core_navigation_and_control_keys() {
    let keymap = Keymap::default();

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(KeyAction::Submit)
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
        keymap.action_for(KeyBinding::new(KeyCode::Char('x'), KeyModifiers::ALT,)),
        Some(KeyAction::OpenCommandPanel)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('p'), KeyModifiers::ALT,)),
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
        keymap.action_for(KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        Some(KeyAction::HistoryPrevious)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Down, KeyModifiers::NONE)),
        Some(KeyAction::HistoryNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
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
        keymap.action_for(KeyBinding::new(KeyCode::Char('u'), KeyModifiers::ALT)),
        Some(KeyAction::ReviewPreviousUserInput)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        None
    );
    #[cfg(feature = "web")]
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('g'), KeyModifiers::ALT)),
        Some(KeyAction::OpenSessionInBrowser)
    );
    #[cfg(not(feature = "web"))]
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('g'), KeyModifiers::ALT)),
        None
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('c'), KeyModifiers::ALT)),
        Some(KeyAction::ToggleComment)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        Some(KeyAction::HistorySearch)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
        None
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        None
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
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
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER),
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
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER),
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
        handle_key_action(KeyAction::Submit, &mut state),
        ControllerEffect::Submit(text_submission("draft"))
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
}

#[test]
fn ctrl_r_searches_history_and_enter_keeps_the_line() {
    use crate::tui::render::render_to_text;

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.record_input_history("older cargo");
    state.record_input_history("newer cargo test");
    state.insert_input_str("keep me");

    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(handle_key_event(ctrl_r, &mut state), ControllerEffect::None);
    // Draft stays untouched while the search filter and candidate list are shown.
    assert_eq!(state.input_text(), "keep me");
    assert_eq!(state.history_search().unwrap().query(), "");
    assert_eq!(state.history_search().unwrap().selected_pos(), 0);
    assert!(state.interaction_status_text().contains("Ctrl+R select"));
    let open_view = render_to_text(&state, 90, 14);
    assert!(open_view.contains("Search history · 1/2"), "{open_view}");
    assert!(open_view.contains("> newer cargo test"), "{open_view}");
    assert!(open_view.contains("older cargo"), "{open_view}");
    assert!(open_view.contains("Type to filter history"), "{open_view}");

    // Ctrl+R / Down steps to older match; Ctrl+S / Up steps back to newer match.
    assert_eq!(handle_key_event(ctrl_r, &mut state), ControllerEffect::None);
    assert_eq!(state.history_search().unwrap().selected_pos(), 1);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(state.history_search().unwrap().selected_pos(), 0);

    // Typing edits the filter query directly in the composer (never the draft).
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(state.history_search().unwrap().query(), "z");
    assert!(state.interaction_status_text().contains("No history match"));
    let empty_view = render_to_text(&state, 90, 14);
    assert!(
        empty_view.contains("No matching history entries"),
        "{empty_view}"
    );
    assert!(empty_view.contains("❯ z"), "{empty_view}");

    // Backspace edits the filter query cleanly and restores matches.
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(state.history_search().unwrap().query(), "");

    // Filter with "test" (case-insensitive) and verify Ctrl+A selects the filter, not draft.
    for ch in "TEST".chars() {
        assert_eq!(
            handle_key_event(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                &mut state
            ),
            ControllerEffect::None
        );
    }
    assert_eq!(state.history_search().unwrap().matches().len(), 1);
    assert!(state.interaction_status_text().contains("History · TEST"));

    // Ctrl+A + Backspace clears the whole filter query while keeping search open.
    handle_key_event(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        &mut state,
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        &mut state,
    );
    assert!(state.history_search_active());
    assert_eq!(state.history_search().unwrap().query(), "");

    // Type 't' to match "newer cargo test" and press Enter to load into composer.
    handle_key_event(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "newer cargo test");
    assert!(!state.history_search_active());

    // Down restores the original draft.
    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "keep me");
}

#[test]
fn esc_and_empty_backspace_cancel_history_search() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.record_input_history("alpha");
    state.insert_input_str("draft");

    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    handle_key_event(ctrl_r, &mut state);
    assert!(state.history_search_active());
    assert_eq!(state.input_text(), "draft");
    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(!state.history_search_active());
    assert_eq!(state.input_text(), "draft");

    handle_key_event(ctrl_r, &mut state);
    handle_key_event(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        &mut state,
    );
    assert!(!state.history_search_active());
    assert_eq!(state.input_text(), "draft");

    handle_key_event(ctrl_r, &mut state);
    assert_eq!(handle_key_event(ctrl_c, &mut state), ControllerEffect::None);
    assert!(!state.history_search_active());
    assert_eq!(state.input_text(), "draft");
    assert_eq!(handle_key_event(ctrl_c, &mut state), ControllerEffect::None);
    assert_eq!(state.input_text(), "");
}

#[cfg(not(feature = "web"))]
#[test]
fn open_session_in_browser_config_validates_syntax_without_binding_when_web_is_disabled() {
    let keymap = Keymap::from_config(&crate::config::TuiKeymapToml {
        open_session_in_browser: Some("ctrl+g".to_owned()),
        ..crate::config::TuiKeymapToml::default()
    })
    .expect("open_session_in_browser should remain accepted by config");
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        None
    );

    let error = Keymap::from_config(&crate::config::TuiKeymapToml {
        open_session_in_browser: Some("invalid+chord".to_owned()),
        ..crate::config::TuiKeymapToml::default()
    })
    .expect_err("invalid key syntax should still fail validation");
    assert!(error.to_string().contains("unsupported TUI key binding"));
}
