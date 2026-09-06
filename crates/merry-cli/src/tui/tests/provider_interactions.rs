use crate::{
    config::{ConfiguredProviderKind, ProviderConfigSource},
    tui::{
        controller::{ControllerEffect, handle_key_event, handle_paste_event},
        keymap::Keymap,
        overlay::Overlay,
        provider_overlay::{ModelListItem, ProviderListItem},
        render::{render_to_buffer_and_cursor, render_to_text},
        state::TuiState,
        tests::rendered_buffer_text,
        theme::TuiTheme,
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn permission_review_overlay_exposes_allow_and_reject_actions_with_exact_id() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_permission_review(
        "approval-1".to_owned(),
        "action: cargo test\nAI review fallback: provider unavailable".to_owned(),
    );

    let allow = handle_key_event(
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        allow,
        ControllerEffect::ApprovePermission("approval-1".to_owned())
    );

    state.open_permission_review("approval-2".to_owned(), "action: cargo test".to_owned());
    let reject = handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert_eq!(
        reject,
        ControllerEffect::DenyPermission("approval-2".to_owned())
    );

    state.open_permission_review("approval-3".to_owned(), "action: cargo test".to_owned());
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );
    let rendered = render_to_text(&state, 80, 10);
    assert!(rendered.contains("Permission review"));
}

#[test]
fn provider_manager_escape_returns_to_settings_when_opened_from_settings() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_settings();
    state.open_provider_manager(Vec::new());

    let effect = handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);

    assert_eq!(effect, ControllerEffect::None);
    assert!(matches!(
        state.overlay(),
        Some(crate::tui::overlay::Overlay::Settings(_))
    ));
}

#[test]
fn provider_error_dialog_wraps_and_restores_provider_manager() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_provider_manager(vec![ProviderListItem::new(
        "opencode",
        "OpenCode",
        ConfiguredProviderKind::OpenAiCompatible,
        ProviderConfigSource::Managed,
        Some(merry_provider_openai::OpenAiProtocol::Responses),
        Some("model-a"),
    )]);
    state.set_provider_overlay_error(
        "provider opencode is defined in config.toml and cannot be edited from this interface"
            .to_owned(),
    );

    let text = render_to_text(&state, 50, 18);
    assert!(text.contains("Provider error"));
    assert!(text.contains("config.toml"));
    assert!(text.contains("interface"));
    assert!(matches!(
        state.overlay(),
        Some(crate::tui::overlay::Overlay::Dialog(_))
    ));

    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(matches!(
        state.overlay(),
        Some(crate::tui::overlay::Overlay::ProviderManager(_))
    ));
}

#[test]
fn model_discovery_error_dialog_returns_to_model_picker() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_model_picker("opencode".to_owned(), "OpenCode".to_owned(), Vec::new());

    state.update_model_picker(
        "opencode",
        Err("the model endpoint returned a response that could not be parsed".to_owned()),
    );

    assert!(render_to_text(&state, 60, 18).contains("Model discovery failed"));
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert!(matches!(
        state.overlay(),
        Some(crate::tui::overlay::Overlay::ModelPicker(_))
    ));
}

#[test]
fn model_selection_requires_reasoning_before_it_emits_a_runtime_change() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_model_picker(
        "opencode".to_owned(),
        "OpenCode".to_owned(),
        vec![ModelListItem::new("model-a", None)],
    );

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::OpenReasoningPicker {
            alias: "opencode".to_owned(),
            model: "model-a".to_owned(),
            target: crate::tui::provider_overlay::ModelPickerTarget::ActiveProvider,
        }
    );
    assert!(state.open_reasoning_picker(
        "opencode".to_owned(),
        "model-a".to_owned(),
        crate::tui::provider_overlay::ModelPickerTarget::ActiveProvider,
    ));

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::ApplyProviderModel {
            alias: "opencode".to_owned(),
            model: "model-a".to_owned(),
            reasoning_effort: merry_llm::ReasoningEffort::new("minimal").expect("preset is valid"),
            target: crate::tui::provider_overlay::ModelPickerTarget::ActiveProvider,
        }
    );
}

#[test]
fn provider_surfaces_fit_supported_terminal_sizes_without_secret_exposure() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_provider_manager(vec![ProviderListItem::new(
        "opencode",
        "OpenCode Gateway",
        ConfiguredProviderKind::OpenAiCompatible,
        ProviderConfigSource::Managed,
        Some(merry_provider_openai::OpenAiProtocol::ChatCompletions),
        Some("deepseek-v4-pro"),
    )]);
    let manager = render_to_text(&state, 100, 30);
    assert!(manager.contains("Protocol"));
    assert!(manager.contains("Chat completions"));
    assert!(manager.contains("N Add"));
    assert!(manager.contains("Enter Switch"));
    assert!(manager.contains("M Models"));
    assert!(manager.contains("E Edit"));
    assert!(manager.contains("D Delete"));
    for (width, height) in [(100, 30), (80, 24), (40, 16)] {
        let text = render_to_text(&state, width, height);
        assert!(text.contains("Providers"));
        assert!(text.contains("OpenCode"));
    }

    state.open_provider_form("provider".to_owned(), Default::default());
    handle_paste_event("OpenCode", &mut state);
    let form = render_to_text(&state, 100, 30);
    assert!(form.contains("API protocol"));
    assert!(form.contains("Responses"));
    assert!(form.contains("Thinking mode"));
    assert!(!form.contains("Default"));
    assert!(form.contains("Save provider"));
    assert!(form.contains("Ctrl+S Save"));
    for (width, height) in [(100, 30), (80, 24), (40, 16)] {
        let (buffer, cursor) = render_to_buffer_and_cursor(&state, width, height);
        let text = rendered_buffer_text(&buffer);
        assert!(text.contains("Add provider"));
        assert!(!text.contains("sk-super-secret"));
        assert!(cursor.x < width);
        assert!(cursor.y < height);
    }

    state.open_provider_editor(
        crate::tui::provider_overlay::ProviderFormSeed {
            original_alias: "opencode".to_owned(),
            display_name: "OpenCode Gateway".to_owned(),
            alias: "opencode".to_owned(),
            kind: crate::config::ManagedProviderKind::OpenAiCompatible,
            protocol: Some(merry_provider_openai::OpenAiProtocol::ChatCompletions),
            base_url: "https://gateway.example.test/v1".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            reasoning_effort: None,
        },
        Default::default(),
    );
    let edit = render_to_text(&state, 100, 30);
    assert!(edit.contains("Edit provider"));
    assert!(edit.contains("Chat Completions"));
    assert!(edit.contains("unchanged"));

    state.open_model_picker(
        "opencode".to_owned(),
        "OpenCode Gateway".to_owned(),
        vec![ModelListItem::new("deepseek-v4-pro", Some("gateway"))],
    );
    let models = render_to_text(&state, 100, 30);
    assert!(models.contains("Enter Use"));
    assert!(models.contains("F5 Refresh"));
    for (width, height) in [(100, 30), (80, 24), (40, 16)] {
        let (buffer, cursor) = render_to_buffer_and_cursor(&state, width, height);
        let text = rendered_buffer_text(&buffer);
        assert!(text.contains("Models"));
        assert!(text.contains("deepseek"));
        assert!(cursor.x < width);
        assert!(cursor.y < height);
    }
}

#[test]
fn provider_form_model_picker_returns_selection_to_the_form() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_provider_editor(
        crate::tui::provider_overlay::ProviderFormSeed {
            original_alias: "opencode".to_owned(),
            display_name: "OpenCode".to_owned(),
            alias: "opencode".to_owned(),
            kind: crate::config::ManagedProviderKind::OpenAiCompatible,
            protocol: Some(merry_provider_openai::OpenAiProtocol::ChatCompletions),
            base_url: "https://opencode.example.test/v1".to_owned(),
            model: "model-a".to_owned(),
            reasoning_effort: None,
        },
        Default::default(),
    );

    assert!(state.open_provider_form_model_picker("opencode".to_owned(), "OpenCode".to_owned(),));
    state.update_model_picker("opencode", Ok(vec![ModelListItem::new("model-b", None)]));
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::OpenReasoningPicker {
            alias: "opencode".to_owned(),
            model: "model-b".to_owned(),
            target: crate::tui::provider_overlay::ModelPickerTarget::ProviderForm,
        }
    );
    assert!(state.open_reasoning_picker(
        "opencode".to_owned(),
        "model-b".to_owned(),
        crate::tui::provider_overlay::ModelPickerTarget::ProviderForm,
    ));
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
        ),
        ControllerEffect::ApplyProviderModel {
            alias: "opencode".to_owned(),
            model: "model-b".to_owned(),
            reasoning_effort: merry_llm::ReasoningEffort::new("minimal").expect("preset is valid"),
            target: crate::tui::provider_overlay::ModelPickerTarget::ProviderForm,
        }
    );
    assert!(state.select_provider_form_model_with_reasoning("model-b", "minimal"));

    let Some(Overlay::ProviderForm(form)) = state.overlay() else {
        panic!("provider form should be restored");
    };
    assert_eq!(
        form.field(crate::tui::provider_overlay::ProviderFormField::Model),
        "model-b"
    );
    assert_eq!(
        form.selected_field(),
        crate::tui::provider_overlay::ProviderFormField::ReasoningEffort
    );
    assert_eq!(form.reasoning_effort_text(), "minimal");
}

#[test]
fn provider_form_model_picker_escape_preserves_unsaved_form() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_provider_form("provider".to_owned(), Default::default());
    handle_paste_event("Unsaved Provider", &mut state);
    assert!(state.open_provider_form_model_picker(
        "unsaved-provider".to_owned(),
        "Unsaved Provider".to_owned(),
    ));

    let effect = handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);

    assert_eq!(effect, ControllerEffect::BackToProviderForm);
    state.back_overlay();
    let Some(Overlay::ProviderForm(form)) = state.overlay() else {
        panic!("provider form should be restored");
    };
    assert_eq!(
        form.field(crate::tui::provider_overlay::ProviderFormField::DisplayName),
        "Unsaved Provider"
    );
}
