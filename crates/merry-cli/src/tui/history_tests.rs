use super::{
    controller::persist_submitted_input_history,
    input_history_store::InputHistoryStore,
    keymap::Keymap,
    state::{TimelineItem, TuiState},
    theme::TuiTheme,
};
use std::path::Path;

fn state() -> TuiState {
    TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

#[tokio::test]
async fn accepted_text_updates_memory_and_workspace_history_together() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = InputHistoryStore::for_workspace(temp.path(), Path::new("/repo"));
    let mut state = state();
    let mut warning_shown = false;

    persist_submitted_input_history(&store, &mut state, "first\nline", &mut warning_shown).await;
    persist_submitted_input_history(&store, &mut state, "second", &mut warning_shown).await;

    assert_eq!(state.input_history_entries(), ["first\nline", "second"]);
    assert_eq!(store.load().await, ["first\nline", "second"]);
    assert!(!warning_shown);
}

#[tokio::test]
async fn blank_or_image_only_history_text_is_not_persisted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = InputHistoryStore::for_workspace(temp.path(), Path::new("/repo"));
    let mut state = state();
    let mut warning_shown = false;

    persist_submitted_input_history(&store, &mut state, "  ", &mut warning_shown).await;

    assert!(state.input_history_entries().is_empty());
    assert!(!store.path().exists());
    assert!(!warning_shown);
}

#[tokio::test]
async fn persistence_failure_and_recovery_keep_memory_history_and_warn_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let blocked_state_dir = temp.path().join("not-a-directory");
    std::fs::write(&blocked_state_dir, "blocked").expect("blocking file");
    let store = InputHistoryStore::for_workspace(&blocked_state_dir, Path::new("/repo"));
    let mut state = state();
    let mut warning_shown = false;

    persist_submitted_input_history(&store, &mut state, "first", &mut warning_shown).await;
    persist_submitted_input_history(&store, &mut state, "second", &mut warning_shown).await;
    std::fs::remove_file(&blocked_state_dir).expect("remove blocking file");
    persist_submitted_input_history(&store, &mut state, "third", &mut warning_shown).await;

    assert_eq!(state.input_history_entries(), ["first", "second", "third"]);
    assert_eq!(store.load().await, ["third"]);
    assert!(warning_shown);
    assert_eq!(
        state
            .timeline()
            .iter()
            .filter(|item| matches!(
                item,
                TimelineItem::Diagnostic { title, .. } if title == "Input history not saved"
            ))
            .count(),
        1
    );
}
