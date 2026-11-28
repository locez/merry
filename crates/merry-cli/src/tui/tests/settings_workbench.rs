use crate::{
    config::{BrandMarkMode, CopyButtonPosition, HeaderItemKind, TextSizingMode},
    tui::{
        controller::{ControllerEffect, handle_key_event, handle_mouse_input, handle_paste_event},
        keymap::{KeyAction, KeyBinding, Keymap},
        overlay::{Overlay, SettingItem},
        preferences::{SubagentLimitBehaviorPref, TuiPreferences, TuiSettingsDefaults},
        provider_overlay::ModelListItem,
        provider_selection::{ModelBindingTarget, ModelPickerTarget, ReasoningBindingTarget},
        render::{prepare_viewport, render_to_buffer, render_to_text},
        settings_overlay::{SettingsCategory, SettingsFocus},
        state::TuiState,
        tests::find_text_position,
        text_interaction::MouseInput,
        theme::{ThemePreset, TuiTheme},
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Size};

fn select_setting(state: &mut TuiState, item: SettingItem) {
    if let Some(Overlay::Settings(settings)) = state.overlay_mut() {
        settings.select_item(item);
    }
}

#[test]
fn wide_settings_escape_steps_back_from_items_to_categories_before_closing() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_settings();
    assert!(matches!(
        state.overlay(),
        Some(Overlay::Settings(settings)) if settings.focus() == SettingsFocus::Categories
    ));

    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        state.overlay(),
        Some(Overlay::Settings(settings)) if settings.focus() == SettingsFocus::Items
    ));

    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(matches!(
        state.overlay(),
        Some(Overlay::Settings(settings)) if settings.focus() == SettingsFocus::Categories
    ));

    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(state.overlay().is_none());
}

#[test]
fn settings_shortcuts_rebind_live_reject_reserved_and_reset() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_settings_category(SettingsCategory::Shortcuts);
    let rendered = render_to_text(&state, 104, 28);
    assert!(rendered.contains("Keyboard shortcuts"));
    assert!(rendered.contains("Command palette"));

    select_setting(&mut state, SettingItem::Shortcut(KeyAction::TogglePlan));

    // Enter starts shortcut capture.
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::None
    );
    let capturing = render_to_text(&state, 104, 28);
    assert!(capturing.contains("Press key combo"));

    // Pressing Ctrl+K rebinds TogglePlan live and persists preferences.
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        &mut state,
    );
    assert!(matches!(
        effect,
        ControllerEffect::PersistPreferences(ref prefs)
            if prefs.keymap.get("toggle_plan").map(String::as_str) == Some("ctrl+k")
    ));
    assert_eq!(
        state
            .keymap()
            .action_for(KeyBinding::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        Some(KeyAction::TogglePlan)
    );
    assert_eq!(state.settings_notice(), Some("Bound to Ctrl+K"));

    // Binding Quit to Ctrl+K cleanly steals Ctrl+K without BTreeMap order reversal.
    select_setting(&mut state, SettingItem::Shortcut(KeyAction::Quit));
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    let steal_effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        &mut state,
    );
    assert!(matches!(
        steal_effect,
        ControllerEffect::PersistPreferences(ref prefs)
            if prefs.keymap.get("quit").map(String::as_str) == Some("ctrl+k")
                && !prefs.keymap.contains_key("toggle_plan")
    ));
    assert_eq!(
        state
            .keymap()
            .action_for(KeyBinding::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        Some(KeyAction::Quit)
    );

    // Alt+X is reserved for Command palette and does not close Settings while capturing.
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    let reserved_effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT),
        &mut state,
    );
    assert_eq!(reserved_effect, ControllerEffect::None);
    assert!(matches!(state.overlay(), Some(Overlay::Settings(_))));
    assert!(
        state
            .settings_notice()
            .is_some_and(|notice| notice.contains("reserved for Command palette"))
    );

    // Resetting restores Quit back to Ctrl+Q, and Esc steps back before closing.
    let reset_effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        reset_effect,
        ControllerEffect::PersistPreferences(ref prefs) if !prefs.keymap.contains_key("quit")
    ));
    assert_eq!(
        state
            .keymap()
            .action_for(KeyBinding::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
        Some(KeyAction::Quit)
    );
    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(matches!(
        state.overlay(),
        Some(Overlay::Settings(settings)) if settings.focus() == SettingsFocus::Categories
    ));
    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(state.overlay().is_none());
}

#[test]
fn settings_workbench_applies_new_ui_and_execution_controls_live_and_resets() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_settings();

    // Animations toggle updates state live.
    select_setting(&mut state, SettingItem::Animations);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(!state.animations_enabled());
    assert_eq!(state.preferences().animations, Some(false));

    // Brand mark & text sizing update live.
    select_setting(&mut state, SettingItem::BrandMark);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        state.preferences().brand_mark,
        Some(BrandMarkMode::Snowflake)
    );

    select_setting(&mut state, SettingItem::TextSizing);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.preferences().text_sizing, Some(TextSizingMode::Auto));

    // Header item toggle shows Tokens pill live, and 'r' resets header items to defaults.
    select_setting(&mut state, SettingItem::HeaderItem(HeaderItemKind::Tokens));
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert!(state.header_items().contains(&HeaderItemKind::Tokens));
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &mut state,
    );
    assert!(!state.header_items().contains(&HeaderItemKind::Tokens));
    assert_eq!(state.preferences().header_items, None);

    // Mouse scroll lines & command output preview update live.
    select_setting(&mut state, SettingItem::MouseScrollLines);
    let initial_scroll = state.mouse_scroll_lines();
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.mouse_scroll_lines(), initial_scroll + 1);

    select_setting(&mut state, SettingItem::ShowSuccessfulCommandOutput);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(!state.show_successful_command_output());

    // Copy button corner position cycles live in wide mode, opens the 4-corner choice
    // submenu in compact mode, and resets to inherited.
    select_setting(&mut state, SettingItem::CopyButtonPosition);
    let _ = handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut state);
    assert_eq!(
        state.copy_button_position(),
        CopyButtonPosition::BottomRight
    );

    // Compact layout exposes all four corners in the Level-3 choice submenu.
    let compact = Size::new(44, 22);
    prepare_viewport(&mut state, compact);
    state.open_settings_category(SettingsCategory::Interaction);
    select_setting(&mut state, SettingItem::CopyButtonPosition);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    let menu_text = render_to_text(&state, compact.width, compact.height);
    assert!(menu_text.contains("Top-left"), "{menu_text}");
    assert!(menu_text.contains("Top-right"), "{menu_text}");
    assert!(menu_text.contains("Bottom-left"), "{menu_text}");
    assert!(menu_text.contains("Bottom-right"), "{menu_text}");
    // Choice list: 0=Inherit, 1=Top-left, 2=Top-right, 3=Bottom-left, 4=Bottom-right (selected).
    let _ = handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut state);
    let choice_effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        choice_effect,
        ControllerEffect::PersistPreferences(ref prefs)
            if prefs.copy_button_position == Some(CopyButtonPosition::BottomLeft)
    ));
    assert_eq!(state.copy_button_position(), CopyButtonPosition::BottomLeft);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.preferences().copy_button_position, None);
    assert_eq!(state.copy_button_position(), CopyButtonPosition::TopRight);
    prepare_viewport(&mut state, Size::new(104, 28));
    state.open_settings();

    // Retained turns & subagent limit behavior emit ApplyRuntimePreferences.
    select_setting(&mut state, SettingItem::RetainedModelTurns);
    let turns_effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        turns_effect,
        ControllerEffect::ApplyRuntimePreferences(ref prefs) if prefs.retained_model_turns == Some(1)
    ));

    select_setting(&mut state, SettingItem::SubagentLimitBehavior);
    let limit_effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        limit_effect,
        ControllerEffect::ApplyRuntimePreferences(ref prefs)
            if prefs.subagent_limit_behavior == Some(SubagentLimitBehaviorPref::Queue)
    ));

    // Max parallel tools applies live without noisy inline [next session] badges.
    select_setting(&mut state, SettingItem::MaxParallelToolCalls);
    let parallel_effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        parallel_effect,
        ControllerEffect::ApplyRuntimePreferences(ref prefs) if prefs.max_parallel_tool_calls.is_some()
    ));
    let rendered_agents = render_to_text(&state, 104, 28);
    assert!(!rendered_agents.contains("[next session]"));

    // Next-session notice appears quietly in the footer only for restart-bound settings.
    select_setting(&mut state, SettingItem::SubagentDefaultProvider);
    let rendered_subagent_default = render_to_text(&state, 104, 28);
    assert!(rendered_subagent_default.contains("Next session ·"));

    // Category navigation via ']' reaches Permissions and Shortcuts.
    if let Some(Overlay::Settings(settings)) = state.overlay_mut() {
        settings.select_category(SettingsCategory::Permissions);
    }
    let rendered_perms = render_to_text(&state, 104, 28);
    assert!(rendered_perms.contains("Sandbox"));
    assert!(rendered_perms.contains("Permission mode"));

    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
        &mut state,
    );
    let rendered_shortcuts = render_to_text(&state, 104, 28);
    assert!(rendered_shortcuts.contains("Keyboard shortcuts"));
    assert!(rendered_shortcuts.contains("Command palette"));
}

#[test]
fn settings_role_bindings_reuse_model_picker_and_configure_subagent_roles() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-5".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.configure_preferences(
        TuiPreferences::default(),
        TuiSettingsDefaults {
            provider_aliases: vec!["compat".to_owned(), "anth".to_owned()],
            provider: Some("compat".to_owned()),
            model: Some("gpt-5".to_owned()),
            ..TuiSettingsDefaults::default()
        },
    );
    state.open_settings();

    // Compaction model opens ModelPicker with ContextCompaction target and applies directly.
    select_setting(&mut state, SettingItem::CompactionModel);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::OpenTargetedModelPicker {
            alias: "compat".to_owned(),
            target: ModelPickerTarget::RoleBinding(ModelBindingTarget::ContextCompaction),
        }
    );
    state.open_targeted_model_picker(
        "compat".to_owned(),
        "Compat".to_owned(),
        vec![ModelListItem::new("gpt-5-mini", None)],
        ModelPickerTarget::RoleBinding(ModelBindingTarget::ContextCompaction),
    );
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::ApplyRoleModel {
            alias: "compat".to_owned(),
            model: "gpt-5-mini".to_owned(),
            target: ModelBindingTarget::ContextCompaction,
        }
    );
    assert!(state.restore_settings_after_role_picker());
    state.apply_role_model_binding(
        &ModelBindingTarget::ContextCompaction,
        "compat",
        "gpt-5-mini",
        None,
    );
    assert_eq!(
        state.setting_value(SettingItem::CompactionProvider),
        "compat"
    );
    assert_eq!(
        state.setting_value(SettingItem::CompactionModel),
        "gpt-5-mini ▸"
    );

    // Compaction & approval review also support custom effort pickers that apply live.
    select_setting(&mut state, SettingItem::CompactionReasoning);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(state.overlay(), Some(Overlay::ReasoningPicker(_))));
    handle_paste_event("high", &mut state);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::ApplyProviderModel {
            alias: "compat".to_owned(),
            model: "gpt-5-mini".to_owned(),
            reasoning_effort: merry::providers::ReasoningEffort::new("high")
                .expect("preset effort should validate"),
            target: ModelPickerTarget::SettingsReasoning(ReasoningBindingTarget::Compaction),
        }
    );
    assert!(state.restore_settings_after_reasoning_picker());
    state.apply_reasoning_binding(
        &ReasoningBindingTarget::Compaction,
        "compat",
        merry::providers::ReasoningEffort::new("high").expect("preset effort should validate"),
    );
    assert_eq!(
        state.setting_value(SettingItem::CompactionReasoning),
        "high ▸"
    );
    assert_eq!(state.settings_notice(), Some("Applied"));

    // Subagent role selector switches from explore -> coder, then picks provider/model/turns/timeout.
    select_setting(&mut state, SettingItem::SubagentRoleSelector);
    assert_eq!(state.selected_settings_role(), "explore");
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(state.selected_settings_role(), "coder");

    select_setting(&mut state, SettingItem::SubagentRoleProvider);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        state
            .preferences()
            .subagent_roles
            .get("coder")
            .and_then(|cfg| cfg.provider.as_deref()),
        Some("anth")
    );

    select_setting(&mut state, SettingItem::SubagentRoleModel);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::OpenTargetedModelPicker {
            alias: "anth".to_owned(),
            target: ModelPickerTarget::RoleBinding(ModelBindingTarget::SubagentRole(
                "coder".to_owned()
            )),
        }
    );
    state.open_targeted_model_picker(
        "anth".to_owned(),
        "Anthropic".to_owned(),
        vec![ModelListItem::new("claude-sonnet", None)],
        ModelPickerTarget::RoleBinding(ModelBindingTarget::SubagentRole("coder".to_owned())),
    );
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::OpenReasoningPicker {
            alias: "anth".to_owned(),
            model: "claude-sonnet".to_owned(),
            target: ModelPickerTarget::RoleBinding(ModelBindingTarget::SubagentRole(
                "coder".to_owned()
            )),
        }
    );
    // Esc in role model picker restores Settings cleanly.
    let _ = handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(matches!(state.overlay(), Some(Overlay::Settings(_))));

    select_setting(&mut state, SettingItem::SubagentRoleMaxTurns);
    let _ = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        state
            .preferences()
            .subagent_roles
            .get("coder")
            .and_then(|cfg| cfg.max_model_turns),
        Some(4)
    );
}

#[test]
fn narrow_mobile_settings_uses_three_level_drill_down_menu_with_touch_targets() {
    let tap = |state: &mut TuiState, size: Size, x: u16, y: u16| -> ControllerEffect {
        let pos = Position::new(x, y);
        let effect = handle_mouse_input(MouseInput::Down(pos), size, state);
        let _ = handle_mouse_input(MouseInput::Up(pos), size, state);
        effect
    };

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mobile = Size::new(44, 22);
    prepare_viewport(&mut state, mobile);
    state.open_settings();

    // Level 1: opens on vertical category list where all 7 categories fit cleanly.
    assert!(matches!(
        state.overlay(),
        Some(Overlay::Settings(settings))
            if settings.is_compact() && settings.focus() == SettingsFocus::Categories
    ));
    let level1 = render_to_text(&state, mobile.width, mobile.height);
    assert!(level1.contains("Appearance"));
    assert!(level1.contains("Permissions"));
    assert!(level1.contains("Shortcuts"));

    // Tapping Appearance drills into Level 2 with a back header and stacked full labels.
    let buf1 = render_to_buffer(&state, mobile.width, mobile.height);
    let (x, y) = find_text_position(&buf1, "Appearance").expect("Appearance row should render");
    tap(&mut state, mobile, x, y);
    assert!(matches!(
        state.overlay(),
        Some(Overlay::Settings(settings))
            if settings.focus() == SettingsFocus::Items
                && settings.category() == SettingsCategory::Appearance
    ));
    let level2 = render_to_text(&state, mobile.width, mobile.height);
    assert!(level2.contains("◂ Settings / Appearance"));
    assert!(level2.contains("Code theme"));
    assert!(level2.contains("UI theme"));

    // Tapping UI theme drills into Level 3 full-width choice menu.
    let buf2 = render_to_buffer(&state, mobile.width, mobile.height);
    let (x, y) = find_text_position(&buf2, "UI theme").expect("UI theme row should render");
    tap(&mut state, mobile, x, y);
    let level3 = render_to_text(&state, mobile.width, mobile.height);
    assert!(level3.contains("◂ Appearance / UI theme"));
    assert!(level3.contains("Tokyo Night"));

    // Tapping Tokyo Night applies live and returns cleanly to Level 2.
    let buf3 = render_to_buffer(&state, mobile.width, mobile.height);
    let (x, y) =
        find_text_position(&buf3, "Tokyo Night").expect("Tokyo Night choice should render");
    let effect = tap(&mut state, mobile, x, y);
    assert!(matches!(
        effect,
        ControllerEffect::PersistPreferences(ref prefs)
            if prefs.theme_preset == Some(ThemePreset::TokyoNight)
    ));
    assert!(matches!(
        state.overlay(),
        Some(Overlay::Settings(settings))
            if settings.choice_menu().is_none() && settings.focus() == SettingsFocus::Items
    ));

    // Tapping back header returns to Level 1; Esc at Level 1 closes Settings.
    let buf_back = render_to_buffer(&state, mobile.width, mobile.height);
    let (x, y) = find_text_position(&buf_back, "◂ Settings").expect("back header should render");
    tap(&mut state, mobile, x, y);
    assert!(matches!(
        state.overlay(),
        Some(Overlay::Settings(settings)) if settings.focus() == SettingsFocus::Categories
    ));
    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(state.overlay().is_none());
}
