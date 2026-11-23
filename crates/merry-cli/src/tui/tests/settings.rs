use crate::{
    config::HeaderItemKind,
    tui::{
        controller::{ControllerEffect, handle_key_event, handle_paste_event},
        keymap::Keymap,
        overlay::{Overlay, SettingItem},
        preferences::{CodeTheme, TuiPreferences, TuiSettingsDefaults},
        render::{render_to_buffer, render_to_buffer_and_cursor, render_to_text},
        state::{TimelineItem, TuiState},
        tests::{find_cell_style, find_text_position},
        theme::{SemanticColor, ThemePreset, TuiTheme},
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use std::collections::BTreeMap;

fn open_settings_via_palette(state: &mut TuiState) {
    handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT), state);
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            state,
        );
    }
    handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), state);
}

fn select_setting(state: &mut TuiState, item: SettingItem) {
    if let Some(Overlay::Settings(settings)) = state.overlay_mut() {
        settings.select_item(item);
    }
}

#[test]
fn settings_cycles_code_theme_without_leaking_keys_to_chat_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    open_settings_via_palette(&mut state);
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );

    let settings = render_to_text(&state, 100, 30);
    assert!(settings.contains("CATEGORIES"));
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
    open_settings_via_palette(&mut state);
    select_setting(&mut state, SettingItem::ReasoningEffort);

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
fn settings_reasoning_picker_accepts_custom_provider_value_and_filters_presets() {
    use crate::tui::provider_selection::{ModelPickerTarget, ReasoningBindingTarget};

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
            model: Some("gpt-5".to_owned()),
            ..TuiSettingsDefaults::default()
        },
    );
    state.open_settings();
    select_setting(&mut state, SettingItem::ReasoningEffort);

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::None
    );
    assert!(matches!(state.overlay(), Some(Overlay::ReasoningPicker(_))));
    handle_paste_event("model-specific", &mut state);

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert_eq!(
        effect,
        ControllerEffect::ApplyProviderModel {
            alias: "compat".to_owned(),
            model: "gpt-5".to_owned(),
            reasoning_effort: merry_llm::ReasoningEffort::new("model-specific")
                .expect("custom effort should validate"),
            target: ModelPickerTarget::SettingsReasoning(ReasoningBindingTarget::ActiveProvider),
        }
    );
    assert!(state.restore_settings_after_reasoning_picker());
    state.apply_reasoning_binding(
        &ReasoningBindingTarget::ActiveProvider,
        "compat",
        merry_llm::ReasoningEffort::new("model-specific").expect("custom effort should validate"),
    );
    assert_eq!(
        state
            .preferences()
            .reasoning_effort_for_provider("compat")
            .map(|effort| effort.as_str()),
        Some("model-specific")
    );
    assert_eq!(state.settings_notice(), Some("Applied"));
}

#[test]
fn settings_reasoning_display_and_picker_follow_the_selected_provider() {
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
        "Inherit (max ultra) ▸"
    );
    state.open_settings();
    select_setting(&mut state, SettingItem::ReasoningEffort);
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    match state.overlay() {
        Some(Overlay::ReasoningPicker(picker)) => assert_eq!(picker.query(), "max ultra"),
        other => panic!("expected ReasoningPicker overlay, got {other:?}"),
    }
    // Esc returns cleanly to Settings.
    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(matches!(state.overlay(), Some(Overlay::Settings(_))));
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
    select_setting(&mut state, SettingItem::ContextWindow);

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
    select_setting(&mut state, SettingItem::ContextWindow);
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
    open_settings_via_palette(&mut state);
    select_setting(&mut state, SettingItem::AutoCompaction);

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
    assert!(!render_to_text(&state, 100, 30).contains("next session"));
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
    open_settings_via_palette(&mut state);
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT),
        &mut state,
    );

    let buffer = render_to_buffer(&state, 80, 18);
    let keyword = find_cell_style(&buffer, "def").expect("python keyword should render");

    assert_eq!(keyword.fg, Some(Color::Rgb(203, 166, 247)));
}

#[test]
fn settings_active_model_opens_searchable_editable_picker_and_context_window_owns_cursor() {
    use crate::tui::{provider_overlay::ModelListItem, provider_selection::ModelPickerTarget};

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.configure_preferences(
        TuiPreferences::default(),
        TuiSettingsDefaults {
            provider_aliases: vec!["compat".to_owned()],
            provider: Some("compat".to_owned()),
            model: Some("gpt-5".to_owned()),
            ..TuiSettingsDefaults::default()
        },
    );
    open_settings_via_palette(&mut state);
    select_setting(&mut state, SettingItem::ActiveModel);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::OpenTargetedModelPicker {
            alias: "compat".to_owned(),
            target: ModelPickerTarget::SettingsActiveModel,
        }
    );

    // Searchable dropdown with API models + custom typed model even when partial match exists.
    state.open_targeted_model_picker(
        "compat".to_owned(),
        "Compat".to_owned(),
        vec![
            ModelListItem::new("gpt-5", None),
            ModelListItem::new("gpt-5-mini", None),
        ],
        ModelPickerTarget::SettingsActiveModel,
    );
    for character in "gpt-5".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 100, 30);
    let (_, search_row) =
        find_text_position(&buffer, "gpt-5").expect("model picker search input should render");
    assert_eq!(cursor.y, search_row);

    // Typing a custom suffix still lets the user select "Use custom: gpt-5-custom".
    for character in "-custom".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::OpenReasoningPicker {
            alias: "compat".to_owned(),
            model: "gpt-5-custom".to_owned(),
            target: ModelPickerTarget::SettingsActiveModel,
        }
    );
    // Esc returns cleanly to Settings.
    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(matches!(state.overlay(), Some(Overlay::Settings(_))));

    // Inline editor (Context window) also owns the visible cursor.
    select_setting(&mut state, SettingItem::ContextWindow);
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for character in "128k".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 100, 30);
    let (_, editor_row) =
        find_text_position(&buffer, "128k").expect("context window editor should render");
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
    open_settings_via_palette(&mut state);
    select_setting(&mut state, SettingItem::HeaderItem(HeaderItemKind::Tokens));

    let settings = render_to_text(&state, 50, 12);

    assert!(settings.contains("Header: tokens"));
}

#[test]
fn ui_theme_and_transparent_bg_update_active_theme_live_and_reset() {
    let custom_configured = TuiTheme::from_config_opaque(&crate::config::TuiThemeToml {
        header_bg: Some("#123456".to_owned()),
        ..crate::config::TuiThemeToml::default()
    })
    .expect("configured theme should parse");
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        custom_configured.clone(),
    );
    state.open_settings();
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    assert_eq!(state.selected_setting(), Some(SettingItem::UiTheme));

    // Cycling right from Sakura (default) moves to ANSI.
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        effect,
        ControllerEffect::PersistPreferences(preferences)
            if preferences.theme_preset == Some(ThemePreset::Ansi)
    ));
    assert_eq!(state.theme(), &TuiTheme::from_preset(ThemePreset::Ansi));
    assert_eq!(state.theme().bg_color(SemanticColor::Background), None);

    // Resetting restores the inherited configured Sakura + custom header_bg theme.
    let reset_effect = handle_key_event(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        reset_effect,
        ControllerEffect::PersistPreferences(preferences)
            if preferences.theme_preset.is_none()
    ));
    assert_eq!(state.theme(), &custom_configured);
    assert_eq!(
        state.setting_value(SettingItem::UiTheme),
        "Inherit (Sakura)"
    );

    // Toggling Transparent bg clears Background and StatusBackground live on Sakura.
    handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    assert_eq!(
        state.selected_setting(),
        Some(SettingItem::TransparentBackground)
    );
    assert_eq!(
        state.theme().bg_color(SemanticColor::Background),
        Some(Color::Rgb(35, 38, 39))
    );
    let transparent_effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        transparent_effect,
        ControllerEffect::PersistPreferences(preferences)
            if preferences.transparent_background == Some(true)
    ));
    assert_eq!(state.theme().bg_color(SemanticColor::Background), None);
    assert_eq!(
        state.theme().bg_color(SemanticColor::StatusBackground),
        None
    );
    assert_eq!(
        state.theme().bg_color(SemanticColor::HeaderBackground),
        Some(Color::Rgb(0x12, 0x34, 0x56))
    );

    // Toggling right again sets explicit Off, restoring Sakura's opaque background.
    let opaque_effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        opaque_effect,
        ControllerEffect::PersistPreferences(preferences)
            if preferences.transparent_background == Some(false)
    ));
    assert_eq!(
        state.theme().bg_color(SemanticColor::Background),
        Some(Color::Rgb(35, 38, 39))
    );
}
