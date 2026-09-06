use crate::tui::provider_overlay::{ModelListItem, ModelPickerOverlay};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn model_picker_shows_cached_models_while_loading_and_accepts_manual_search() {
    let mut picker = ModelPickerOverlay::new(
        "opencode".to_owned(),
        "OpenCode".to_owned(),
        vec![ModelListItem::new("cached-model", Some("gateway"))],
        true,
    );
    assert!(picker.is_loading());
    assert_eq!(picker.visible_models()[0].id(), "cached-model");

    for character in "manual-model".chars() {
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }

    assert_eq!(picker.manual_model(), Some("manual-model"));
}
