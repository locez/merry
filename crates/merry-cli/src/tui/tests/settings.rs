use crate::tui::{
    controller::{ControllerEffect, handle_key_event, handle_paste_event},
    keymap::Keymap,
    overlay::SettingItem,
    preferences::{CodeTheme, TuiPreferences, TuiSettingsDefaults},
    render::{render_to_buffer, render_to_buffer_and_cursor, render_to_text},
    state::{TimelineItem, TuiState},
    tests::{find_cell_style, find_text_position},
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use std::collections::BTreeMap;

#[test]
fn settings_cycles_code_theme_without_leaking_keys_to_chat_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );

    let settings = render_to_text(&state, 100, 30);
    assert!(settings.contains("Catppuccin Mocha"));
    assert_eq!(state.input_text(), "");
    assert!(matches!(
        effect,
        ControllerEffect::PersistPreferences(preferences)
            if preferences.code_theme == CodeTheme::CatppuccinMocha
    ));
}

#[test]
fn settings_reasoning_change_applies_to_the_current_runtime() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.configure_preferences(
        TuiPreferences::default(),
        TuiSettingsDefaults {
            provider: Some("compat".to_owned()),
            ..TuiSettingsDefaults::default()
        },
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..3 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );

    assert!(matches!(
        effect,
        ControllerEffect::ApplyRuntimePreferences(preferences)
            if preferences
                .reasoning_effort_for_provider("compat")
                .map(|effort| effort.as_str())
                == Some("minimal")
    ));
    assert_eq!(state.settings_notice(), Some("Applied"));

    for expected in ["low", "medium", "high", "xhigh", "max", "ultra"] {
        let effect = handle_key_event(
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &mut state,
        );
        assert!(matches!(
            effect,
            ControllerEffect::ApplyRuntimePreferences(preferences)
                if preferences
                    .reasoning_effort_for_provider("compat")
                    .map(|effort| effort.as_str())
                    == Some(expected)
        ));
    }
}

#[test]
fn settings_reasoning_editor_accepts_custom_provider_value() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.configure_preferences(
        TuiPreferences::default(),
        TuiSettingsDefaults {
            provider: Some("compat".to_owned()),
            ..TuiSettingsDefaults::default()
        },
    );
    state.open_settings();
    for _ in 0..3 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::None
    );
    assert!(state.settings_reasoning_editor().is_some());
    handle_paste_event("model-specific", &mut state);

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert!(matches!(
        effect,
        ControllerEffect::ApplyRuntimePreferences(preferences)
            if preferences
                .reasoning_effort_for_provider("compat")
                .map(|effort| effort.as_str())
                == Some("model-specific")
    ));
    assert_eq!(state.settings_notice(), Some("Applied"));
}

#[test]
fn settings_reasoning_display_and_editor_follow_the_selected_provider() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut preferences = TuiPreferences::default();
    preferences.provider = Some("alt".to_owned());
    let defaults = TuiSettingsDefaults {
        provider_aliases: vec!["compat".to_owned(), "alt".to_owned()],
        provider: Some("compat".to_owned()),
        reasoning_efforts: BTreeMap::from([(
            "alt".to_owned(),
            merry_llm::ReasoningEffort::new("max ultra").expect("custom effort should validate"),
        )]),
        ..TuiSettingsDefaults::default()
    };
    state.configure_preferences(preferences, defaults);

    assert_eq!(
        state.setting_value(SettingItem::ReasoningEffort),
        "Inherit (max ultra)"
    );
    state.open_settings();
    state.begin_settings_reasoning_edit();
    assert_eq!(
        state
            .settings_reasoning_editor()
            .expect("reasoning editor should open")
            .text(),
        "max ultra"
    );
}

#[test]
fn settings_context_window_editor_applies_to_the_current_runtime() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_settings();
    for _ in 0..4 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    handle_paste_event("128k", &mut state);
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert!(matches!(
        effect,
        ControllerEffect::ApplyRuntimePreferences(preferences)
            if preferences.context_window_tokens == Some(128_000)
    ));
    assert!(render_to_text(&state, 100, 30).contains("128k"));
    assert_eq!(state.settings_notice(), Some("Applied"));
}

#[test]
fn settings_context_window_editor_rejects_zero() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_settings();
    for _ in 0..4 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    handle_paste_event("0", &mut state);

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.preferences().context_window_tokens, None);
    assert!(
        state
            .settings_notice()
            .is_some_and(|notice| notice.contains("positive token count"))
    );
}

#[test]
fn settings_compaction_change_applies_to_the_current_runtime() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..5 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );

    assert!(matches!(
        effect,
        ControllerEffect::ApplyRuntimePreferences(preferences)
            if preferences.auto_compaction_enabled == Some(true)
    ));
    assert_eq!(state.settings_notice(), Some("Applied"));
}

#[test]
fn settings_do_not_describe_runtime_changes_as_next_session() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    let settings = render_to_text(&state, 100, 30);

    assert!(!settings.contains("next session"));
}

#[test]
fn code_theme_setting_applies_to_existing_code_blocks_immediately() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "```python\ndef greet():\n    return 'hello'\n```".to_owned(),
    });
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    let buffer = render_to_buffer(&state, 80, 18);
    let keyword = find_cell_style(&buffer, "def").expect("python keyword should render");

    assert_eq!(keyword.fg, Some(Color::Rgb(203, 166, 247)));
}

#[test]
fn shortcuts_opened_from_settings_return_to_settings() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..9 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert!(render_to_text(&state, 100, 30).contains("Command palette"));

    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);

    let settings = render_to_text(&state, 100, 30);
    assert!(settings.contains("Code theme"));
    assert!(settings.contains("Default provider"));
}

#[test]
fn settings_model_editor_owns_the_visible_cursor() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..2 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for character in "custom-model".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }

    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 100, 30);
    let (_, editor_row) =
        find_text_position(&buffer, "custom-model").expect("model editor should render");

    assert_eq!(cursor.y, editor_row);
    assert!(cursor.x > 30);
}

#[test]
fn settings_keep_the_selected_row_visible_in_a_short_terminal() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..9 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    let settings = render_to_text(&state, 40, 12);

    assert!(settings.contains("Keyboard shortcuts"));
}
