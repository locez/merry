use crate::{
    cli::ExplicitCliFlags,
    coding::{AutoFallback, PermissionMode, SandboxMode},
    tui::{
        controller::{ControllerEffect, handle_key_event},
        keymap::Keymap,
        overlay::{Overlay, SettingItem},
        preferences::{TuiPreferences, TuiSettingsDefaults},
        render::render_to_text,
        resolve_effective_auto_fallback, resolve_effective_permission_mode,
        resolve_effective_sandbox,
        settings_overlay::SettingsCategory,
        state::TuiState,
        theme::TuiTheme,
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn select_setting(state: &mut TuiState, item: SettingItem) {
    if let Some(Overlay::Settings(settings)) = state.overlay_mut() {
        settings.select_item(item);
    }
}

#[test]
fn settings_permissions_toggles_respect_cli_flag_precedence_and_keep_config_only_fields_in_config_toml()
 {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-5".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.configure_preferences(
        TuiPreferences::default(),
        TuiSettingsDefaults::default().with_explicit_cli_flags(ExplicitCliFlags {
            sandbox: Some(SandboxMode::Off),
            permission_mode: None,
            auto_fallback: None,
        }),
    );
    state.open_settings();

    // Explicit CLI flag displays [CLI flag] badge and wins over TUI override.
    assert!(
        state
            .setting_value(SettingItem::PermissionSandbox)
            .contains("[CLI flag]")
    );
    select_setting(&mut state, SettingItem::PermissionSandbox);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        state.preferences().permissions.sandbox,
        Some(SandboxMode::Workspace)
    );
    assert_eq!(
        resolve_effective_sandbox(
            ExplicitCliFlags {
                sandbox: Some(SandboxMode::Off),
                permission_mode: None,
                auto_fallback: None,
            },
            state.preferences(),
            SandboxMode::Workspace,
        ),
        SandboxMode::Off
    );
    assert_eq!(
        resolve_effective_sandbox(
            ExplicitCliFlags::default(),
            state.preferences(),
            SandboxMode::Off,
        ),
        SandboxMode::Workspace
    );

    // Permission mode & auto fallback apply live and override config defaults when no CLI flag.
    select_setting(&mut state, SettingItem::PermissionMode);
    let mode_effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        mode_effect,
        ControllerEffect::ApplyRuntimePreferences(_)
    ));
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        resolve_effective_permission_mode(
            ExplicitCliFlags::default(),
            state.preferences(),
            PermissionMode::Auto,
        ),
        PermissionMode::Ask
    );

    select_setting(&mut state, SettingItem::PermissionAutoFallback);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        resolve_effective_auto_fallback(
            ExplicitCliFlags::default(),
            state.preferences(),
            AutoFallback::Ask,
        ),
        AutoFallback::Deny
    );

    // Host integrations & network toggle cleanly and reset with 'r'.
    select_setting(&mut state, SettingItem::PermissionSshAgent);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.preferences().permissions.ssh_agent, Some(true));
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.preferences().permissions.ssh_agent, None);

    // Permissions pane keeps only concise toggles and points list/table config to config.toml.
    if let Some(Overlay::Settings(settings)) = state.overlay_mut() {
        settings.select_category(SettingsCategory::Permissions);
    }
    let rendered = render_to_text(&state, 104, 24);
    assert!(rendered.contains("Sandbox"));
    assert!(rendered.contains("Session D-Bus"));
    assert!(rendered.contains("config.toml"));
    assert!(!rendered.contains("Read-only paths"));
    assert!(!rendered.contains("Review: allow rules"));
}
