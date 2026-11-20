use crate::{
    config::{ConfiguredProviderKind, ProviderConfigSource},
    tui::{
        controller::{ControllerEffect, handle_mouse_input},
        keymap::Keymap,
        overlay::Overlay,
        provider_overlay::{ModelListItem, ModelPickerTarget, ProviderListItem},
        render::{prepare_viewport, render_to_buffer, render_to_text},
        state::{TimelineItem, TuiState},
        tests::rendered_buffer_text,
        text_interaction::MouseInput,
        theme::TuiTheme,
    },
};
use merry_core::{InteractiveRunState, QueuedInputLane};
use merry_provider_openai::OpenAiProtocol;
use ratatui::{buffer::Buffer, layout::Position, layout::Size};

fn state() -> TuiState {
    TuiState::new(
        "/repo".into(),
        "click-model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

fn draw(state: &mut TuiState, size: Size) -> Buffer {
    prepare_viewport(state, size);
    render_to_buffer(state, size.width, size.height)
}

fn point(buffer: &Buffer, text: &str) -> Position {
    let (column, row) = find_text_position(buffer, text)
        .unwrap_or_else(|| panic!("missing {text}:\n{}", rendered_buffer_text(buffer)));
    Position::new(column, row)
}

fn find_text_position(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let area = buffer.area;
    for y in area.y..area.y + area.height {
        let mut row = String::new();
        for x in area.x..area.x + area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        if let Some(byte_index) = row.find(needle) {
            let x = row[..byte_index].chars().count();
            return Some((u16::try_from(x).ok()?, y));
        }
    }
    None
}

fn click(state: &mut TuiState, size: Size, position: Position) -> ControllerEffect {
    let effect = handle_mouse_input(MouseInput::Down(position), size, state);
    assert_eq!(
        handle_mouse_input(MouseInput::Up(position), size, state),
        ControllerEffect::None
    );
    effect
}

#[test]
fn status_commands_chip_opens_the_palette_and_a_row_click_opens_settings() {
    let mut state = state();
    let size = Size::new(80, 24);
    let buffer = draw(&mut state, size);

    assert_eq!(
        click(&mut state, size, point(&buffer, "[commands]")),
        ControllerEffect::None
    );
    assert!(matches!(state.overlay(), Some(Overlay::CommandPalette(_))));

    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&mut state, size, point(&buffer, "Settings")),
        ControllerEffect::None
    );
    assert!(matches!(state.overlay(), Some(Overlay::Settings(_))));

    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&mut state, size, point(&buffer, "Keyboard shortcuts")),
        ControllerEffect::None
    );
    assert!(matches!(state.overlay(), Some(Overlay::Shortcuts(_))));
}

#[test]
fn header_model_click_opens_the_model_picker_for_the_active_provider() {
    let mut state = state();
    let mut preferences = state.preferences().clone();
    preferences.provider = Some("opencode".to_owned());
    state.replace_preferences(preferences);
    let size = Size::new(100, 24);
    let buffer = draw(&mut state, size);

    assert_eq!(
        click(&mut state, size, point(&buffer, "click-model")),
        ControllerEffect::OpenModelPicker("opencode".to_owned())
    );
}

#[test]
fn header_model_click_opens_providers_when_no_provider_is_selected() {
    let mut state = state();
    let size = Size::new(100, 24);
    let buffer = draw(&mut state, size);

    assert_eq!(
        click(&mut state, size, point(&buffer, "click-model")),
        ControllerEffect::OpenProviderManager
    );
}

#[test]
fn stop_and_follow_chips_use_the_same_actions_as_the_keyboard() {
    let mut state = state();
    state.push_timeline_item(TimelineItem::User {
        text: "previous question".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.jump_to_previous_user_input();
    state.set_run_state(InteractiveRunState::RunningModel);
    let size = Size::new(100, 24);
    let buffer = draw(&mut state, size);

    assert_eq!(
        click(&mut state, size, point(&buffer, "[stop]")),
        ControllerEffect::Interrupt
    );

    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&mut state, size, point(&buffer, "[follow]")),
        ControllerEffect::None
    );
    assert!(!state.is_timeline_reviewing());
}

#[test]
fn completion_row_click_accepts_a_slash_command() {
    let mut state = state();
    state.insert_input_str("/he");
    let size = Size::new(80, 24);
    let buffer = draw(&mut state, size);

    assert_eq!(
        click(&mut state, size, point(&buffer, "/help")),
        ControllerEffect::None
    );
    let rendered = render_to_text(&state, size.width, size.height);
    assert!(rendered.contains("Command help"));
    assert!(state.input_text().is_empty());
}

#[test]
fn permission_allow_and_dialog_close_are_clickable() {
    let mut state = state();
    state.open_permission_review("approval-9".to_owned(), "action: cargo test".to_owned());
    let size = Size::new(80, 24);
    let buffer = draw(&mut state, size);

    assert_eq!(
        click(&mut state, size, point(&buffer, "[Allow]")),
        ControllerEffect::ApprovePermission("approval-9".to_owned())
    );

    state.show_info_dialog("Notice", "Saved the provider".to_owned());
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&mut state, size, point(&buffer, "[Close]")),
        ControllerEffect::None
    );
    assert!(state.overlay().is_none());
}

#[test]
fn provider_and_model_and_reasoning_rows_activate_on_click() {
    let mut state = state();
    state.open_provider_manager(vec![
        ProviderListItem::new(
            "alpha",
            "Alpha Provider",
            ConfiguredProviderKind::OpenAiCompatible,
            ProviderConfigSource::Managed,
            Some(OpenAiProtocol::ChatCompletions),
            Some("alpha-model"),
        ),
        ProviderListItem::new(
            "beta",
            "Beta Provider",
            ConfiguredProviderKind::OpenAiCompatible,
            ProviderConfigSource::Managed,
            Some(OpenAiProtocol::Responses),
            Some("beta-model"),
        ),
    ]);
    let size = Size::new(100, 30);
    let buffer = draw(&mut state, size);
    // In the two-column Provider Workbench, clicking a provider in the left list
    // selects it into the right-hand inline editor; clicking [Set active] activates it.
    assert_eq!(
        click(&mut state, size, point(&buffer, "Beta Provider")),
        ControllerEffect::None
    );
    let updated = draw(&mut state, size);
    assert_eq!(
        click(&mut state, size, point(&updated, "[Set active]")),
        ControllerEffect::SelectProvider {
            alias: "beta".to_owned(),
        }
    );

    state.open_model_picker(
        "alias".to_owned(),
        "Alias".to_owned(),
        vec![
            ModelListItem::new("model-one", None),
            ModelListItem::new("model-two", None),
        ],
    );
    let buffer = draw(&mut state, size);
    assert_eq!(
        click(&mut state, size, point(&buffer, "model-two")),
        ControllerEffect::OpenReasoningPicker {
            alias: "alias".to_owned(),
            model: "model-two".to_owned(),
            target: ModelPickerTarget::ActiveProvider,
        }
    );

    state.open_model_picker(
        "alias".to_owned(),
        "Alias".to_owned(),
        vec![ModelListItem::new("model-two", None)],
    );
    assert!(state.open_reasoning_picker(
        "alias".to_owned(),
        "picked-model".to_owned(),
        ModelPickerTarget::ActiveProvider,
    ));
    let buffer = draw(&mut state, size);
    let effect = click(&mut state, size, point(&buffer, "medium"));
    match effect {
        ControllerEffect::ApplyProviderModel {
            alias,
            model,
            reasoning_effort,
            target,
        } => {
            assert_eq!(alias, "alias");
            assert_eq!(model, "picked-model");
            assert_eq!(reasoning_effort.as_str(), "medium");
            assert_eq!(target, ModelPickerTarget::ActiveProvider);
        }
        other => panic!("unexpected effect: {other:?}"),
    }
}
