use crate::config::ManagedProviderKind;
use crate::tui::provider_overlay::{
    ProviderFormField, ProviderFormOverlay, ProviderFormSeed, ProviderOverlayAction,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_llm::ReasoningEffort;
use merry_provider_openai::OpenAiProtocol;
use std::collections::BTreeSet;

#[test]
fn provider_form_masks_secret_and_never_debugs_it() {
    let mut form = ProviderFormOverlay::new("opencode".to_owned(), BTreeSet::new());
    for _ in 0..5 {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(form.selected_field(), ProviderFormField::ApiKey);
    form.insert_paste("sk-super-secret");

    assert_eq!(form.masked_api_key(), "***************");
    assert!(!format!("{form:?}").contains("sk-super-secret"));
    assert!(format!("{form:?}").contains("<redacted>"));
}

#[test]
fn provider_form_derives_readable_alias_until_the_alias_is_manually_edited() {
    let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());
    for character in "OpenCode Gateway".chars() {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }

    assert_eq!(form.field(ProviderFormField::Alias), "opencode-gateway");
}

#[test]
fn provider_form_derives_alias_without_colliding_with_existing_provider() {
    let mut form = ProviderFormOverlay::new(
        "provider".to_owned(),
        BTreeSet::from(["opencode".to_owned()]),
    );
    for character in "OpenCode".chars() {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }

    assert_eq!(form.field(ProviderFormField::Alias), "opencode-2");
}

#[test]
fn provider_form_exposes_openai_protocol_selection() {
    let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());
    assert_eq!(form.protocol(), Some(OpenAiProtocol::Responses));
    for _ in 0..3 {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }

    let _ = form.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));

    assert_eq!(form.selected_field(), ProviderFormField::Protocol);
    assert_eq!(form.protocol(), Some(OpenAiProtocol::ChatCompletions));
}

#[test]
fn provider_form_exposes_reasoning_effort_selection() {
    let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());
    for _ in 0..7 {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(form.selected_field(), ProviderFormField::ReasoningEffort);
    assert_eq!(form.reasoning_effort(), None);

    let _ = form.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
    assert_eq!(
        form.reasoning_effort()
            .as_ref()
            .map(ReasoningEffort::as_str),
        Some("minimal")
    );
    let _ = form.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
    assert_eq!(
        form.reasoning_effort()
            .as_ref()
            .map(ReasoningEffort::as_str),
        Some("low")
    );
}

#[test]
fn provider_form_supports_builtin_and_custom_reasoning_efforts() {
    let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());
    for _ in 0..7 {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }

    for expected in ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"] {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert_eq!(form.reasoning_effort_text(), expected);
    }

    let _ = form.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    for _ in 0.."ultra".len() {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    }
    form.insert_paste("max ultra");
    assert_eq!(
        form.reasoning_effort()
            .as_ref()
            .map(ReasoningEffort::as_str),
        Some("max ultra")
    );
}

#[test]
fn provider_edit_form_prefills_values_retains_secret_and_emits_update() {
    let mut form = ProviderFormOverlay::edit(
        ProviderFormSeed {
            original_alias: "opencode".to_owned(),
            display_name: "OpenCode".to_owned(),
            alias: "opencode".to_owned(),
            kind: ManagedProviderKind::OpenAiCompatible,
            protocol: Some(OpenAiProtocol::Responses),
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "model-a".to_owned(),
            reasoning_effort: None,
        },
        BTreeSet::from(["opencode".to_owned()]),
    );
    assert_eq!(form.field(ProviderFormField::DisplayName), "OpenCode");
    assert_eq!(form.masked_api_key(), "unchanged");

    let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    form.insert_paste("-pasted");
    let _ = form.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(form.field(ProviderFormField::Alias), "opencode");
    assert!(
        form.notice()
            .is_some_and(|notice| notice.contains("stable provider ID"))
    );
    let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let _ = form.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    for _ in 0..5 {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }

    let action = form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        action,
        ProviderOverlayAction::UpdateProvider { original_alias, values }
            if original_alias == "opencode"
                && values.protocol == Some(OpenAiProtocol::ChatCompletions)
                && values.api_key.is_empty()
    ));
}

#[test]
fn provider_form_model_enter_discovers_before_explicit_save() {
    let mut form = ProviderFormOverlay::edit(
        ProviderFormSeed {
            original_alias: "opencode".to_owned(),
            display_name: "OpenCode".to_owned(),
            alias: "opencode".to_owned(),
            kind: ManagedProviderKind::OpenAiCompatible,
            protocol: Some(OpenAiProtocol::ChatCompletions),
            base_url: "https://opencode.example.test/v1".to_owned(),
            model: "model-a".to_owned(),
            reasoning_effort: None,
        },
        BTreeSet::from(["opencode".to_owned()]),
    );
    for _ in 0..6 {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }

    let discover = form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        discover,
        ProviderOverlayAction::DiscoverFormModels {
            original_alias: Some(original_alias),
            values,
        } if original_alias == "opencode" && values.model == "model-a"
    ));

    for _ in 0..2 {
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(form.selected_field(), ProviderFormField::Save);
    assert!(matches!(
        form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ProviderOverlayAction::UpdateProvider { original_alias, values }
            if original_alias == "opencode" && values.model == "model-a"
    ));
}

#[test]
fn provider_form_ctrl_s_saves_from_any_field() {
    let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());

    let action = form.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

    assert!(matches!(action, ProviderOverlayAction::SaveProvider(_)));
}
