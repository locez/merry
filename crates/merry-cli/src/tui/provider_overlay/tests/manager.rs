use crate::config::{ConfiguredProviderKind, ProviderConfigSource};
use crate::tui::provider_overlay::{
    ProviderListItem, ProviderManagerOverlay, ProviderOverlayAction,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_provider_openai::OpenAiProtocol;

#[test]
fn provider_manager_keeps_duplicate_endpoint_profiles_as_distinct_rows() {
    let manager = ProviderManagerOverlay::new(
        vec![
            ProviderListItem::new(
                "work",
                "OpenCode Work",
                ConfiguredProviderKind::OpenAiCompatible,
                ProviderConfigSource::Managed,
                Some(OpenAiProtocol::ChatCompletions),
                Some("model-work"),
            ),
            ProviderListItem::new(
                "personal",
                "OpenCode Personal",
                ConfiguredProviderKind::OpenAiCompatible,
                ProviderConfigSource::Managed,
                Some(OpenAiProtocol::ChatCompletions),
                Some("model-personal"),
            ),
        ],
        Some("work"),
    );

    assert_eq!(manager.items().len(), 2);
    assert_eq!(manager.items()[0].alias(), "work");
    assert_eq!(manager.items()[1].alias(), "personal");
}

#[test]
fn provider_manager_requires_confirmation_before_deleting_managed_provider() {
    let mut manager = ProviderManagerOverlay::new(
        vec![ProviderListItem::new(
            "opencode",
            "OpenCode",
            ConfiguredProviderKind::OpenAiCompatible,
            ProviderConfigSource::Managed,
            Some(OpenAiProtocol::ChatCompletions),
            Some("model-a"),
        )],
        None,
    );
    let delete = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);

    assert_eq!(manager.handle_key(delete), ProviderOverlayAction::Consumed);
    assert!(
        manager
            .notice()
            .is_some_and(|notice| notice.contains("again"))
    );
    assert_eq!(
        manager.handle_key(delete),
        ProviderOverlayAction::DeleteProvider("opencode".to_owned())
    );
}

#[test]
fn provider_manager_enter_switches_the_resolved_model_and_e_edits() {
    let item = ProviderListItem::new(
        "opencode",
        "OpenCode",
        ConfiguredProviderKind::OpenAiCompatible,
        ProviderConfigSource::Managed,
        Some(OpenAiProtocol::ChatCompletions),
        Some("model-a"),
    );
    let mut switch_manager = ProviderManagerOverlay::new(vec![item.clone()], None);
    let mut edit_manager = ProviderManagerOverlay::new(vec![item.clone()], None);
    let mut model_manager = ProviderManagerOverlay::new(vec![item], None);

    assert_eq!(
        switch_manager.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ProviderOverlayAction::SelectProvider("opencode".to_owned())
    );
    assert_eq!(
        edit_manager.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
        ProviderOverlayAction::OpenProviderEditor("opencode".to_owned())
    );
    assert_eq!(
        model_manager.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE)),
        ProviderOverlayAction::OpenModelPicker("opencode".to_owned())
    );
}

#[test]
fn provider_manager_enter_opens_models_when_no_model_is_resolved() {
    let mut manager = ProviderManagerOverlay::new(
        vec![ProviderListItem::new(
            "opencode",
            "OpenCode",
            ConfiguredProviderKind::OpenAiCompatible,
            ProviderConfigSource::Managed,
            Some(OpenAiProtocol::ChatCompletions),
            None,
        )],
        None,
    );

    assert_eq!(
        manager.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ProviderOverlayAction::OpenModelPicker("opencode".to_owned())
    );
}

#[test]
fn provider_manager_refuses_to_delete_user_or_active_provider() {
    let delete = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
    let mut user_manager = ProviderManagerOverlay::new(
        vec![ProviderListItem::new(
            "user-config",
            "User Config",
            ConfiguredProviderKind::OpenAiCompatible,
            ProviderConfigSource::User,
            Some(OpenAiProtocol::ChatCompletions),
            Some("model-a"),
        )],
        None,
    );
    assert_eq!(
        user_manager.handle_key(delete),
        ProviderOverlayAction::Consumed
    );
    assert!(
        user_manager
            .notice()
            .is_some_and(|notice| notice.contains("read-only"))
    );

    let mut active_manager = ProviderManagerOverlay::new(
        vec![ProviderListItem::new(
            "active",
            "Active",
            ConfiguredProviderKind::Anthropic,
            ProviderConfigSource::Managed,
            None,
            Some("model-b"),
        )],
        Some("active"),
    );
    assert_eq!(
        active_manager.handle_key(delete),
        ProviderOverlayAction::Consumed
    );
    assert!(
        active_manager
            .notice()
            .is_some_and(|notice| notice.contains("active"))
    );
}
