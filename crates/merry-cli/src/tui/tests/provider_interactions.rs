use crate::{
    config::ConfiguredProviderKind,
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
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT),
            &mut state,
        ),
        ControllerEffect::None
    );
    let rendered = render_to_text(&state, 80, 10);
    assert!(rendered.contains("Allow this command?"));
}

#[test]
fn structured_permission_review_renders_hierarchy_badges_queue_and_scrolls() {
    use crate::permission_prompt::{
        PermissionCapabilityBadge, PermissionFallbackNotice, PermissionReviewView,
    };
    use merry::permissions::{PathAccess, PermissionReviewRisk, PermissionUserAuthorization};

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_permission_review_view(PermissionReviewView {
        approval_id: "approval-rich".to_owned(),
        actor_label: Some("subagent reviewer [review] (subagent-2)".to_owned()),
        command: "git push origin fix/review-ui".to_owned(),
        cwd: ".".to_owned(),
        capabilities: vec![
            PermissionCapabilityBadge::Network,
            PermissionCapabilityBadge::Path {
                path: "/tmp".to_owned(),
                access: PathAccess::ReadWrite,
            },
            PermissionCapabilityBadge::HostIntegration("ssh-agent".to_owned()),
        ],
        review_only: false,
        agent_reason: Some("Push verified commits to the remote branch.".to_owned()),
        fallback: Some(PermissionFallbackNotice::ModelDenied {
            risk: PermissionReviewRisk::High,
            user_authorization: PermissionUserAuthorization::Medium,
            rationale: "Remote push requires explicit user direction for the target branch."
                .to_owned(),
        }),
        queue_total: 3,
        plain_body: None,
    });

    let rendered = render_to_text(&state, 88, 26);
    assert!(rendered.contains("Allow this command?"), "{rendered}");
    assert!(
        rendered.contains("subagent reviewer [review] (subagent-2)"),
        "{rendered}"
    );
    assert!(rendered.contains("[HIGH RISK]"), "{rendered}");
    assert!(rendered.contains("1/3"), "{rendered}");
    assert!(rendered.contains("COMMAND"), "{rendered}");
    assert!(
        rendered.contains("$ git push origin fix/review-ui"),
        "{rendered}"
    );
    assert!(rendered.contains("CAPABILITIES"), "{rendered}");
    assert!(rendered.contains("[network]"), "{rendered}");
    assert!(rendered.contains("[rw /tmp]"), "{rendered}");
    assert!(rendered.contains("[ssh-agent]"), "{rendered}");
    assert!(rendered.contains("AI REVIEW"), "{rendered}");
    assert!(rendered.contains("auth: medium"), "{rendered}");
    assert!(
        rendered.contains("Remote push requires explicit user direction"),
        "{rendered}"
    );
    assert!(rendered.contains("REASON"), "{rendered}");
    assert!(rendered.contains("[Allow]"), "{rendered}");
    assert!(rendered.contains("[Don't allow]"), "{rendered}");
    assert!(rendered.contains("Enter/Y allow"), "{rendered}");

    // Scrolling updates the overlay offset without closing or deciding.
    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert!(matches!(
        state.overlay(),
        Some(crate::tui::overlay::Overlay::PermissionReview(_))
    ));
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
        Some(merry::providers::OpenAiProtocol::Responses),
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
            reasoning_effort: merry::providers::ReasoningEffort::new("minimal")
                .expect("preset is valid"),
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
        Some(merry::providers::OpenAiProtocol::ChatCompletions),
        Some("deepseek-v4-pro"),
    )]);
    let manager = render_to_text(&state, 100, 30);
    assert!(manager.contains("PROVIDERS"), "{manager}");
    assert!(manager.contains("+ Add provider"), "{manager}");
    assert!(manager.contains("API protocol"), "{manager}");
    assert!(manager.contains("Chat Completions"), "{manager}");
    assert!(manager.contains("[Save provider]"), "{manager}");
    assert!(manager.contains("N Add"), "{manager}");
    assert!(manager.contains("Enter Switch"), "{manager}");
    assert!(manager.contains("M Models"), "{manager}");
    assert!(manager.contains("E Edit"), "{manager}");
    assert!(manager.contains("D Delete"), "{manager}");
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
            protocol: Some(merry::providers::OpenAiProtocol::ChatCompletions),
            base_url: "https://gateway.example.test/v1".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            reasoning_effort: None,
            service_tier: None,
        },
        Default::default(),
    );
    let edit = render_to_text(&state, 100, 30);
    assert!(edit.contains("Edit provider"));
    assert!(edit.contains("Chat Completions"));
    assert!(edit.contains("saved"));
    assert!(edit.contains("[Copy]"));

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
            protocol: Some(merry::providers::OpenAiProtocol::ChatCompletions),
            base_url: "https://opencode.example.test/v1".to_owned(),
            model: "model-a".to_owned(),
            reasoning_effort: None,
            service_tier: None,
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
            reasoning_effort: merry::providers::ReasoningEffort::new("minimal")
                .expect("preset is valid"),
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

#[test]
fn provider_workbench_supports_inline_two_column_editing_adding_and_close_click() {
    use crate::tui::{
        controller::{handle_mouse_input, handle_mouse_scroll_down, handle_mouse_scroll_up},
        render::render_to_buffer,
        tests::find_text_position,
        text_interaction::MouseInput,
    };
    use ratatui::layout::{Position, Size};

    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_provider_manager(vec![
        ProviderListItem::new(
            "alpha",
            "Alpha Managed",
            ConfiguredProviderKind::OpenAiCompatible,
            Some(merry::providers::OpenAiProtocol::Responses),
            Some("gpt-5"),
        )
        .with_details(
            Some("https://alpha.example/v1".to_owned()),
            Some("medium".to_owned()),
        ),
        ProviderListItem::new(
            "beta",
            "Beta Anthropic",
            ConfiguredProviderKind::Anthropic,
            None,
            Some("claude-sonnet"),
        )
        .with_details(Some("https://api.anthropic.com".to_owned()), None),
    ]);

    let size = Size::new(104, 28);
    let buffer = render_to_buffer(&state, size.width, size.height);
    let text = rendered_buffer_text(&buffer);
    assert!(text.contains("https://alpha.example/v1"), "{text}");
    assert!(text.contains("Edit provider"), "{text}");

    // Clicking the Base URL row in the right pane focuses the inline editor directly.
    let (url_x, url_y) = find_text_position(&buffer, "Base URL").expect("Base URL field");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(url_x, url_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    handle_paste_event("/beta", &mut state);
    let edited = render_to_text(&state, size.width, size.height);
    assert!(edited.contains("https://alpha.example/v1/beta"), "{edited}");

    // Clicking "+ Add provider" switches the right column to a fresh Add provider form.
    let buffer = render_to_buffer(&state, size.width, size.height);
    let (add_x, add_y) = find_text_position(&buffer, "+ Add provider").expect("+ Add provider");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(add_x, add_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    let adding = render_to_text(&state, size.width, size.height);
    assert!(adding.contains("Add provider"), "{adding}");

    // Mouse-wheel scroll navigates by exactly 1 item per notch (never jumping straight to bottom).
    let four_items: Vec<ProviderListItem> = (1..=4)
        .map(|idx| {
            ProviderListItem::new(
                &format!("p{idx}"),
                &format!("Provider {idx}"),
                ConfiguredProviderKind::OpenAiCompatible,
                Some(merry::providers::OpenAiProtocol::Responses),
                Some(&format!("m{idx}")),
            )
        })
        .collect();
    state.open_provider_manager(four_items);
    assert_eq!(
        handle_mouse_scroll_down(Position::new(10, 10), size, &mut state),
        ControllerEffect::None
    );
    match state.overlay() {
        Some(Overlay::ProviderManager(manager)) => {
            assert_eq!(
                manager.selected(),
                1,
                "single wheel notch must step by 1 item, not jump to bottom"
            );
        }
        other => panic!("expected ProviderManager overlay, got {other:?}"),
    }
    assert_eq!(
        handle_mouse_scroll_up(Position::new(10, 10), size, &mut state),
        ControllerEffect::None
    );
    match state.overlay() {
        Some(Overlay::ProviderManager(manager)) => assert_eq!(manager.selected(), 0),
        other => panic!("expected ProviderManager overlay, got {other:?}"),
    }

    // Clicking top-right [×] closes the workbench back to command palette / closes modal.
    let buffer = render_to_buffer(&state, size.width, size.height);
    let (close_x, close_y) = find_text_position(&buffer, "[×]").expect("[×] close button");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(close_x + 1, close_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    assert!(!matches!(
        state.overlay(),
        Some(Overlay::ProviderManager(_))
    ));
}
