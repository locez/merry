use crate::{
    config::{ConfiguredProviderKind, ProviderConfigSource},
    tui::{
        controller::{
            ControllerEffect, handle_key_event, handle_mouse_scroll_up, handle_paste_event,
        },
        keymap::Keymap,
        overlay::{Overlay, PaletteCommand},
        provider_overlay::ProviderListItem,
        render::{render_to_buffer, render_to_buffer_and_cursor, render_to_text},
        state::{TimelineItem, TuiState},
        tests::{find_cell_style, find_text_position, rendered_buffer_text},
        theme::TuiTheme,
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Position, Size},
    style::Color,
};

#[test]
fn controller_ctrl_p_opens_searchable_command_palette_and_settings() {
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

    let palette = render_to_text(&state, 100, 30);
    assert!(palette.contains("Commands"));
    assert!(palette.contains("Settings"));
    assert!(palette.contains("Open trajectory in browser"));

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
    assert!(settings.contains("Settings"));
    assert!(settings.contains("Code theme"));
    assert!(settings.contains("Default provider"));
}

#[test]
fn command_palette_renders_categories_as_single_group_headers() {
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

    let palette = render_to_text(&state, 100, 30);

    assert_eq!(palette.matches("Navigation").count(), 1);
    assert_eq!(palette.matches("Runtime").count(), 1);
    assert_eq!(palette.matches("Session").count(), 1);
}

#[test]
fn command_palette_search_has_no_redundant_brand_and_commands_are_indented() {
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

    let buffer = render_to_buffer(&state, 100, 30);
    let (search_x, search_y) =
        find_text_position(&buffer, "Search commands").expect("search placeholder");
    let (group_x, _) = find_text_position(&buffer, "Navigation").expect("group heading");
    let (command_x, _) =
        find_text_position(&buffer, "Open trajectory in browser").expect("group command");

    assert_ne!(buffer[(search_x.saturating_sub(2), search_y)].symbol(), "M");
    assert!(command_x > group_x);
}

#[test]
fn provider_manager_escape_returns_to_command_palette() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_command_palette();
    state.open_provider_manager(vec![ProviderListItem::new(
        "opencode",
        "OpenCode",
        ConfiguredProviderKind::OpenAiCompatible,
        ProviderConfigSource::Managed,
        Some(merry_provider_openai::OpenAiProtocol::ChatCompletions),
        Some("model-a"),
    )]);

    let effect = handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);

    assert_eq!(effect, ControllerEffect::None);
    assert!(matches!(
        state.overlay(),
        Some(crate::tui::overlay::Overlay::CommandPalette(_))
    ));
}

#[test]
fn command_palette_uses_a_magenta_selection_surface() {
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

    let buffer = render_to_buffer(&state, 100, 30);
    let selected = find_cell_style(&buffer, "Settings").expect("selected command should render");

    assert_eq!(selected.bg, Some(Color::Rgb(54, 26, 58)));
    assert_eq!(selected.fg, Some(Color::White));
}

#[test]
fn command_palette_displays_configured_shortcuts_instead_of_stale_defaults() {
    let keymap = Keymap::from_config(&crate::config::TuiKeymapToml {
        open_session_in_browser: Some("ctrl+n".to_owned()),
        ..crate::config::TuiKeymapToml::default()
    })
    .expect("configured keymap should validate");
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        keymap,
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    let palette = render_to_text(&state, 100, 30);
    let trajectory = palette
        .lines()
        .find(|line| line.contains("Open trajectory in browser"))
        .expect("trajectory command should render");

    assert!(trajectory.contains("Ctrl+N"));
    assert!(!trajectory.contains("Ctrl+G"));
}

#[test]
fn command_palette_exposes_open_trajectory_action() {
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
    for character in "open trajectory".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert_eq!(effect, ControllerEffect::OpenSessionInBrowser);
    assert!(state.overlay().is_none());
}

#[test]
fn command_palette_and_cursor_fit_a_narrow_terminal() {
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
    handle_key_event(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        &mut state,
    );

    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 32, 12);
    let text = rendered_buffer_text(&buffer);

    assert!(text.contains("Commands"));
    assert!(text.contains("Settings"));
    assert!(cursor.x < 32);
    assert!(cursor.y < 12);
}

#[test]
fn command_palette_keeps_the_selected_command_visible_in_a_short_terminal() {
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
    let mut selected_quit = false;
    for _ in 0..32 {
        selected_quit = matches!(
            state.overlay(),
            Some(Overlay::CommandPalette(palette))
                if palette
                    .visible_commands()
                    .get(palette.selected())
                    .is_some_and(|command| command.command == PaletteCommand::Quit)
        );
        if selected_quit {
            break;
        }
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }
    assert!(
        selected_quit,
        "Quit should remain reachable in the command palette"
    );

    let palette = render_to_text(&state, 40, 12);

    assert!(palette.contains("Quit Merry"));
}

#[test]
fn paste_is_routed_to_the_command_palette_instead_of_chat_input() {
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

    handle_paste_event("settings", &mut state);

    let palette = render_to_text(&state, 100, 30);
    assert!(palette.contains("settings"));
    assert!(palette.contains("Settings"));
    assert_eq!(state.input_text(), "");
}

#[test]
fn command_palette_blocks_mouse_scroll_from_mutating_the_hidden_timeline() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: (0..30)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    handle_mouse_scroll_up(Position::new(1, 1), Size::new(80, 24), &mut state);

    assert_eq!(state.timeline_scroll_offset(), 0);
}
