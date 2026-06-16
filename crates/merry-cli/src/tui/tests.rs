use super::input::TextInput;
use super::keymap::{KeyAction, KeyBinding, Keymap};
use super::state::{QueuePreview, TuiState};
use super::theme::{SemanticColor, TuiTheme};
use crossterm::event::{KeyCode, KeyModifiers};
use merry_core::{QueuedInputLane, QueuedInputView};

#[test]
fn text_input_inserts_deletes_and_takes_trimmed_text() {
    let mut input = TextInput::default();

    input.insert_char('h');
    input.insert_char('i');
    input.backspace();
    input.insert_char('!');

    assert_eq!(input.text(), "h!");
    assert_eq!(input.take_trimmed(), Some("h!".to_owned()));
    assert_eq!(input.text(), "");
    assert_eq!(input.take_trimmed(), None);
}

#[test]
fn default_keymap_maps_enter_to_submit_next_and_ctrl_b_to_backlog() {
    let keymap = Keymap::default();

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(KeyAction::SubmitNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL,)),
        Some(KeyAction::SubmitBacklog)
    );
}

#[test]
fn queue_preview_keeps_actual_text_and_truncates_for_display() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "short next".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![QueuedInputView {
            text: "a very long backlog item that should be truncated".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });

    assert_eq!(state.queue_preview().next[0].text, "short next");
    assert_eq!(
        state.queue_preview().backlog[0].display_text(18),
        "a very long bac..."
    );
}

#[test]
fn theme_has_required_semantic_color_slots() {
    let theme = TuiTheme::default();

    for slot in [
        SemanticColor::Status,
        SemanticColor::Muted,
        SemanticColor::Focus,
        SemanticColor::Selection,
        SemanticColor::DiffAdd,
        SemanticColor::DiffDelete,
        SemanticColor::Warning,
        SemanticColor::Error,
        SemanticColor::Risk,
        SemanticColor::Success,
    ] {
        assert!(theme.color(slot).is_some());
    }
}
