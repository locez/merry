use super::completion::{CompletionKind, CompletionSources};
use super::controller::{
    ControllerEffect, apply_clipboard_image_completion, handle_key_action, handle_key_event,
    handle_mouse_scroll_down, handle_mouse_scroll_up, handle_paste_event,
};
use super::input::{DraftImage, TextInput, TuiSubmission};
use super::keymap::{KeyAction, KeyBinding, Keymap};
use super::overlay::{Overlay, PaletteCommand};
use super::panels::{FocusPanelBody, FocusPanelTone, focus_panel_view};
use super::preferences::CodeTheme;
use super::projector::TuiProjector;
use super::provider_overlay::{ModelListItem, ProviderListItem};
use super::render::{render_to_buffer, render_to_buffer_and_cursor, render_to_text};
use super::state::{PatchChangeView, PatchLineView, QueuePreview, TimelineItem, TuiState};
use super::theme::{SemanticColor, TuiTheme};
use crate::config::{ConfiguredProviderKind, ProviderConfigSource};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, CompactionUsageWindow, ContextWindowSource, ErrorInfo,
    InteractiveRunState, ModelUsage, PendingToolCall, PendingToolCallBatch, QueuedInputLane,
    QueuedInputView, RuntimeEvent, RuntimeEventSource, SessionId, SessionUsage, ToolCallArguments,
    ToolCallBatchId, ToolCallId, ToolCallResult, ToolName, ToolOutput, UsageContextWindow,
};
use merry_runtime::{SessionTranscriptItem, SkillMetadata};
use merry_tool_workspace::WORKSPACE_PATCH_TOOL;
use ratatui::{
    layout::{Position, Size},
    style::{Color, Modifier},
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn text_submission(text: &str) -> TuiSubmission {
    TuiSubmission {
        text: text.to_owned(),
        history_text: text.to_owned(),
        images: Vec::new(),
    }
}

fn draft_image(marker: u8) -> DraftImage {
    DraftImage::new([137, 80, 78, 71, 13, 10, 26, 10, marker], 2, 3).expect("valid draft image")
}

fn source() -> RuntimeEventSource {
    RuntimeEventSource::new(SessionId::new("tui-test").unwrap(), 1)
}

fn text_artifact(id: &str) -> ArtifactRef {
    ArtifactRef::new(ArtifactId::new(id).unwrap(), ArtifactKind::Text)
}

fn pending_call(id: &str, tool_name: &str) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(id).unwrap(),
        ToolName::new(tool_name).unwrap(),
        ToolCallArguments::new(Default::default()),
    )
}

fn pending_call_with_args(
    id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new(id).unwrap(),
        ToolName::new(tool_name).unwrap(),
        ToolCallArguments::try_from(arguments).unwrap(),
    )
}

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

fn pending_batch(id: &str, calls: Vec<PendingToolCall>) -> PendingToolCallBatch {
    PendingToolCallBatch::new(ToolCallBatchId::new(id).unwrap(), calls).unwrap()
}

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
fn text_input_preserves_submit_newlines() {
    let mut input = TextInput::default();

    input.insert_str("\nfirst line\nsecond line\n");

    assert_eq!(
        input.take_trimmed(),
        Some("\nfirst line\nsecond line\n".to_owned())
    );
    assert_eq!(input.text(), "");
}

#[test]
fn text_input_compacts_large_paste_until_submit() {
    let mut input = TextInput::default();
    let pasted = "hello world\n".repeat(30);
    let placeholder = format!("[pasted {} chars]", pasted.chars().count());

    input.insert_str("prefix ");
    input.insert_paste(&pasted);
    input.insert_str(" suffix");

    assert_eq!(input.text(), format!("prefix {placeholder} suffix"));
    assert_eq!(
        input.take_trimmed(),
        Some(format!("prefix {pasted} suffix"))
    );
}

#[test]
fn text_input_deletes_large_paste_placeholder_as_one_block() {
    let mut input = TextInput::default();
    let pasted = "hello world\n".repeat(30);
    let placeholder = format!("[pasted {} chars]", pasted.chars().count());

    input.insert_paste(&pasted);
    assert_eq!(input.text(), placeholder);

    input.backspace();

    assert_eq!(input.text(), "");
    assert_eq!(input.cursor_byte_index(), 0);

    input.insert_paste(&pasted);
    input.move_home();
    input.delete();

    assert_eq!(input.text(), "");
    assert_eq!(input.cursor_byte_index(), 0);
}

#[test]
fn text_input_deleting_paste_placeholder_removes_stale_expansion() {
    let mut input = TextInput::default();
    let first = "a".repeat(300);
    let second = "b".repeat(300);

    input.insert_paste(&first);
    input.backspace();
    input.insert_paste(&second);

    assert_eq!(input.take_trimmed(), Some(second));
}

#[test]
fn text_input_inserts_pasted_text_at_cursor() {
    let mut input = TextInput::default();

    input.insert_char('你');
    input.insert_str("好 world");

    assert_eq!(input.text(), "你好 world");
}

#[test]
fn text_input_handles_plain_chars_and_backspace_key_events() {
    let mut input = TextInput::default();

    input.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    input.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::SHIFT));
    input.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

    assert_eq!(input.text(), "a");
}

#[test]
fn text_input_moves_cursor_and_edits_at_cursor() {
    let mut input = TextInput::default();

    input.insert_str("abc");
    input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    input.insert_char('X');
    input.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    input.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    input.insert_char('!');
    input.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    input.insert_char('>');

    assert_eq!(input.text(), ">aXc!");
    assert_eq!(input.cursor_byte_index(), ">".len());
}

#[test]
fn text_input_replaces_completion_token_at_cursor() {
    let mut input = TextInput::default();

    input.insert_str("open @rend");
    input.replace_range(
        "open ".len().."open @rend".len(),
        "@crates/merry-cli/src/tui/render.rs ",
    );

    assert_eq!(input.text(), "open @crates/merry-cli/src/tui/render.rs ");
    assert_eq!(input.cursor_byte_index(), input.text().len());
}

#[test]
fn completion_sources_fuzzy_match_workspace_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let nested = temp.path().join("crates/merry-cli/src/tui");
    fs::create_dir_all(&nested).expect("mkdir nested");
    fs::write(nested.join("render.rs"), "").expect("write render");
    fs::write(nested.join("state.rs"), "").expect("write state");
    let sources = CompletionSources::from_skill_names(temp.path().to_path_buf(), &[]);

    let menu = sources
        .menu_for_input("edit @cmrender", "edit @cmrender".len(), None)
        .expect("path completion");

    assert_eq!(menu.items()[0].kind(), &CompletionKind::Path);
    assert_eq!(
        menu.items()[0].value(),
        "crates/merry-cli/src/tui/render.rs"
    );
}

#[test]
fn completion_sources_match_skill_references_without_expanding_text() {
    let sources = CompletionSources::from_skill_names(
        std::env::current_dir().expect("cwd"),
        &["brainstorming", "frontend-design"],
    );

    let menu = sources
        .menu_for_input("use $brain", "use $brain".len(), None)
        .expect("skill completion");

    assert_eq!(menu.items()[0].kind(), &CompletionKind::Skill);
    assert_eq!(menu.items()[0].value(), "brainstorming");
    assert_eq!(menu.replacement_text(), Some("$brainstorming ".to_owned()));
}

#[test]
fn completion_sources_include_skill_descriptions_as_detail() {
    let skill = SkillMetadata::new(
        "brainstorming",
        "Use for collaborative design work.",
        PathBuf::from("skills/brainstorming/SKILL.md"),
        PathBuf::from("/skills"),
    )
    .expect("valid skill");
    let sources = CompletionSources::new(std::env::current_dir().expect("cwd"), vec![skill]);

    let menu = sources
        .menu_for_input("$brain", "$brain".len(), None)
        .expect("skill completion");

    assert_eq!(
        menu.items()[0].detail(),
        Some("Use for collaborative design work.")
    );
}

#[test]
fn controller_accepts_completion_before_submit() {
    let mut state = TuiState::new(
        std::env::current_dir().expect("cwd"),
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.set_completion_skills(Vec::new());
    state.insert_input_str("edit @Cargo");

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.input_text(), "edit @Cargo.toml ");
}

#[test]
fn controller_tab_accepts_completion_like_shells() {
    let mut state = TuiState::new(
        std::env::current_dir().expect("cwd"),
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("edit @Cargo");

    let effect = handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);

    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.input_text(), "edit @Cargo.toml ");
}

#[test]
fn controller_moves_completion_selection_with_arrows() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::write(temp.path().join("alpha.rs"), "").expect("write alpha");
    fs::write(temp.path().join("beta.rs"), "").expect("write beta");
    let mut state = TuiState::new(
        temp.path().to_path_buf(),
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("open @rs");

    handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert_eq!(state.input_text(), "open @beta.rs ");
}

#[test]
fn renderer_shows_completion_candidates_above_input() {
    let mut state = TuiState::new(
        std::env::current_dir().expect("cwd"),
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("open @Cargo");

    let text = render_to_text(&state, 80, 12);

    assert!(text.contains("> Cargo.toml"));
    assert!(!text.contains("> @ Cargo.toml"));
    assert!(text.find("> Cargo.toml").unwrap() < text.find('M').unwrap());
}

#[test]
fn renderer_shows_skill_completion_descriptions() {
    let skill = SkillMetadata::new(
        "brainstorming",
        "Use for collaborative design work.",
        PathBuf::from("skills/brainstorming/SKILL.md"),
        PathBuf::from("/skills"),
    )
    .expect("valid skill");
    let mut state = TuiState::new(
        std::env::current_dir().expect("cwd"),
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.set_completion_skills(vec![skill]);
    state.insert_input_str("$brain");

    let text = render_to_text(&state, 100, 12);

    assert!(text.contains("> brainstorming"));
    assert!(text.contains("Use for collaborative"));
}

#[test]
fn text_input_supports_common_shell_line_editing_keys() {
    let mut input = TextInput::default();

    input.insert_str("alpha beta");
    input.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    input.insert_str("> ");
    input.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    input.insert_str(" tail");
    input.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    input.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    input.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

    assert_eq!(input.text(), "");
    assert_eq!(input.cursor_byte_index(), 0);
}

#[test]
fn text_input_viewport_uses_terminal_width_for_wide_chars() {
    let mut input = TextInput::default();

    input.insert_str("a你好b");

    let full_viewport = input.viewport(7);
    assert_eq!(full_viewport.text, "a你好b");
    assert_eq!(full_viewport.cursor_column, 6);

    input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

    let viewport = input.viewport(4);

    assert_eq!(viewport.text, "好b");
    assert_eq!(viewport.cursor_column, 2);
}

#[test]
fn text_input_viewport_reports_multiline_cursor_row() {
    let mut input = TextInput::default();

    input.insert_str("first");
    input.insert_newline();
    input.insert_str("second");

    let viewport = input.viewport_rows(16, 5);

    assert_eq!(viewport.text, "first\nsecond");
    assert_eq!(viewport.cursor_row, 1);
    assert_eq!(viewport.cursor_column, 6);
    assert_eq!(viewport.visible_rows, 2);
}

#[test]
fn text_input_multiline_viewport_keeps_cursor_line_visible() {
    let mut input = TextInput::default();

    input.insert_str("one\ntwo\nthree\nfour\nfive\nsix");

    let viewport = input.viewport_rows(16, 3);

    assert_eq!(viewport.text, "four\nfive\nsix");
    assert_eq!(viewport.cursor_row, 2);
    assert_eq!(viewport.cursor_column, 3);
    assert_eq!(viewport.visible_rows, 3);
}

#[test]
fn controller_submit_next_takes_input_text() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.input_mut().insert_char('n');
    state.input_mut().insert_char('o');
    state.input_mut().insert_char('w');

    let effect = handle_key_action(KeyAction::SubmitNext, &mut state);

    assert_eq!(effect, ControllerEffect::SubmitNext(text_submission("now")));
    assert_eq!(state.input_text(), "");
}

#[test]
fn controller_submit_next_preserves_multiline_input_text() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("1234");
    state.insert_input_newline();
    state.insert_input_str("换行测试");

    let effect = handle_key_action(KeyAction::SubmitNext, &mut state);

    assert_eq!(
        effect,
        ControllerEffect::SubmitNext(text_submission("1234\n换行测试"))
    );
}

#[test]
fn controller_submit_carries_images_and_records_text_only_history() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("inspect ");
    state
        .input_mut()
        .insert_image(draft_image(7))
        .expect("image should insert");

    let effect = handle_key_action(KeyAction::SubmitNext, &mut state);
    let ControllerEffect::SubmitNext(submission) = effect else {
        panic!("image submission should produce a next-lane effect");
    };

    assert_eq!(submission.text, "inspect [Image #1]");
    assert_eq!(submission.history_text, "inspect ");
    assert_eq!(submission.images.len(), 1);
    assert_eq!(submission.images[0].label(), "[Image #1]");
    assert_eq!(submission.images[0].png_bytes()[8], 7);
    assert!(state.input_text().is_empty());
    state.record_input_history(&submission.history_text);

    assert_eq!(
        handle_key_action(KeyAction::HistoryPrevious, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "inspect ");
}

#[test]
fn controller_submit_records_shell_like_input_history() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.input_mut().insert_str("first");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("first"))
    );
    state.record_input_history("first");
    state.input_mut().insert_str("second");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("second"))
    );
    state.record_input_history("second");

    assert_eq!(
        handle_key_action(KeyAction::HistoryPrevious, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "second");
    assert_eq!(
        handle_key_action(KeyAction::HistoryPrevious, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "first");
    assert_eq!(
        handle_key_action(KeyAction::HistoryNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "second");
    assert_eq!(
        handle_key_action(KeyAction::HistoryNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "");
}

#[test]
fn controller_history_restores_unsent_draft() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.input_mut().insert_str("sent");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("sent"))
    );
    state.record_input_history("sent");
    state.input_mut().insert_str("draft");

    assert_eq!(
        handle_key_action(KeyAction::HistoryPrevious, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "sent");
    assert_eq!(
        handle_key_action(KeyAction::HistoryNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "draft");
}

#[test]
fn controller_empty_submit_does_nothing() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    let effect = handle_key_action(KeyAction::SubmitNext, &mut state);

    assert_eq!(effect, ControllerEffect::None);
}

#[test]
fn controller_scroll_actions_move_timeline_viewport() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    let initial = state.timeline_scroll_offset();

    assert_eq!(
        handle_key_action(KeyAction::ScrollUp, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_scroll_offset(), initial.saturating_add(5));

    assert_eq!(
        handle_key_action(KeyAction::ScrollDown, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_scroll_offset(), initial);
}

#[test]
fn controller_mouse_scroll_routes_focus_pane_independently_from_chat() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let size = Size::new(180, 28);
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran cargo test".to_owned(),
        body: "output".to_owned(),
    });
    state.select_previous_artifact();

    handle_mouse_scroll_down(Position::new(150, 5), size, &mut state);
    assert_eq!(state.focus_scroll_offset(), 5);
    assert_eq!(state.timeline_scroll_offset(), 0);

    handle_mouse_scroll_down(Position::new(10, 5), size, &mut state);
    assert_eq!(state.focus_scroll_offset(), 5);
    assert_eq!(state.timeline_scroll_offset(), 0);

    state.scroll_timeline_up_by(10);
    handle_mouse_scroll_down(Position::new(10, 5), size, &mut state);
    assert_eq!(state.focus_scroll_offset(), 5);
    assert_eq!(state.timeline_scroll_offset(), 5);
}

#[test]
fn controller_review_previous_user_input_steps_between_user_turns() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "answer one".to_owned(),
    });
    state.push_timeline_item(TimelineItem::User {
        text: "second".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "answer two".to_owned(),
    });

    assert_eq!(
        handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_review_user_index(), Some(2));

    assert_eq!(
        handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_review_user_index(), Some(0));
}

#[test]
fn controller_submit_exits_review_mode_before_submitting_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "answer".to_owned(),
    });
    state.input_mut().insert_str("draft");
    state.jump_to_previous_user_input();

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_review_user_index(), None);
    assert_eq!(state.timeline_scroll_offset(), 0);
    assert_eq!(state.input_text(), "draft");

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("draft"))
    );
}

#[test]
fn controller_artifact_review_steps_through_artifacts_and_submit_returns_to_latest_first() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran first command".to_owned(),
        body: "first output".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran second command".to_owned(),
        body: "second output".to_owned(),
    });
    state.insert_input_str("draft");

    assert_eq!(
        handle_key_action(KeyAction::ReviewPreviousArtifact, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.artifact_review_timeline_index(), Some(1));

    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran third command".to_owned(),
        body: "third output".to_owned(),
    });
    assert_eq!(state.selected_artifact_timeline_index(), Some(1));

    assert_eq!(
        handle_key_action(KeyAction::ReviewPreviousArtifact, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.artifact_review_timeline_index(), Some(0));

    assert_eq!(
        handle_key_action(KeyAction::ReviewNextArtifact, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.artifact_review_timeline_index(), Some(1));

    assert_eq!(
        handle_key_action(KeyAction::FollowLatestArtifact, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.selected_artifact_timeline_index(), Some(2));

    assert_eq!(
        handle_key_action(KeyAction::ReviewPreviousArtifact, &mut state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.artifact_review_timeline_index(), None);
    assert_eq!(state.input_text(), "draft");

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("draft"))
    );
}

#[test]
fn controller_follow_latest_clears_every_review_and_scroll_state() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran command".to_owned(),
        body: "full output".to_owned(),
    });
    state.scroll_timeline_up_by(25);
    state.jump_to_previous_user_input();
    state.select_previous_artifact();

    assert_eq!(
        handle_key_action(KeyAction::FollowLatestArtifact, &mut state),
        ControllerEffect::None
    );

    assert_eq!(state.timeline_scroll_offset(), 0);
    assert_eq!(state.timeline_review_user_index(), None);
    assert_eq!(state.artifact_review_timeline_index(), None);
    assert_eq!(state.focus_scroll_offset(), 0);
}

#[test]
fn controller_suspended_actions_emit_runtime_effects() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    assert_eq!(
        handle_key_action(KeyAction::ResumeSuspended, &mut state),
        ControllerEffect::ResumeSuspended
    );
    assert_eq!(
        handle_key_action(KeyAction::DiscardSuspended, &mut state),
        ControllerEffect::DiscardSuspended
    );
}

#[test]
fn default_keymap_maps_core_navigation_and_control_keys() {
    let keymap = Keymap::default();

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(KeyAction::SubmitNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL,)),
        Some(KeyAction::SubmitBacklog)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('c'), KeyModifiers::CONTROL,)),
        Some(KeyAction::CancelInputOrQuit)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('j'), KeyModifiers::CONTROL,)),
        Some(KeyAction::InsertNewline)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('v'), KeyModifiers::CONTROL,)),
        Some(KeyAction::PasteImage)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('o'), KeyModifiers::CONTROL,)),
        Some(KeyAction::TogglePlan)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE)),
        Some(KeyAction::Interrupt)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Up, KeyModifiers::NONE)),
        Some(KeyAction::HistoryPrevious)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Down, KeyModifiers::NONE)),
        Some(KeyAction::HistoryNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::PageUp, KeyModifiers::NONE)),
        Some(KeyAction::ScrollUp)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::PageDown, KeyModifiers::NONE)),
        Some(KeyAction::ScrollDown)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        Some(KeyAction::ReviewPreviousUserInput)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        Some(KeyAction::ReviewPreviousArtifact)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        Some(KeyAction::ReviewNextArtifact)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        Some(KeyAction::FollowLatestArtifact)
    );
}

#[test]
fn configured_image_paste_binding_replaces_the_default() {
    let keymap = Keymap::from_config(&crate::config::TuiKeymapToml {
        paste_image: Some("ctrl+n".to_owned()),
        ..crate::config::TuiKeymapToml::default()
    })
    .expect("configured keymap should validate");

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        Some(KeyAction::PasteImage)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
        None
    );
}

#[test]
fn controller_only_starts_image_paste_from_the_main_composer() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::PasteImage
    );

    state.open_command_palette();
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );
}

#[test]
fn clipboard_image_completion_updates_the_draft_or_reports_a_nonfatal_diagnostic() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("before ");

    apply_clipboard_image_completion(Ok(draft_image(9)), &mut state);
    assert_eq!(state.input_text(), "before [Image #1]");

    apply_clipboard_image_completion(Err("clipboard has no image".to_owned()), &mut state);
    assert_eq!(state.input_text(), "before [Image #1]");
    assert!(matches!(
        state.timeline().last(),
        Some(TimelineItem::Diagnostic { title, body })
            if title == "clipboard_image" && body == "clipboard has no image"
    ));
}

#[test]
fn renderer_highlights_complete_image_placeholders_in_the_composer() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("inspect ");
    state
        .input_mut()
        .insert_image(draft_image(1))
        .expect("image should insert");
    state.insert_input_str(" now");

    let buffer = render_to_buffer(&state, 80, 16);

    assert_eq!(
        find_cell_color(&buffer, "[Image #1]"),
        Some(Color::LightMagenta)
    );
}

#[test]
fn controller_ctrl_j_inserts_newline_without_submitting() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("first");

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        &mut state,
    );
    state.insert_input_str("second");

    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.input_text(), "first\nsecond");
}

#[test]
fn controller_ctrl_p_opens_searchable_command_palette_and_settings() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    let palette = render_to_text(&state, 100, 30);
    assert!(palette.contains("Commands"));
    assert!(palette.contains("Settings"));
    assert!(palette.contains("Follow latest"));

    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    let settings = render_to_text(&state, 100, 30);
    assert!(settings.contains("Settings"));
    assert!(settings.contains("Code theme"));
    assert!(settings.contains("Default provider"));
}

#[test]
fn command_palette_renders_categories_as_single_group_headers() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    let palette = render_to_text(&state, 100, 30);

    assert_eq!(palette.matches("Navigation").count(), 1);
    assert_eq!(palette.matches("Runtime").count(), 1);
    assert_eq!(palette.matches("Session").count(), 1);
}

#[test]
fn command_palette_search_has_no_redundant_brand_and_commands_are_indented() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    let buffer = render_to_buffer(&state, 100, 30);
    let (search_x, search_y) =
        find_text_position(&buffer, "Search commands").expect("search placeholder");
    let (group_x, _) = find_text_position(&buffer, "Navigation").expect("group heading");
    let (command_x, _) = find_text_position(&buffer, "Follow latest").expect("group command");

    assert_ne!(buffer[(search_x.saturating_sub(2), search_y)].symbol(), "M");
    assert!(command_x > group_x);
}

#[test]
fn provider_manager_escape_returns_to_command_palette() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_command_palette();
    state.open_provider_manager(vec![ProviderListItem::new(
        "opencode",
        "OpenCode",
        ConfiguredProviderKind::OpenAiCompatible,
        ProviderConfigSource::Managed,
        Some(merry_provider_openai::OpenAiProtocol::ChatCompletions),
        Some("model-a"),
    )]);

    let effect = handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);

    assert_eq!(effect, ControllerEffect::None);
    assert!(matches!(
        state.overlay(),
        Some(super::overlay::Overlay::CommandPalette(_))
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
        Some(super::overlay::Overlay::Settings(_))
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
        Some(super::overlay::Overlay::Dialog(_))
    ));

    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);
    assert!(matches!(
        state.overlay(),
        Some(super::overlay::Overlay::ProviderManager(_))
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
        Some(super::overlay::Overlay::ModelPicker(_))
    ));
}

#[test]
fn command_palette_uses_a_magenta_selection_surface() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    let buffer = render_to_buffer(&state, 100, 30);
    let selected = find_cell_style(&buffer, "Settings").expect("selected command should render");

    assert_eq!(selected.bg, Some(Color::Rgb(54, 26, 58)));
    assert_eq!(selected.fg, Some(Color::White));
}

#[test]
fn command_palette_displays_configured_shortcuts_instead_of_stale_defaults() {
    let keymap = Keymap::from_config(&crate::config::TuiKeymapToml {
        follow_latest_artifact: Some("ctrl+n".to_owned()),
        ..crate::config::TuiKeymapToml::default()
    })
    .expect("configured keymap should validate");
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        keymap,
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    let palette = render_to_text(&state, 100, 30);
    let follow_latest = palette
        .lines()
        .find(|line| line.contains("Follow latest"))
        .expect("follow latest command should render");

    assert!(follow_latest.contains("Ctrl+N"));
    assert!(!follow_latest.contains("Ctrl+R"));
}

#[test]
fn command_palette_executes_follow_latest_instead_of_only_describing_it() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "old request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran command".to_owned(),
        body: "output".to_owned(),
    });
    state.scroll_timeline_up_by(20);
    state.jump_to_previous_user_input();
    state.select_previous_artifact();
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "follow latest".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }

    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert_eq!(state.timeline_scroll_offset(), 0);
    assert_eq!(state.timeline_review_user_index(), None);
    assert_eq!(state.artifact_review_timeline_index(), None);
    assert!(state.overlay().is_none());
}

#[test]
fn command_palette_and_cursor_fit_a_narrow_terminal() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        &mut state,
    );

    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 32, 12);
    let text = rendered_buffer_text(&buffer);

    assert!(text.contains("Commands"));
    assert!(text.contains("Settings"));
    assert!(cursor.x < 32);
    assert!(cursor.y < 12);
}

#[test]
fn command_palette_keeps_the_selected_command_visible_in_a_short_terminal() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    let mut selected_quit = false;
    for _ in 0..32 {
        selected_quit = matches!(
            state.overlay(),
            Some(Overlay::CommandPalette(palette))
                if palette
                    .visible_commands()
                    .get(palette.selected())
                    .is_some_and(|command| command.command == PaletteCommand::Quit)
        );
        if selected_quit {
            break;
        }
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }
    assert!(
        selected_quit,
        "Quit should remain reachable in the command palette"
    );

    let palette = render_to_text(&state, 40, 12);

    assert!(palette.contains("Quit Merry"));
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
        super::provider_overlay::ProviderFormSeed {
            original_alias: "opencode".to_owned(),
            display_name: "OpenCode Gateway".to_owned(),
            alias: "opencode".to_owned(),
            kind: crate::config::ManagedProviderKind::OpenAiCompatible,
            protocol: Some(merry_provider_openai::OpenAiProtocol::ChatCompletions),
            base_url: "https://gateway.example.test/v1".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
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
        super::provider_overlay::ProviderFormSeed {
            original_alias: "opencode".to_owned(),
            display_name: "OpenCode".to_owned(),
            alias: "opencode".to_owned(),
            kind: crate::config::ManagedProviderKind::OpenAiCompatible,
            protocol: Some(merry_provider_openai::OpenAiProtocol::ChatCompletions),
            base_url: "https://opencode.example.test/v1".to_owned(),
            model: "model-a".to_owned(),
        },
        Default::default(),
    );

    assert!(state.open_provider_form_model_picker("opencode".to_owned(), "OpenCode".to_owned(),));
    assert!(state.select_provider_form_model("model-b"));

    let Some(Overlay::ProviderForm(form)) = state.overlay() else {
        panic!("provider form should be restored");
    };
    assert_eq!(
        form.field(super::provider_overlay::ProviderFormField::Model),
        "model-b"
    );
    assert_eq!(
        form.selected_field(),
        super::provider_overlay::ProviderFormField::Model
    );
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
        form.field(super::provider_overlay::ProviderFormField::DisplayName),
        "Unsaved Provider"
    );
}

#[test]
fn paste_is_routed_to_the_command_palette_instead_of_chat_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    handle_paste_event("settings", &mut state);

    let palette = render_to_text(&state, 100, 30);
    assert!(palette.contains("settings"));
    assert!(palette.contains("Settings"));
    assert_eq!(state.input_text(), "");
}

#[test]
fn command_palette_blocks_mouse_scroll_from_mutating_the_hidden_timeline() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: (0..30)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    handle_mouse_scroll_up(Position::new(1, 1), Size::new(80, 24), &mut state);

    assert_eq!(state.timeline_scroll_offset(), 0);
}

#[test]
fn settings_cycles_code_theme_without_leaking_keys_to_chat_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );

    let settings = render_to_text(&state, 100, 30);
    assert!(settings.contains("Catppuccin Mocha"));
    assert_eq!(state.input_text(), "");
    assert!(matches!(
        effect,
        ControllerEffect::PersistPreferences(preferences)
            if preferences.code_theme == CodeTheme::CatppuccinMocha
    ));
}

#[test]
fn settings_reasoning_change_applies_to_the_current_runtime() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..3 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );

    assert!(matches!(
        effect,
        ControllerEffect::ApplyRuntimePreferences(preferences)
            if preferences.reasoning_effort.is_some()
    ));
    assert_eq!(state.settings_notice(), Some("Applied"));
}

#[test]
fn settings_context_window_editor_applies_to_the_current_runtime() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_settings();
    for _ in 0..4 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    handle_paste_event("128k", &mut state);
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert!(matches!(
        effect,
        ControllerEffect::ApplyRuntimePreferences(preferences)
            if preferences.context_window_tokens == Some(128_000)
    ));
    assert!(render_to_text(&state, 100, 30).contains("128k"));
    assert_eq!(state.settings_notice(), Some("Applied"));
}

#[test]
fn settings_context_window_editor_rejects_zero() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.open_settings();
    for _ in 0..4 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    handle_paste_event("0", &mut state);

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.preferences().context_window_tokens, None);
    assert!(
        state
            .settings_notice()
            .is_some_and(|notice| notice.contains("positive token count"))
    );
}

#[test]
fn settings_compaction_change_applies_to_the_current_runtime() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..5 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );

    assert!(matches!(
        effect,
        ControllerEffect::ApplyRuntimePreferences(preferences)
            if preferences.auto_compaction_enabled == Some(true)
    ));
    assert_eq!(state.settings_notice(), Some("Applied"));
}

#[test]
fn settings_do_not_describe_runtime_changes_as_next_session() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );

    let settings = render_to_text(&state, 100, 30);

    assert!(!settings.contains("next session"));
}

#[test]
fn code_theme_setting_applies_to_existing_code_blocks_immediately() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "```python\ndef greet():\n    return 'hello'\n```".to_owned(),
    });
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        &mut state,
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );

    let buffer = render_to_buffer(&state, 80, 18);
    let keyword = find_cell_style(&buffer, "def").expect("python keyword should render");

    assert_eq!(keyword.fg, Some(Color::Rgb(203, 166, 247)));
}

#[test]
fn shortcuts_opened_from_settings_return_to_settings() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..9 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert!(render_to_text(&state, 100, 30).contains("Command palette"));

    handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state);

    let settings = render_to_text(&state, 100, 30);
    assert!(settings.contains("Code theme"));
    assert!(settings.contains("Default provider"));
}

#[test]
fn settings_model_editor_owns_the_visible_cursor() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..2 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for character in "custom-model".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }

    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 100, 30);
    let (_, editor_row) =
        find_text_position(&buffer, "custom-model").expect("model editor should render");

    assert_eq!(cursor.y, editor_row);
    assert!(cursor.x > 30);
}

#[test]
fn settings_keep_the_selected_row_visible_in_a_short_terminal() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    handle_key_event(
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        &mut state,
    );
    for character in "settings".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut state,
        );
    }
    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    for _ in 0..9 {
        handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut state);
    }

    let settings = render_to_text(&state, 40, 12);

    assert!(settings.contains("Keyboard shortcuts"));
}

#[test]
fn controller_configured_insert_newline_binding_takes_precedence() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::from_config(&crate::config::TuiKeymapToml {
            insert_newline: Some("ctrl+r".to_owned()),
            ..crate::config::TuiKeymapToml::default()
        })
        .unwrap(),
        TuiTheme::default(),
    );

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        &mut state,
    );

    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.input_text(), "\n");
}

#[test]
fn controller_ctrl_c_clears_input_before_quitting_on_empty_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("draft");

    let first = handle_key_event(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &mut state,
    );

    assert_eq!(first, ControllerEffect::None);
    assert_eq!(state.input_text(), "");

    let second = handle_key_event(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &mut state,
    );

    assert_eq!(second, ControllerEffect::Quit);
}

#[test]
fn controller_ctrl_c_quit_confirmation_resets_after_new_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );

    state.insert_input_str("draft");

    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext(text_submission("draft"))
    );
    assert_eq!(state.input_text(), "");

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::Quit
    );
}

#[test]
fn controller_respects_configured_ctrl_c_interrupt_binding() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::from_config(&crate::config::TuiKeymapToml {
            interrupt: Some("ctrl+c".to_owned()),
            ..crate::config::TuiKeymapToml::default()
        })
        .unwrap(),
        TuiTheme::default(),
    );
    state.set_run_state(InteractiveRunState::RunningModel);

    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &mut state,
    );

    assert_eq!(effect, ControllerEffect::Interrupt);
}

#[test]
fn configured_navigation_bindings_take_precedence() {
    let keymap = Keymap::from_config(&crate::config::TuiKeymapToml {
        history_previous: Some("ctrl+p".to_owned()),
        history_next: Some("ctrl+n".to_owned()),
        review_previous_user_input: Some("ctrl+u".to_owned()),
        scroll_up: Some("up".to_owned()),
        scroll_down: Some("down".to_owned()),
        resume_suspended: Some("ctrl+r".to_owned()),
        discard_suspended: Some("ctrl+d".to_owned()),
        ..crate::config::TuiKeymapToml::default()
    })
    .unwrap();

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL,)),
        Some(KeyAction::HistoryPrevious)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL,)),
        Some(KeyAction::HistoryNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Up, KeyModifiers::NONE)),
        Some(KeyAction::ScrollUp)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Down, KeyModifiers::NONE)),
        Some(KeyAction::ScrollDown)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('u'), KeyModifiers::CONTROL,)),
        Some(KeyAction::ReviewPreviousUserInput)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('r'), KeyModifiers::CONTROL,)),
        Some(KeyAction::ResumeSuspended)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('d'), KeyModifiers::CONTROL,)),
        Some(KeyAction::DiscardSuspended)
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
        SemanticColor::Assistant,
        SemanticColor::Selection,
        SemanticColor::ToolKeyword,
        SemanticColor::Command,
        SemanticColor::CodeBackground,
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

#[test]
fn projector_renders_assistant_text_as_primary_timeline_item() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::AssistantMessage {
            text: "hello from assistant".to_owned(),
            artifact: text_artifact("assistant-1"),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        &[TimelineItem::Assistant {
            text: "hello from assistant".to_owned()
        }]
    );
}

#[test]
fn projector_rebuilds_resume_transcript_history() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    let call = pending_call_with_args(
        "call-read",
        "workspace_read_file",
        json!({"path": "hello_world.py"}),
    );
    let result = ToolCallResult::succeeded(
        call.id().clone(),
        ArtifactRef::new(
            ArtifactId::new("read-result").expect("valid artifact id"),
            ArtifactKind::Json,
        ),
    );

    projector.apply_transcript_item(
        SessionTranscriptItem::UserMessage {
            text: "看看 hello_world.py".to_owned(),
            images: Vec::new(),
        },
        &mut state,
    );
    projector.apply_transcript_item(
        SessionTranscriptItem::AssistantText {
            text: "我先读一下文件。".to_owned(),
        },
        &mut state,
    );
    projector.apply_transcript_item(SessionTranscriptItem::ToolCall { call }, &mut state);
    projector.apply_transcript_item(
        SessionTranscriptItem::ToolResult {
            call_id: ToolCallId::new("call-read").expect("valid call id"),
            result,
            output: Some(ToolOutput::Json {
                json: json!({
                    "ok": true,
                    "tool": "workspace_read_file",
                    "path": "hello_world.py",
                    "content": "print('hi')\n",
                    "bytes": 12,
                    "truncated": false
                })
                .to_string(),
            }),
        },
        &mut state,
    );

    assert!(matches!(
        &state.timeline()[0],
        TimelineItem::User { text, lane: QueuedInputLane::Next } if text == "看看 hello_world.py"
    ));
    assert!(matches!(
        &state.timeline()[1],
        TimelineItem::Assistant { text } if text == "我先读一下文件。"
    ));
    assert!(matches!(
        &state.timeline()[2],
        TimelineItem::ExpandedDetail { title, body, focus_body }
            if title == "Read workspace_read_file path=hello_world.py"
                && body.contains("print('hi')")
                && focus_body.contains("print('hi')")
    ));
}

#[test]
fn projector_updates_streaming_assistant_delta_until_final_message() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::AssistantMessageDelta {
            delta: "hel".to_owned(),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::AssistantMessageDelta {
            delta: "lo".to_owned(),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [TimelineItem::Assistant {
            text: "hello".to_owned()
        }]
    );

    projector.apply(
        RuntimeEvent::AssistantMessage {
            text: "hello final".to_owned(),
            artifact: text_artifact("assistant-final"),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [TimelineItem::Assistant {
            text: "hello final".to_owned()
        }]
    );
}

#[test]
fn projector_resets_streaming_assistant_after_terminal_error() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::AssistantMessageDelta {
            delta: "partial".to_owned(),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::RunFailed {
            diagnostic: ErrorInfo::new("model_protocol", "stream failed").unwrap(),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::AssistantMessageDelta {
            delta: "fresh".to_owned(),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [
            TimelineItem::Assistant {
                text: "partial".to_owned()
            },
            TimelineItem::Diagnostic {
                title: "model_protocol".to_owned(),
                body: "stream failed".to_owned()
            },
            TimelineItem::Assistant {
                text: "fresh".to_owned()
            }
        ]
    );
}

#[test]
fn projector_replaces_compaction_progress_with_a_durable_timeline_trace() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::CompactionStarted { source: source() },
        &mut state,
    );
    assert_eq!(
        state.timeline(),
        [TimelineItem::Muted {
            title: "Compacting".to_owned(),
            detail: "preparing checkpoint".to_owned(),
        }]
    );

    projector.apply(
        RuntimeEvent::CompactionCompleted {
            checkpoint_id: "checkpoint-session-42".to_owned(),
            covered_history_item_count: 48,
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [TimelineItem::Muted {
            title: "Compacted".to_owned(),
            detail: "48 history items · checkpoint-session-42".to_owned(),
        }]
    );
}

#[test]
fn projector_replaces_compaction_progress_with_failure() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::CompactionStarted { source: source() },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::RunFailed {
            diagnostic: ErrorInfo::new("auto_compaction", "compaction response was invalid")
                .unwrap(),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [TimelineItem::Diagnostic {
            title: "compaction failed".to_owned(),
            body: "compaction response was invalid".to_owned(),
        }]
    );
}

#[test]
fn projector_replaces_compaction_progress_with_cancellation() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::CompactionStarted { source: source() },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::RunCancelled {
            diagnostic: ErrorInfo::new("run_cancelled", "cancelled by user").unwrap(),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [TimelineItem::Muted {
            title: "Compaction cancelled".to_owned(),
            detail: "cancelled by user".to_owned(),
        }]
    );
}

#[test]
fn renderer_makes_diagnostic_code_and_reason_visible_in_the_timeline() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Diagnostic {
        title: "auto_compaction".to_owned(),
        body: "compaction state error: compaction window is stale".to_owned(),
    });

    let rendered = render_to_text(&state, 120, 24);

    assert!(rendered.contains("! Error"));
    assert!(rendered.contains("auto_compaction"));
    assert!(rendered.contains("compaction window is stale"));
}

#[test]
fn projector_keeps_successful_non_patch_tool_compact_and_expands_patch_tool() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-read", "workspace_read_file"),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-read").unwrap(),
                text_artifact("read-output"),
            ),
            output: Some(ToolOutput::Text {
                text: "file contents".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-patch", WORKSPACE_PATCH_TOOL),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-patch").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Text {
                text: "--- a/src/lib.rs\n+++ b/src/lib.rs\n+new line".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 2);
    assert!(!matches!(state.timeline()[0], TimelineItem::Muted { .. }));
    assert!(matches!(state.timeline()[1], TimelineItem::Expanded { .. }));
}

#[test]
fn projector_shows_the_specific_schema_violation_for_failed_tool_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-read-plan", "read_plan"),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::failed(
                ToolCallId::new("call-read-plan").unwrap(),
                text_artifact("read-plan-schema-error"),
                ErrorInfo::new(
                    "tool_input_schema_invalid",
                    "tool arguments did not match the registered input schema",
                )
                .unwrap(),
            ),
            output: Some(ToolOutput::Json {
                json: json!({
                    "ok": false,
                    "tool": "read_plan",
                    "error": {
                        "code": "tool_input_schema_invalid",
                        "message": "tool arguments did not match the registered input schema",
                        "violations": [{
                            "path": "$",
                            "schema_path": "/additionalProperties",
                            "message": "Additional properties are not allowed ('include_leases' was unexpected)"
                        }]
                    },
                    "retry": {
                        "instruction": "Remove unsupported fields and call read_plan again."
                    }
                })
                .to_string(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::Diagnostic { title, body } =
        state.timeline().last().expect("failed tool exists")
    else {
        panic!("schema failure should replace the pending row with a diagnostic");
    };
    assert_eq!(title, "Tool read_plan -> failed");
    assert!(body.contains("include_leases"));
    assert!(body.contains("Remove unsupported fields"));
}

#[test]
fn projector_describes_runtime_control_tools() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-subagents",
                "spawn_subagents",
                json!({
                    "tasks": [
                        {"task": "inspect runtime"},
                        {"task": "inspect TUI"}
                    ],
                    "max_concurrency": 2
                }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-checkpoint",
                "merry_read_checkpoint_ref",
                json!({"ref": "prior-c1"}),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-wait",
                "wait_subagents",
                json!({"agent_ids": ["a1", "a2"], "mode": "all", "timeout_ms": 30000}),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-cancel",
                "cancel_subagents",
                json!({"agent_ids": ["a1", "a2"]}),
            ),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [
            TimelineItem::Muted {
                title: "Delegated".to_owned(),
                detail: "spawn_subagents max_concurrency=2 tasks=[{\"task\":\"inspect runtime\"},{\"task\":\"inspect TUI\"}]".to_owned(),
            },
            TimelineItem::Muted {
                title: "Retrieved".to_owned(),
                detail: "merry_read_checkpoint_ref ref=prior-c1".to_owned(),
            },
            TimelineItem::Muted {
                title: "Waited".to_owned(),
                detail: "wait_subagents agent_ids=[\"a1\",\"a2\"] mode=all timeout_ms=30000".to_owned(),
            },
            TimelineItem::Muted {
                title: "Cancelled".to_owned(),
                detail: "cancel_subagents agent_ids=[\"a1\",\"a2\"]".to_owned(),
            }
        ]
    );
}

#[test]
fn projector_keeps_the_real_name_for_unknown_tools() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-custom",
                "custom_lookup",
                json!({"source": "docs", "limit": 2}),
            ),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [TimelineItem::Muted {
            title: "Tool".to_owned(),
            detail: "custom_lookup limit=2 source=docs".to_owned(),
        }]
    );
}

#[test]
fn projector_renders_generic_tool_arguments_and_completed_result() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-custom",
                "custom_lookup",
                json!({"source": "docs", "limit": 2}),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-custom").unwrap(),
                text_artifact("custom-output"),
            ),
            output: Some(ToolOutput::Text {
                text: "2 matching documents".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::Expanded { title, body } = &state.timeline()[0] else {
        panic!("generic tool result should replace its pending row");
    };
    assert!(title.contains("custom_lookup"));
    assert!(title.contains("source=docs"));
    assert!(title.contains("limit=2"));
    assert!(body.contains("2 matching documents"));
}

#[test]
fn projector_expands_tool_batches_in_model_order() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallBatchStarted {
            batch: pending_batch(
                "batch-1",
                vec![
                    pending_call_with_args(
                        "call-first",
                        "workspace_read_file",
                        json!({"path": "first.rs"}),
                    ),
                    pending_call_with_args(
                        "call-second",
                        "workspace_list_dir",
                        json!({"path": "src"}),
                    ),
                ],
            ),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [
            TimelineItem::Muted {
                title: "Read".to_owned(),
                detail: "workspace_read_file path=first.rs".to_owned(),
            },
            TimelineItem::Muted {
                title: "Listed".to_owned(),
                detail: "workspace_list_dir path=src".to_owned(),
            },
        ]
    );
}

#[test]
fn projector_keeps_non_patch_tool_results_compact_without_raw_json() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-read", "workspace_read_file"),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-read").unwrap(),
                text_artifact("read-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"workspace_read_file","path":"AGENTS.md","bytes":19704,"content":"large raw content"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::ExpandedDetail {
        title,
        body,
        focus_body,
    } = &state.timeline()[0]
    else {
        panic!("read tool result should expand to a compact preview");
    };
    assert_eq!(title, "Read workspace_read_file");
    assert!(!body.contains("AGENTS.md:1"));
    assert!(body.contains("large raw content"));
    assert!(focus_body.contains("large raw content"));
    assert!(!body.contains(r#""content":"#));
}

#[test]
fn projector_shows_tool_call_arguments_without_completed_noise() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-read",
                "workspace_read_file",
                json!({ "path": "AGENTS.md" }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-read").unwrap(),
                text_artifact("read-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"workspace_read_file","path":"AGENTS.md","bytes":19704,"content":"large raw content"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::ExpandedDetail {
        title,
        body,
        focus_body,
    } = &state.timeline()[0]
    else {
        panic!("read tool call should expand to a compact preview");
    };
    assert_eq!(title, "Read workspace_read_file path=AGENTS.md");
    assert!(!body.contains("AGENTS.md:1"));
    assert!(body.contains("large raw content"));
    assert!(focus_body.contains("large raw content"));
    assert!(!body.contains("completed"));
}

#[test]
fn renderer_shows_tool_result_preview_below_tool_call() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Tool custom_lookup source=docs limit=2 -> succeeded".to_owned(),
        body: "2 matching documents".to_owned(),
    });

    let rendered = render_to_text(&state, 120, 24);

    assert!(rendered.contains("custom_lookup"));
    assert!(rendered.contains("source=docs"));
    assert!(rendered.contains("2 matching documents"));
}

#[test]
fn renderer_limits_tool_result_preview_to_five_lines() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Tool custom_lookup source=docs -> succeeded".to_owned(),
        body:
            "first result\nsecond result\nthird result\nfourth result\nfifth result\nsixth result"
                .to_owned(),
    });

    let rendered = render_to_text(&state, 120, 24);

    assert!(rendered.contains("first result"));
    assert!(rendered.contains("second result"));
    assert!(rendered.contains("third result"));
    assert!(rendered.contains("fourth result"));
    assert!(rendered.contains("fifth result"));
    assert!(!rendered.contains("sixth result"));
}

#[test]
fn projector_expands_read_file_output_for_focus_review() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-read",
                "workspace_read_file",
                json!({ "path": "hello_world.py" }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-read").unwrap(),
                text_artifact("read-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"workspace_read_file","path":"hello_world.py","bytes":22,"content":"print(\"Hello, Merry!\")\n"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::ExpandedDetail {
        title,
        body,
        focus_body,
    } = &state.timeline()[0]
    else {
        panic!("read_file result should expand so Focus can show content");
    };
    assert_eq!(title, "Read workspace_read_file path=hello_world.py");
    assert!(!body.contains("hello_world.py:1"));
    assert!(body.contains("print(\"Hello, Merry!\")"));
    assert!(focus_body.contains("print(\"Hello, Merry!\")"));
}

#[test]
fn projector_expands_list_dir_output_for_focus_review() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args("call-list", "workspace_list_dir", json!({ "path": "." })),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-list").unwrap(),
                text_artifact("list-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"workspace_list_dir","path":".","entries":[{"name":"Cargo.toml","path":"Cargo.toml","kind":"file"},{"name":"crates","path":"crates","kind":"directory"}],"truncated":false}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::ExpandedDetail {
        title,
        body,
        focus_body,
    } = &state.timeline()[0]
    else {
        panic!("list_dir result should expand so Focus can show entries");
    };
    assert_eq!(title, "Listed workspace_list_dir path=.");
    assert!(body.contains("Cargo.toml"));
    assert!(body.contains("crates/"));
    assert!(focus_body.contains("Cargo.toml"));
    assert!(focus_body.contains("crates/"));
}

#[test]
fn projector_renders_mcp_tools_with_server_and_tool_label() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-mcp-search",
                "mcp_openaiDeveloperDocs_search_openai_docs",
                json!({ "query": "Responses API streaming" }),
            ),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::Muted { title, detail } = &state.timeline()[0] else {
        panic!("MCP tool call should render as a compact muted line");
    };
    assert_eq!(title, "MCP");
    assert_eq!(
        detail,
        "openaiDeveloperDocs/search_openai_docs query=\"Responses API streaming\""
    );
}

#[test]
fn projector_renders_list_dir_as_listed_path_without_field_label() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args("call-list", "workspace_list_dir", json!({ "path": "." })),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::Muted { title, detail } = &state.timeline()[0] else {
        panic!("list tool call should render as a compact muted line");
    };
    assert_eq!(title, "Listed");
    assert_eq!(detail, "workspace_list_dir path=.");
}

#[test]
fn projector_renders_process_calls_as_ran_with_preview() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-process",
                "run_process",
                json!({ "command": "python3 hello_world.py", "cwd": "." }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-process").unwrap(),
                text_artifact("process-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"kind":"process_action","status":0,"stdout":{"text":"hello world\n","bytes":12,"truncated":false},"stderr":{"text":"","bytes":0,"truncated":false}}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::ExpandedDetail {
        title,
        body,
        focus_body,
    } = &state.timeline()[0]
    else {
        panic!("process call should expand with output preview");
    };
    assert_eq!(title, "Ran python3 hello_world.py (.)");
    assert_eq!(body, "  hello world");
    assert_eq!(focus_body, "  hello world");
}

#[test]
fn projector_renders_nonzero_process_exit_as_command_result() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-process",
                "run_process",
                json!({ "command": "cargo test -p merry-cli", "cwd": "." }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::failed(
                ToolCallId::new("call-process").unwrap(),
                text_artifact("process-output"),
                ErrorInfo::new("process_action_failed", "process exited with code 101")
                    .unwrap(),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"kind":"process_action","status":{"kind":"exited","code":101},"stdout":{"text":"","bytes":0,"truncated":false},"stderr":{"text":"error: test failed\nrerun with --exact\n","bytes":38,"truncated":false}}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::ExpandedDetail {
        title,
        body,
        focus_body,
    } = &state.timeline()[0]
    else {
        panic!("nonzero process exit should remain a command result");
    };
    assert_eq!(title, "Ran cargo test -p merry-cli (.) -> exit 101");
    assert_eq!(body, "  error: test failed\n  rerun with --exact");
    assert_eq!(focus_body, "  error: test failed\n  rerun with --exact");
    assert!(!body.contains("process_action_failed"));
}

#[test]
fn projector_keeps_process_start_failure_as_diagnostic() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-process-start-failure",
                "run_process",
                json!({ "command": "missing-command", "cwd": "." }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::failed(
                ToolCallId::new("call-process-start-failure").unwrap(),
                text_artifact("process-start-failure-output"),
                ErrorInfo::new("process_action_failed", "failed to start process").unwrap(),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"kind":"process_action","status":{"kind":"failed_to_start"},"stdout":{"text":"","bytes":0,"truncated":false},"stderr":{"text":"","bytes":0,"truncated":false}}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::Diagnostic { title, body } = &state.timeline()[0] else {
        panic!("process start failure should remain a diagnostic");
    };
    assert_eq!(title, "Ran missing-command (.) -> failed");
    assert!(body.contains("process_action_failed"));
    assert!(body.contains("failed to start process"));
}

#[test]
fn projector_shows_permission_allow_rationale_on_success() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-permission-success",
                "request_permissions",
                json!({
                    "requested": { "network": true },
                    "for_action": { "kind": "process", "command": "cargo test", "cwd": null }
                }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-permission-success").unwrap(),
                text_artifact("permission-success-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"kind":"process_action","permission_profile_id":"process.permission_request.approved","permission_review":{"source":"model","risk":"low","user_authorization":"high","rationale":"The exact command is grounded in the user's task."}}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::ExpandedDetail {
        body, focus_body, ..
    } = &state.timeline()[0]
    else {
        panic!("successful permission call should show an expanded admission result");
    };
    assert!(body.contains("allowed: The exact command is grounded in the user's task."));
    assert!(body.contains("profile: process.permission_request.approved"));
    assert_eq!(body, focus_body);
}

#[test]
fn projector_keeps_process_preview_lines_intact() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-process",
                "run_process",
                json!({ "command": "cargo test", "cwd": "." }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-process").unwrap(),
                text_artifact("process-output"),
            ),
            output: Some(ToolOutput::Json {
                json: format!(
                    r#"{{"kind":"process_action","status":0,"stdout":{{"text":"{}\n","bytes":160,"truncated":false}},"stderr":{{"text":"","bytes":0,"truncated":false}}}}"#,
                    "x".repeat(150)
                ),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::ExpandedDetail { body, .. } = &state.timeline()[0] else {
        panic!("process call should expand with output preview");
    };
    assert!(!body.contains("stdout:"));
    assert!(body.contains(&format!("  {}", "x".repeat(150))));
}

#[test]
fn projector_limits_process_preview_to_five_output_lines() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-process-five-lines",
                "run_process",
                json!({ "command": "printf output", "cwd": "." }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-process-five-lines").unwrap(),
                text_artifact("process-five-lines-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"kind":"process_action","status":0,"stdout":{"text":"one\ntwo\nthree\nfour\nfive\nsix\n","bytes":28,"truncated":false},"stderr":{"text":"stderr should not be previewed\n","bytes":28,"truncated":false}}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::ExpandedDetail {
        body, focus_body, ..
    } = &state.timeline()[0]
    else {
        panic!("process call should expand with output preview");
    };
    assert_eq!(body, "  one\n  two\n  three\n  four\n  five");
    assert!(focus_body.contains("  six"));
    assert!(focus_body.contains("  stderr should not be previewed"));
}

#[test]
fn focus_panel_shows_full_process_output_when_preview_is_compact() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-process",
                "run_process",
                json!({ "command": "ls", "cwd": "." }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-process").unwrap(),
                text_artifact("process-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"kind":"process_action","status":0,"stdout":{"text":"AGENTS.md\nCargo.lock\nCargo.toml\nREADME.md\ncrates\ntarget\n","bytes":56,"truncated":false},"stderr":{"text":"","bytes":0,"truncated":false}}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::ExpandedDetail { body, .. } = &state.timeline()[0] else {
        panic!("process call should expand with output preview");
    };
    assert!(body.contains("AGENTS.md"));
    assert!(body.contains("Cargo.toml"));
    assert!(body.contains("README.md"));
    assert!(body.contains("crates"));
    assert!(!body.contains("target"));

    state.select_previous_artifact();
    let text = render_to_text(&state, 180, 24);

    assert!(text.contains("command ls"));
    assert!(text.contains("AGENTS.md"));
    assert!(text.contains("Cargo.lock"));
    assert!(text.contains("Cargo.toml"));
    assert!(text.contains("README.md"));
    assert!(text.contains("crates"));
    assert!(text.contains("target"));
}

#[test]
fn projector_projects_workspace_patch_using_patch_tool_format() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    let patch = "\
*** Begin Patch
*** Update File: crates/merry-cli/src/tui/render.rs
     let old = true;
-    lines.push(old);
+    lines.push(new);
*** End Patch";

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-patch",
                WORKSPACE_PATCH_TOOL,
                json!({ "patch": patch }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-patch").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"workspace_patch","changes":[{"path":"crates/merry-cli/src/tui/render.rs","hunks":1,"bytes_before":120,"bytes_after":121,"lines":[{"kind":"context","old_line":10,"new_line":10,"text":"    let old = true;"},{"kind":"remove","old_line":11,"text":"    lines.push(old);"},{"kind":"add","new_line":11,"text":"    lines.push(new);"}]}]}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::Patch { changes } = &state.timeline()[0] else {
        panic!("workspace patch result should render as a patch view");
    };
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "crates/merry-cli/src/tui/render.rs");
    assert_eq!(changes[0].added, 1);
    assert_eq!(changes[0].removed, 1);
    assert_eq!(
        changes[0].lines,
        vec![
            PatchLineView::context("    let old = true;", Some(10)),
            PatchLineView::remove("    lines.push(old);", Some(11)),
            PatchLineView::add("    lines.push(new);", Some(11)),
        ]
    );
}

#[test]
fn projector_projects_workspace_patch_add_file_line_numbers() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    let patch = "*** Begin Workspace Patch\n*** Add File: hello.txt\n+hello\n+world\n*** End Workspace Patch";
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-add-file",
                WORKSPACE_PATCH_TOOL,
                json!({ "patch": patch }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-add-file").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"workspace_patch","changes":[{"path":"hello.txt","hunks":1,"bytes_before":0,"bytes_after":12}]}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::Patch { changes } = &state.timeline()[0] else {
        panic!("workspace add patch should render as a patch view");
    };
    assert_eq!(
        changes[0].lines,
        vec![
            PatchLineView::add("hello", Some(1)),
            PatchLineView::add("world", Some(2)),
        ]
    );
}

#[test]
fn projector_derives_patch_line_numbers_from_hunk_headers() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    let patch = "\
*** Begin Patch
*** Update File: hello_world.py
@@ -4,2 +4,2 @@
 def build_message():
-    return \"hello   world\"
+    return \"hello world\"
*** End Patch";

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-numbered-patch",
                WORKSPACE_PATCH_TOOL,
                json!({ "patch": patch }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-numbered-patch").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"workspace_patch","changes":[{"path":"hello_world.py","hunks":1,"bytes_before":209,"bytes_after":222}]}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::Patch { changes } = &state.timeline()[0] else {
        panic!("workspace patch result should render as a patch view");
    };
    assert_eq!(
        changes[0].lines,
        vec![
            PatchLineView::context("def build_message():", Some(4)),
            PatchLineView::remove("    return \"hello   world\"", Some(5)),
            PatchLineView::add("    return \"hello world\"", Some(5)),
        ]
    );
}

#[test]
fn projector_compacts_failed_tool_result_without_raw_artifact_json() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-permission",
                "request_permissions",
                json!({
                    "requested": { "network": true },
                    "for_action": { "command": "cargo test", "cwd": null }
                }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::failed(
                ToolCallId::new("call-permission").unwrap(),
                text_artifact("permission-output"),
                ErrorInfo::new(
                    "permission_review_failed",
                    "permission review failed: provider stream Protocol: stream line must start with data:",
                )
                .unwrap(),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"error":{"code":"permission_review_failed","message":"permission review failed: provider stream Protocol: stream line must start with data:"},"review":{"source":"model","risk":"unknown","user_authorization":"unknown","rationale":"The approval reviewer could not establish a trustworthy decision."},"guidance":{"kind":"permission_review_failed","message":"Do not assume the requested capability was granted."},"status":"review_failed","tool_call_id":"call-permission"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::Diagnostic { title, body } = &state.timeline()[0] else {
        panic!("failed tool should replace its pending row with a compact diagnostic");
    };
    assert!(title.contains("-> failed"));
    assert!(body.contains("permission_review_failed"));
    assert!(body.contains("The approval reviewer could not establish a trustworthy decision."));
    assert!(body.contains("Do not assume the requested capability was granted."));
    assert!(!body.contains("\"tool_call_id\""));
    assert!(!body.contains("call-permission"));
}

#[test]
fn projector_replaces_pending_row_with_failed_result_instead_of_leaving_stale_row() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-custom",
                "custom_lookup",
                json!({"source": "docs", "limit": 2}),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::failed(
                ToolCallId::new("call-custom").unwrap(),
                text_artifact("custom-failure"),
                ErrorInfo::new("custom_lookup_failed", "no matching documents found").unwrap(),
            ),
            output: Some(ToolOutput::Text {
                text: "no matching documents found".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline().len(),
        1,
        "failed generic tool should replace its pending row, not leave a stale row"
    );
    let TimelineItem::Diagnostic { title, body } = &state.timeline()[0] else {
        panic!("failed generic tool should replace the pending row with a diagnostic");
    };
    assert_eq!(title, "Tool custom_lookup limit=2 source=docs -> failed");
    assert!(body.contains("custom_lookup_failed"));
    assert!(body.contains("no matching documents found"));

    let buffer = render_to_buffer(&state, 120, 24);
    assert_eq!(find_cell_color(&buffer, "Error"), Some(Color::LightRed));

    state.select_previous_artifact();
    let focus = focus_panel_view(&state);
    assert_eq!(focus.tone, FocusPanelTone::Error);
    assert!(matches!(focus.body, FocusPanelBody::Text { .. }));
}

#[test]
fn projector_replaces_failed_patch_row_without_leaving_stale_pending_row() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    let patch = "\
*** Begin Patch
*** Update File: crates/merry-cli/src/tui/render.rs
-    lines.push(old);
+    lines.push(new);
*** End Patch";

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-patch-fail",
                WORKSPACE_PATCH_TOOL,
                json!({ "patch": patch }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::failed(
                ToolCallId::new("call-patch-fail").unwrap(),
                text_artifact("patch-failure"),
                ErrorInfo::new(
                    "workspace_patch_preimage_mismatch",
                    "preimage text was not found in the target file",
                )
                .unwrap(),
            ),
            output: Some(ToolOutput::Json {
                json: json!({
                    "ok": false,
                    "tool": "workspace_patch",
                    "error": {
                        "code": "workspace_patch_preimage_mismatch",
                        "message": "preimage text was not found in the target file"
                    },
                    "recovery": {
                        "instruction": "Read the current file content and retry with a matching preimage."
                    }
                })
                .to_string(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline().len(),
        1,
        "failed patch tool should replace its pending row, not leave a stale row"
    );
    let TimelineItem::Diagnostic { title, body } = &state.timeline()[0] else {
        panic!("failed patch tool should replace the pending row with a diagnostic");
    };
    assert!(title.starts_with("Patch workspace_patch"));
    assert!(title.contains("crates/merry-cli/src/tui/render.rs"));
    assert!(title.ends_with("-> failed"));
    assert!(body.contains("workspace_patch_preimage_mismatch"));
    assert!(body.contains("preimage text was not found"));
    assert!(body.contains("Read the current file content and retry"));
    assert!(
        !body.contains("*** Begin Patch"),
        "failed patch body must not leak the full patch payload"
    );
}

#[test]
fn renderer_shows_workspace_patch_as_edited_block() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "crates/merry-cli/src/tui/render.rs".to_owned(),
            added: 1,
            removed: 1,
            hunks: 1,
            bytes_before: Some(120),
            bytes_after: Some(121),
            lines: vec![
                PatchLineView::context("    let old = true;", Some(20)),
                PatchLineView::remove("    lines.push(old);", Some(21)),
                PatchLineView::add("    lines.push(new);", Some(21)),
            ],
        }],
    });
    state.select_previous_artifact();

    let text = render_to_text(&state, 180, 16);

    assert!(text.contains("Edited crates/merry-cli/src/tui/render.rs (+1 -1)"));
    assert!(text.contains("    let old = true;"));
    assert!(text.contains("-    lines.push(old);"));
    assert!(text.contains("+    lines.push(new);"));
    assert!(!text.contains("\"changes\""));
}

#[test]
fn projector_expands_workspace_patch_by_tool_name() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-patch", WORKSPACE_PATCH_TOOL),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-patch").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"tool":"workspace_patch","status":"applied"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    assert!(matches!(state.timeline()[0], TimelineItem::Expanded { .. }));
}

#[test]
fn projector_keeps_diff_like_non_patch_output_muted() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call("call-read", "workspace_read_file"),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-read").unwrap(),
                text_artifact("read-output"),
            ),
            output: Some(ToolOutput::Text {
                text: "--- a/src/lib.rs\n+++ b/src/lib.rs\n+not a patch result".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    assert!(matches!(state.timeline()[0], TimelineItem::Expanded { .. }));
}

#[test]
fn projector_updates_queue_preview_and_usage_without_timeline_noise() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::QueuedInputsChanged {
            inputs: merry_core::QueuedInputsView {
                next: vec![merry_core::QueuedInputView {
                    text: "urgent".to_owned(),
                    lane: merry_core::QueuedInputLane::Next,
                    position: 0,
                }],
                suspended: vec![],
                backlog: vec![],
            },
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::UsageUpdated {
            usage: SessionUsage {
                total: ModelUsage::with_details(20_000, Some(18_000), 1_000, None, 21_000),
                last: ModelUsage::with_details(20_000, Some(18_000), 1_000, None, 21_000),
                context: Some(UsageContextWindow {
                    resolved_model_window_tokens: 64_000,
                    effective_window_tokens: 60_800,
                    source: ContextWindowSource::Fallback,
                }),
                compaction: Some(CompactionUsageWindow {
                    auto_compaction_enabled: true,
                    dynamic_body_estimated_tokens: Some(20_200),
                    body_budget_tokens: 56_792,
                    soft_water_tokens: 46_792,
                    hard_water_tokens: 54_792,
                }),
            },
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.queue_preview().next[0].text, "urgent");
    assert!(state.timeline().is_empty());
    assert!(state.status_text().contains("ctx 20.2k/54.8k"));
    assert!(state.status_text().contains("win 64k fallback"));
    assert!(state.status_text().contains("cache 90%"));
    assert!(
        state
            .status_text()
            .contains("last in 20k out 1k | total 21k tok")
    );
}

#[test]
fn narrow_header_preserves_context_pressure_before_secondary_usage() {
    let mut state = TuiState::new(
        "/home/locez/source/rust/merry".into(),
        "gpt-5.6-sol xhigh".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.set_usage(SessionUsage {
        total: ModelUsage::with_details(2_627_620, Some(2_045_312), 31_541, None, 2_659_161),
        last: ModelUsage::with_details(29_021, Some(28_160), 1_301, None, 30_322),
        context: Some(UsageContextWindow {
            resolved_model_window_tokens: 64_000,
            effective_window_tokens: 60_800,
            source: ContextWindowSource::Fallback,
        }),
        compaction: Some(CompactionUsageWindow {
            auto_compaction_enabled: true,
            dynamic_body_estimated_tokens: Some(20_200),
            body_budget_tokens: 56_792,
            soft_water_tokens: 46_792,
            hard_water_tokens: 54_792,
        }),
    });

    let rendered = render_to_text(&state, 72, 16);

    assert!(rendered.contains("ctx 20.2k/54.8k"));
    assert!(!rendered.contains("total 2659.1k"));
}

#[test]
fn narrow_header_counts_wide_characters_when_preserving_context() {
    let mut state = TuiState::new(
        "/home/用户/项目/非常长的中文目录/merry".into(),
        "模型-gpt-5.6-超高".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.set_usage(SessionUsage {
        total: ModelUsage::with_details(20_000, Some(18_000), 1_000, None, 21_000),
        last: ModelUsage::with_details(20_000, Some(18_000), 1_000, None, 21_000),
        context: Some(UsageContextWindow {
            resolved_model_window_tokens: 64_000,
            effective_window_tokens: 60_800,
            source: ContextWindowSource::Fallback,
        }),
        compaction: Some(CompactionUsageWindow {
            auto_compaction_enabled: true,
            dynamic_body_estimated_tokens: Some(20_200),
            body_budget_tokens: 56_792,
            soft_water_tokens: 46_792,
            hard_water_tokens: 54_792,
        }),
    });

    let rendered = render_to_text(&state, 48, 16);

    assert!(rendered.contains("ctx 20.2k/54.8k"));
}

#[test]
fn status_text_compacts_large_usage_counts_but_keeps_last_in_out_visible() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    state.set_usage(SessionUsage {
        total: ModelUsage::new(12_000, 1_248),
        last: ModelUsage::new(11_000, 1_000),
        context: None,
        compaction: None,
    });

    assert!(
        state
            .status_text()
            .contains("last in 11k out 1k | total 13.2k tok")
    );
}

#[test]
fn status_text_shows_model_reasoning_effort() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-5.5".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.set_reasoning_effort_label(Some("medium".to_owned()));

    assert!(state.status_text().contains("gpt-5.5 medium"));
}

#[test]
fn status_text_shows_compact_merry_shuttle_and_elapsed_while_running() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let now = Instant::now();

    state.set_run_state(InteractiveRunState::RunningModel);
    let frames = [
        (0, "[M··]"),
        (100, "[·M·]"),
        (200, "[··M]"),
        (300, "[·M·]"),
        (400, "[M··]"),
    ];
    for (elapsed_ms, expected) in frames {
        state.set_active_run_started_at_for_test(now - Duration::from_millis(elapsed_ms));
        let status = state.interaction_status_text_at(now);
        assert_eq!(status.split_whitespace().next(), Some(expected));
    }

    state.set_active_run_started_at_for_test(now - Duration::from_secs(37));
    assert!(
        state
            .interaction_status_text_at(now)
            .contains("Running model (37s)")
    );
}

#[test]
fn status_text_uses_quiet_ready_label_when_waiting() {
    let state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    let status = state.interaction_status_text();
    assert_eq!(status, "Ready");
    assert!(!status.contains("Running"));
    assert!(!status.contains("[M"));
}

#[test]
fn interaction_status_keeps_completed_run_elapsed_after_returning_ready() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let started = Instant::now();

    state.set_run_state_at(InteractiveRunState::RunningModel, started);
    state.set_run_state_at(
        InteractiveRunState::WaitingForInput,
        started + Duration::from_secs(42),
    );

    assert_eq!(state.interaction_status_text(), "Ready  last run 42s");
}

#[test]
fn projector_projects_accepted_user_input_into_timeline() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    projector.apply(
        RuntimeEvent::QueuedInputAccepted {
            lane: QueuedInputLane::Next,
            inputs: vec![QueuedInputView {
                text: "查一下 baidu.com".to_owned(),
                lane: QueuedInputLane::Next,
                position: 0,
            }],
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        &[TimelineItem::User {
            text: "查一下 baidu.com".to_owned(),
            lane: QueuedInputLane::Next,
        }]
    );
}

#[test]
fn projector_confirms_local_echo_without_duplicate_user_line() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

    state.push_local_user_echo("same".to_owned(), QueuedInputLane::Next);
    state.push_local_user_echo("same".to_owned(), QueuedInputLane::Next);
    projector.apply(
        RuntimeEvent::QueuedInputAccepted {
            lane: QueuedInputLane::Next,
            inputs: vec![QueuedInputView {
                text: "same".to_owned(),
                lane: QueuedInputLane::Next,
                position: 0,
            }],
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        &[
            TimelineItem::User {
                text: "same".to_owned(),
                lane: QueuedInputLane::Next,
            },
            TimelineItem::User {
                text: "same".to_owned(),
                lane: QueuedInputLane::Next,
            },
        ]
    );
}

#[test]
fn renderer_shows_status_timeline_queue_and_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "assistant says hello".to_owned(),
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![QueuedInputView {
            text: "backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });
    state.input_mut().insert_char('h');
    state.input_mut().insert_char('i');

    let text = render_to_text(&state, 79, 24);

    assert!(text.contains("Ready"));
    assert!(text.contains("gpt-test"));
    assert!(text.contains("assistant says hello"));
    assert!(text.contains("Next"));
    assert!(text.contains("next item"));
    assert!(text.contains("Backlog"));
    assert!(text.contains("backlog item"));
    assert!(text.contains("hi"));
}

#[test]
fn renderer_uses_one_timeline_without_permanent_side_rails() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "make the TUI distinct".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran cargo test -p merry-cli".to_owned(),
        body: "ok".to_owned(),
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "queued next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![QueuedInputView {
            text: "queued backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });

    let text = render_to_text(&state, 180, 32);

    assert!(text.contains("merry"));
    assert!(text.contains("make the TUI distinct"));
    assert!(text.contains("Ran cargo test -p merry-cli"));
    assert!(text.contains("queued next item"));
    assert!(text.contains("queued backlog item"));
    assert!(!text.contains("CHAT"));
    assert!(!text.contains("FOCUS"));
    assert!(!text.contains("RUN"));
}

#[test]
fn renderer_keeps_medium_terminal_focused_on_the_timeline() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "review layout".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Muted {
        title: "Read".to_owned(),
        detail: "AGENTS.md".to_owned(),
    });

    let text = render_to_text(&state, 140, 28);

    assert!(text.contains("review layout"));
    assert!(text.contains("Read AGENTS.md"));
    assert!(!text.contains("FOCUS"));
    assert!(!text.contains("RUN"));
}

#[test]
fn renderer_keeps_reviewed_artifact_visible_while_index_shows_newer_items() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran first command".to_owned(),
        body: "first output".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran second command".to_owned(),
        body: "second output".to_owned(),
    });
    state.select_previous_artifact();
    state.select_previous_artifact();

    let text = render_to_text(&state, 180, 28);

    assert!(text.contains("command first command"));
    assert!(text.contains("first output"));
    assert!(!text.contains("stdout"));
    assert!(text.contains("Ran second command"));
    assert!(!text.contains("RUN"));
}

#[test]
fn detail_opens_read_content_without_replacing_the_wide_timeline() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Read hello_world.py".to_owned(),
        body: "print(\"Hello, Merry!\")".to_owned(),
    });
    state.select_previous_artifact();

    let text = render_to_text(&state, 180, 28);

    assert!(text.contains("Read hello_world.py"));
    assert!(text.contains("print(\"Hello, Merry!\")"));
    assert!(!text.contains("FOCUS"));

    let chat_text = text
        .lines()
        .map(|line| line.chars().take(72).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!chat_text.contains("print(\"Hello, Merry!\")"));
}

#[test]
fn focus_read_file_preserves_code_indentation_and_highlights_by_extension() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Read hello_world.py".to_owned(),
        body: "def build_greeting(name: str) -> str:\n    period = \"morning\"\n    return f\"Good {period}, {name}!\"".to_owned(),
    });
    state.select_previous_artifact();

    let buffer = render_to_buffer(&state, 180, 28);
    let text = rendered_buffer_text(&buffer);

    assert!(text.contains("    period = \"morning\""), "{text}");
    let keyword_style = find_cell_style(&buffer, "def").expect("python keyword should render");
    assert!(
        keyword_style.fg.is_some(),
        "read file focus should syntax-highlight known source extensions"
    );
}

#[test]
fn focus_list_dir_renders_entries_with_semantic_colors() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Listed .".to_owned(),
        body: "Cargo.toml\ncrates/\n.hidden".to_owned(),
    });
    state.select_previous_artifact();

    let buffer = render_to_buffer(&state, 180, 28);

    assert_eq!(find_cell_color(&buffer, "Cargo.toml"), Some(Color::White));
    assert_eq!(find_cell_color(&buffer, "crates/"), Some(Color::LightBlue));
    assert_eq!(find_cell_color(&buffer, ".hidden"), Some(Color::DarkGray));
}

#[test]
fn focus_panel_scrolls_independently_from_chat_timeline() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "chat anchor".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Listed .".to_owned(),
        body: (0..30)
            .map(|index| format!("entry-{index}.txt"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    state.select_previous_artifact();

    let bottom = render_to_text(&state, 180, 24);
    state.scroll_focus_down_by(10);
    let scrolled = render_to_text(&state, 180, 24);

    assert!(bottom.contains("entry-0.txt"), "{bottom}");
    assert!(!bottom.contains("entry-20.txt"), "{bottom}");
    assert!(!scrolled.contains("entry-0.txt"), "{scrolled}");
    assert!(scrolled.contains("entry-10.txt"), "{scrolled}");
    assert_eq!(state.timeline_scroll_offset(), 0);
    assert!(scrolled.contains("chat anchor"));
}

#[test]
fn renderer_keeps_bottom_queue_on_narrow_terminal() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "narrow next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });

    let text = render_to_text(&state, 79, 24);

    assert!(!text.contains("FOCUS"));
    assert!(!text.contains("RUN"));
    assert!(text.contains("queue"));
    assert!(text.contains("narrow next item"));
    assert!(text.contains("M"));
    assert!(!text.contains("input"));
}

#[test]
fn narrow_chat_shows_read_result_preview() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Read hello_world.py".to_owned(),
        body: "print(\"Hello, Merry!\")".to_owned(),
    });

    let text = render_to_text(&state, 79, 24);

    assert!(text.contains("Read hello_world.py"));
    assert!(text.contains("print(\"Hello, Merry!\")"));
}

#[test]
fn standard_width_uses_detail_as_the_content_surface() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Read hello_world.py".to_owned(),
        body: "print(\"Hello, Merry!\")".to_owned(),
    });
    state.select_previous_artifact();

    let text = render_to_text(&state, 100, 24);

    assert!(text.contains("Read hello_world.py"));
    assert!(text.contains("print(\"Hello, Merry!\")"));
    assert!(!text.contains("CHAT"));
    assert!(!text.contains("FOCUS"));
    assert!(!text.contains("RUN"));
}

#[test]
fn renderer_preserves_user_message_newlines() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "1234\n换行测试".to_owned(),
        lane: QueuedInputLane::Next,
    });

    let text = render_to_text(&state, 80, 16);

    assert!(text.contains("▌ 1234"));
    assert!(text.contains("▌ 换 行 测 试"));
    assert!(!text.contains("user:"));
}

#[test]
fn renderer_places_terminal_cursor_inside_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.input_mut().insert_str("a你b");
    state
        .input_mut()
        .handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

    let (_buffer, cursor) = render_to_buffer_and_cursor(&state, 80, 16);

    assert_eq!(cursor.x, 4);
    assert_eq!(cursor.y, 13);
}

#[test]
fn renderer_places_terminal_cursor_on_multiline_input_row() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("first");
    state.insert_input_newline();
    state.insert_input_str("second");

    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 80, 16);

    assert!(rendered_buffer_text(&buffer).contains("first"));
    assert!(rendered_buffer_text(&buffer).contains("second"));
    assert_eq!(cursor.x, 7);
    assert_eq!(cursor.y, 13);
}

#[test]
fn renderer_shows_user_input_in_timeline() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "查一下 baidu.com".to_owned(),
        lane: QueuedInputLane::Next,
    });

    let buffer = render_to_buffer(&state, 80, 16);
    let text = rendered_buffer_text(&buffer);

    assert!(text.contains("▌ 查"));
    assert!(!text.contains("user:"));
    assert!(text.contains("baidu.com"));
    let accent_style = find_cell_style(&buffer, "▌").expect("user accent should render");
    let body_style = find_cell_style(&buffer, "baidu.com").expect("user body should render");
    assert_eq!(accent_style.fg, Some(Color::LightMagenta));
    assert!(accent_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(body_style.fg, Some(Color::White));
}

#[test]
fn renderer_hides_empty_queue_panel_to_preserve_timeline_space() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "latest local echo".to_owned(),
        lane: QueuedInputLane::Next,
    });

    let text = render_to_text(&state, 80, 16);

    assert!(text.contains("latest local echo"));
    assert!(!text.contains("queue"));
    assert!(!text.contains("Next"));
    assert!(!text.contains("Suspended"));
    assert!(!text.contains("Backlog"));
}

#[test]
fn renderer_scrolls_timeline_viewport() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    for index in 0..12 {
        state.push_timeline_item(TimelineItem::Assistant {
            text: format!("line {index}"),
        });
    }

    let bottom = render_to_text(&state, 48, 12);
    assert!(bottom.contains("line 11"));
    assert!(!bottom.contains("line 0"));

    state.scroll_timeline_up();
    state.scroll_timeline_up();
    state.scroll_timeline_up();
    let scrolled = render_to_text(&state, 48, 12);
    assert!(scrolled.contains("line 10"));
    assert!(!scrolled.contains("line 11"));
}

#[test]
fn cockpit_wide_timeline_scroll_still_changes_chat_viewport() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    for index in 0..20 {
        state.push_timeline_item(TimelineItem::Assistant {
            text: format!("assistant line {index}"),
        });
    }

    let bottom = render_to_text(&state, 180, 24);
    state.scroll_timeline_up_by(10);
    let scrolled = render_to_text(&state, 180, 24);

    assert!(bottom.contains("assistant line 19"));
    assert_ne!(bottom, scrolled);
    assert!(state.timeline_scroll_offset() >= 10);
}

#[test]
fn renderer_review_user_input_starts_viewport_at_selected_user_turn() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "first answer".to_owned(),
    });
    state.push_timeline_item(TimelineItem::User {
        text: "second request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "second answer".to_owned(),
    });

    state.jump_to_previous_user_input();
    let second = render_to_text(&state, 80, 18);
    assert!(second.contains("▌ second request"));
    assert!(!second.contains("first request"));

    state.jump_to_previous_user_input();
    let first = render_to_text(&state, 80, 18);
    assert!(first.contains("▌ first request"));
    assert!(first.contains("first answer"));

    state.exit_timeline_review();
    let bottom = render_to_text(&state, 80, 18);
    assert!(bottom.contains("second answer"));
}

#[test]
fn cockpit_ctrl_u_review_still_jumps_between_user_turns() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::User {
        text: "first request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "first answer".to_owned(),
    });
    state.push_timeline_item(TimelineItem::User {
        text: "second request".to_owned(),
        lane: QueuedInputLane::Next,
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "second answer".to_owned(),
    });

    handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state);
    let second = render_to_text(&state, 180, 24);
    handle_key_action(KeyAction::ReviewPreviousUserInput, &mut state);
    let first = render_to_text(&state, 180, 24);
    handle_key_action(KeyAction::SubmitNext, &mut state);
    let bottom = render_to_text(&state, 180, 24);

    assert!(second.contains("second request"));
    assert!(first.contains("first request"));
    assert!(bottom.contains("second answer"));
    assert_eq!(state.timeline_review_user_index(), None);
}

#[test]
fn cockpit_wide_cursor_remains_inside_input_with_cjk_text() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("你好 cockpit");

    let (buffer, cursor) = render_to_buffer_and_cursor(&state, 180, 24);
    let input_label = find_text_position(&buffer, "M").expect("input brand should render");

    assert!(cursor.y > input_label.1);
    assert!(cursor.x > input_label.0);
    assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), " ");
}

#[test]
fn renderer_applies_configured_semantic_theme_colors() {
    let theme = TuiTheme::from_config(&crate::config::TuiThemeToml {
        status: Some("red".to_owned()),
        muted: Some("blue".to_owned()),
        focus: Some("magenta".to_owned()),
        assistant: Some("white".to_owned()),
        tool_keyword: Some("cyan".to_owned()),
        command: Some("light_blue".to_owned()),
        warning: Some("yellow".to_owned()),
        success: Some("green".to_owned()),
        diff_add: Some("green".to_owned()),
        diff_delete: Some("yellow".to_owned()),
        ..crate::config::TuiThemeToml::default()
    })
    .expect("theme config should validate");
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        theme,
    );
    state.push_timeline_item(TimelineItem::Muted {
        title: "tool".to_owned(),
        detail: "read".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran cargo test --package 'hello world' && printf $HOME (.)".to_owned(),
        body: "ok".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "patch".to_owned(),
            added: 1,
            removed: 1,
            hunks: 1,
            bytes_before: Some(8),
            bytes_after: Some(6),
            lines: vec![
                PatchLineView::remove("removed", Some(1)),
                PatchLineView::add("added", Some(1)),
            ],
        }],
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "queued".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });
    state.select_previous_artifact();

    let buffer = render_to_buffer(&state, 180, 24);

    assert_eq!(find_cell_color(&buffer, "merry"), Some(Color::Red));
    assert_eq!(find_cell_color(&buffer, "/repo"), Some(Color::LightBlue));
    assert_eq!(find_cell_color(&buffer, "gpt-test"), Some(Color::Cyan));
    assert_eq!(find_cell_color(&buffer, "tool"), Some(Color::Blue));
    assert_eq!(find_cell_color(&buffer, "Ran"), Some(Color::Cyan));
    assert_eq!(find_cell_color(&buffer, "cargo"), Some(Color::LightBlue));
    assert_eq!(find_cell_color(&buffer, "--package"), Some(Color::Magenta));
    assert_eq!(
        find_cell_color(&buffer, "'hello world'"),
        Some(Color::LightBlue)
    );
    assert_eq!(find_cell_color(&buffer, "&&"), Some(Color::Cyan));
    assert_eq!(find_cell_color(&buffer, "$HOME"), Some(Color::Green));
    assert_eq!(find_cell_color(&buffer, "patch"), Some(Color::Cyan));
    assert_eq!(find_cell_color(&buffer, "+added"), Some(Color::Green));
    assert_eq!(find_cell_color(&buffer, "-removed"), Some(Color::Yellow));
    assert_eq!(find_cell_color(&buffer, "Next"), Some(Color::Magenta));
    assert_eq!(find_cell_color(&buffer, "queued"), Some(Color::Blue));
}

#[test]
fn renderer_uses_colored_header_bar_and_branded_input() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("draft");

    let buffer = render_to_buffer(&state, 80, 16);
    let text = rendered_buffer_text(&buffer);
    let path_style = find_cell_style(&buffer, "/repo").expect("workspace path should render");
    let model_style = find_cell_style(&buffer, "gpt-test").expect("model should render");
    let usage_style = find_cell_style(&buffer, "usage -").expect("usage should render");
    let input_brand_style = find_cell_style(&buffer, "M").expect("input brand should render");
    let input_style = find_cell_style(&buffer, "draft").expect("input text should render");

    assert!(!text.contains("input"));
    assert_eq!(path_style.fg, Some(Color::LightBlue));
    assert_eq!(model_style.fg, Some(Color::LightCyan));
    assert!(model_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(usage_style.fg, Some(Color::White));
    assert!(usage_style.add_modifier.contains(Modifier::DIM));
    assert_eq!(path_style.bg, Some(Color::Rgb(54, 26, 58)));
    assert_eq!(model_style.bg, Some(Color::Rgb(54, 26, 58)));
    assert_eq!(input_brand_style.fg, Some(Color::LightMagenta));
    assert!(input_brand_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(input_style.fg, Some(Color::White));
}

#[test]
fn renderer_highlights_inline_code_spans() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "call `build_message()` now".to_owned(),
    });

    let buffer = render_to_buffer(&state, 80, 16);
    let assistant_style =
        find_cell_style(&buffer, "call").expect("assistant text should be rendered");
    let code_style =
        find_cell_style(&buffer, "build_message()").expect("inline code text should be rendered");

    assert_eq!(assistant_style.fg, Some(Color::White));
    assert_eq!(code_style.fg, Some(Color::LightMagenta));
    assert_eq!(code_style.bg, Some(Color::Reset));
    assert!(code_style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn renderer_renders_assistant_markdown_strong_without_markers() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "**hello** world".to_owned(),
    });

    let text = render_to_text(&state, 80, 16);
    assert!(!text.contains("**hello**"));
    assert!(text.contains("hello"));

    let buffer = render_to_buffer(&state, 80, 16);
    let strong_style = find_cell_style(&buffer, "hello").expect("strong text should render");
    assert_eq!(strong_style.fg, Some(Color::LightMagenta));
    assert!(strong_style.add_modifier.contains(Modifier::BOLD));
    assert!(!strong_style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn renderer_does_not_underline_cjk_strong_text_in_nested_lists() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "- **橙子**\n  - **血橙**\n  - **脐橙**\n    - **赣南脐橙**".to_owned(),
    });

    let rendered = render_to_text(&state, 80, 18);
    let buffer = render_to_buffer(&state, 80, 18);
    for text in ["橙", "血", "脐", "赣"] {
        let style = find_cell_style(&buffer, text)
            .unwrap_or_else(|| panic!("strong CJK list item should render: {text}\n{rendered}"));
        assert_eq!(style.fg, Some(Color::LightMagenta));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(!style.add_modifier.contains(Modifier::UNDERLINED));
    }
}

#[test]
fn renderer_keeps_plain_assistant_markdown_text_white() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "plain text with **strong text**".to_owned(),
    });

    let buffer = render_to_buffer(&state, 80, 16);
    let plain_style = find_cell_style(&buffer, "plain text").expect("plain text should render");
    let trailing_style =
        find_cell_style(&buffer, "with").expect("trailing plain text should render");

    assert_eq!(plain_style.fg, Some(Color::White));
    assert_eq!(trailing_style.fg, Some(Color::White));
}

#[test]
fn renderer_renders_assistant_markdown_heading_as_title_block() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "# Result\nBody text".to_owned(),
    });

    let text = render_to_text(&state, 80, 16);
    assert!(!text.contains("# Result"));
    assert!(text.contains("Result"));
    assert!(text.contains("Body text"));

    let buffer = render_to_buffer(&state, 80, 16);
    let heading_style = find_cell_style(&buffer, "Result").expect("heading should render");
    let body_style = find_cell_style(&buffer, "Body text").expect("body should render");
    assert_eq!(heading_style.fg, Some(Color::LightMagenta));
    assert!(heading_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(body_style.fg, Some(Color::White));
}

#[test]
fn renderer_renders_assistant_markdown_table_with_header_and_cells() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "| Name | Status |\n| --- | --- |\n| API | **OK** |\n| UI | pending |".to_owned(),
    });

    let text = render_to_text(&state, 100, 18);
    assert!(text.contains("Name"));
    assert!(text.contains("Status"));
    assert!(text.contains("API"));
    assert!(text.contains("OK"));
    assert!(text.contains("UI"));
    assert!(text.contains("pending"));
    assert!(!text.contains("| --- |"));

    let buffer = render_to_buffer(&state, 100, 18);
    let header_style = find_cell_style(&buffer, "Status").expect("table header should render");
    let strong_style = find_cell_style(&buffer, "OK").expect("strong table cell should render");
    assert_eq!(header_style.fg, Some(Color::LightMagenta));
    assert!(header_style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(strong_style.fg, Some(Color::LightMagenta));
    assert!(strong_style.add_modifier.contains(Modifier::BOLD));
    assert!(!strong_style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn renderer_renders_assistant_markdown_strikethrough_as_muted_crossed_text() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "before ~~removed~~ after".to_owned(),
    });

    let text = render_to_text(&state, 80, 16);
    assert!(!text.contains("~~removed~~"));
    assert!(text.contains("removed"));

    let buffer = render_to_buffer(&state, 80, 16);
    let removed_style =
        find_cell_style(&buffer, "removed").expect("strikethrough text should render");
    assert_eq!(removed_style.fg, Some(Color::DarkGray));
    assert!(removed_style.add_modifier.contains(Modifier::CROSSED_OUT));
}

#[test]
fn renderer_prefixes_wrapped_assistant_markdown_block_quotes() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "> quoted text wraps onto another visual line".to_owned(),
    });

    let text = render_to_text(&state, 34, 16);
    let quote_lines = text
        .lines()
        .filter(|line| line.trim_start().starts_with(">"))
        .collect::<Vec<_>>();
    assert!(
        quote_lines.len() >= 2,
        "wrapped quote should keep quote prefix on each visual line:\n{text}"
    );

    let buffer = render_to_buffer(&state, 34, 16);
    let quote_style = find_cell_style(&buffer, ">").expect("quote marker should render");
    assert_eq!(quote_style.fg, Some(Color::LightMagenta));
}

#[test]
fn renderer_keeps_assistant_markdown_link_url_visible() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "[OpenAI](https://openai.com) and [https://example.com](https://example.com)"
            .to_owned(),
    });

    let text = render_to_text(&state, 100, 16);
    assert!(text.contains("OpenAI"));
    assert!(text.contains("https://openai.com"));
    assert!(text.contains("https://example.com"));
    assert_eq!(text.matches("https://example.com").count(), 1);

    let buffer = render_to_buffer(&state, 100, 16);
    let label_style = find_cell_style(&buffer, "OpenAI").expect("link label should render");
    let url_style = find_cell_style(&buffer, "https://openai.com").expect("link url should render");
    assert_eq!(label_style.fg, Some(Color::LightBlue));
    assert_eq!(url_style.fg, Some(Color::LightBlue));
    assert!(url_style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn renderer_preserves_assistant_message_newlines() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Done:\n- changed `hello_world.py`\n- verified output".to_owned(),
    });

    let text = render_to_text(&state, 80, 18);
    assert!(text.contains("Done:"));
    assert!(text.contains("- changed  hello_world.py"));
    assert!(text.contains("- verified output"));
    assert!(!text.contains("Done:- changed"));
}

#[test]
fn renderer_renders_assistant_fenced_code_blocks_without_fence_markers() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Output:\n```text\nhello world\n```\nDone".to_owned(),
    });

    let buffer = render_to_buffer(&state, 80, 18);
    let text = rendered_buffer_text(&buffer);
    assert!(text.contains("Output:"));
    assert!(text.contains("▎ hello world"));
    assert!(text.contains("Done"));
    assert!(!text.contains("```"));

    let rail_style = find_cell_style(&buffer, "▎").expect("code rail should render");
    let code_style = find_cell_style(&buffer, "hello world").expect("code should render");
    let (_, code_row) = find_text_position(&buffer, "hello world").expect("code should render");
    assert_eq!(rail_style.fg, Some(Color::LightMagenta));
    assert_eq!(rail_style.bg, Some(Color::Rgb(40, 36, 42)));
    assert_eq!(code_style.bg, Some(Color::Rgb(40, 36, 42)));
    assert_eq!(
        buffer[(40, code_row)].style().bg,
        Some(Color::Rgb(40, 36, 42))
    );
}

#[test]
fn renderer_repeats_code_rail_on_wrapped_visual_lines() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "```text\nalpha beta gamma delta epsilon\n```".to_owned(),
    });

    let text = render_to_text(&state, 20, 18);
    let code_lines = text.lines().filter(|line| line.contains('▎')).count();

    assert!(
        code_lines >= 2,
        "wrapped code should repeat the rail:\n{text}"
    );
}

#[test]
fn renderer_keeps_inline_code_atomic_when_wrapping_assistant_text() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "prefix text `hello world` suffix".to_owned(),
    });

    let text = render_to_text(&state, 24, 18);
    assert!(text.contains(" hello world "));
    assert!(!text.contains("hello \nworld"));
}

#[test]
fn renderer_wraps_assistant_text_on_word_boundaries() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "alpha beta gamma delta".to_owned(),
    });

    let text = render_to_text(&state, 18, 18);
    assert!(text.contains("alpha beta"));
    assert!(!text.contains("bet\na"));
    assert!(!text.contains("gamm\na"));
    assert!(!text.contains("delt\na"));
}

#[test]
fn renderer_draws_assistant_separator_across_timeline_width() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "done".to_owned(),
    });

    let text = render_to_text(&state, 40, 12);
    assert!(text.lines().any(|line| line == "-".repeat(40)));
}

#[test]
fn renderer_colors_ran_title_and_shows_process_preview() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran python3 hello_world.py (.)".to_owned(),
        body: "hello world".to_owned(),
    });

    let text = render_to_text(&state, 79, 16);
    assert!(text.contains("Ran python3 hello_world.py (.)"));
    assert!(text.contains("hello world"));

    let buffer = render_to_buffer(&state, 79, 16);
    assert_eq!(find_cell_color(&buffer, "Ran"), Some(Color::LightCyan));
    assert_eq!(find_cell_color(&buffer, "python3"), Some(Color::LightBlue));
}

#[test]
fn renderer_highlights_shell_syntax_in_ran_titles() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran git status --short --branch && git diff --check (.)".to_owned(),
        body: "  clean".to_owned(),
    });

    let buffer = render_to_buffer(&state, 120, 16);
    let executable = find_cell_color(&buffer, "git").expect("shell executable should render");
    let option = find_cell_color(&buffer, "--short").expect("shell option should render");
    let operator = find_cell_color(&buffer, "&&").expect("shell operator should render");

    assert_eq!(executable, Color::LightBlue);
    assert_eq!(option, Color::LightMagenta);
    assert_eq!(operator, Color::LightCyan);
    assert_eq!(find_cell_color(&buffer, "."), Some(Color::DarkGray));
}

#[test]
fn renderer_colors_common_tool_title_keywords() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Muted {
        title: "Searched".to_owned(),
        detail: "query=hello_world.py".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Listed .".to_owned(),
        body: "Cargo.toml".to_owned(),
    });

    let buffer = render_to_buffer(&state, 79, 18);

    assert_eq!(find_cell_color(&buffer, "Searched"), Some(Color::LightCyan));
    assert_eq!(find_cell_color(&buffer, "Listed"), Some(Color::LightCyan));
    assert_eq!(
        find_cell_color(&buffer, "query=hello_world.py"),
        Some(Color::White)
    );
}

#[test]
fn renderer_shows_patch_line_numbers_and_diff_backgrounds() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "hello_world.py".to_owned(),
            added: 1,
            removed: 1,
            hunks: 1,
            bytes_before: Some(209),
            bytes_after: Some(222),
            lines: vec![
                PatchLineView::context("def build_message():", Some(4)),
                PatchLineView::remove("    return \"hello   world\"", Some(5)),
                PatchLineView::add("    return \"hello world\"", Some(5)),
            ],
        }],
    });
    state.select_previous_artifact();

    let text = render_to_text(&state, 96, 18);
    assert!(text.contains("   4  def build_message():"));
    assert!(text.contains("   5 -    return \"hello   world\""));
    assert!(text.contains("   5 +    return \"hello world\""));

    let buffer = render_to_buffer(&state, 96, 18);
    let remove_style = find_cell_style(&buffer, "-    return").expect("remove line should render");
    let add_style = find_cell_style(&buffer, "+    return").expect("add line should render");
    assert_eq!(remove_style.fg, Some(Color::LightRed));
    assert_eq!(add_style.fg, Some(Color::LightGreen));
    assert_ne!(remove_style.bg, None);
    assert_ne!(add_style.bg, None);
}

#[test]
fn renderer_opens_latest_patch_in_on_demand_detail() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "hello_world.py".to_owned(),
            added: 1,
            removed: 1,
            hunks: 1,
            bytes_before: Some(20),
            bytes_after: Some(21),
            lines: vec![
                PatchLineView::remove("print('old')", Some(7)),
                PatchLineView::add("print('new')", Some(7)),
            ],
        }],
    });
    state.select_previous_artifact();

    let text = render_to_text(&state, 180, 32);

    assert!(text.contains("patch hello_world.py"));
    assert!(text.contains("Edited hello_world.py (+1 -1)"));
    assert!(text.contains("   7 -print('old')"));
    assert!(text.contains("   7 +print('new')"));
}

#[test]
fn renderer_ellipsizes_queue_items_to_region_width() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "abcdefghijklmnopqrstuvwxyz".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });

    let text = render_to_text(&state, 18, 12);

    assert!(
        text.lines()
            .any(|line| line.contains("  1. ") && line.contains("..."))
    );
    assert!(!text.contains("abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn queue_preview_truncates_long_content_on_narrow_terminal() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "this queue item is intentionally long enough to exceed the right rail width"
                .to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });

    let text = render_to_text(&state, 50, 24);

    assert!(text.contains("this queue item"));
    assert!(text.contains("..."));
    assert!(!text.contains("exceed the right rail width"));
}

#[test]
fn focus_panel_clips_long_command_output_with_ellipsis() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran cargo test".to_owned(),
        body: (0..40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    state.select_previous_artifact();

    let text = render_to_text(&state, 180, 16);

    assert!(text.contains("command cargo test"));
    assert!(text.contains("..."));
    assert!(!text.contains("line 39"));
}

#[test]
fn very_short_terminal_keeps_input_visible() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("short terminal input");

    let text = render_to_text(&state, 100, 8);

    assert!(text.contains("M"));
    assert!(text.contains("gpt-test"));
}

#[test]
fn renderer_keeps_input_region_stable_when_queue_count_changes() {
    let mut one_item_state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    one_item_state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![],
        backlog: vec![],
    });
    let mut three_lane_state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    three_lane_state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![QueuedInputView {
            text: "suspended item".to_owned(),
            lane: QueuedInputLane::Suspended,
            position: 0,
        }],
        backlog: vec![QueuedInputView {
            text: "backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });

    let one_item_text = render_to_text(&one_item_state, 80, 18);
    let three_lane_text = render_to_text(&three_lane_state, 80, 18);
    let one_item_input_row = one_item_text
        .lines()
        .position(|line| line.contains("M"))
        .expect("one item queue render should show input");
    let three_lane_input_row = three_lane_text
        .lines()
        .position(|line| line.contains("M"))
        .expect("three lane queue render should show input");

    assert_eq!(one_item_input_row, three_lane_input_row);
}

#[test]
fn renderer_keeps_timeline_visible_when_bottom_panes_are_taller_than_short_window() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Assistant {
        text: "latest assistant output".to_owned(),
    });
    state.update_queue_preview(QueuePreview {
        next: vec![QueuedInputView {
            text: "next item".to_owned(),
            lane: QueuedInputLane::Next,
            position: 0,
        }],
        suspended: vec![QueuedInputView {
            text: "suspended item".to_owned(),
            lane: QueuedInputLane::Suspended,
            position: 0,
        }],
        backlog: vec![QueuedInputView {
            text: "backlog item".to_owned(),
            lane: QueuedInputLane::Backlog,
            position: 0,
        }],
    });
    state.insert_input_str("line one\nline two\nline three\nline four\nline five");

    let text = render_to_text(&state, 79, 10);

    assert!(text.contains("latest assistant output"));
    assert!(text.contains("line five"));
    assert!(text.contains("M"));
    assert!(!text.contains("input"));
    assert!(text.contains("gpt-test"));
    let assistant_row = text
        .lines()
        .position(|line| line.contains("latest assistant output"))
        .expect("assistant output should render");
    let input_row = text
        .lines()
        .position(|line| line.contains("M"))
        .expect("input panel should render");
    assert!(input_row > assistant_row, "{text}");
}

fn find_cell_color(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<Color> {
    find_cell_style(buffer, text).and_then(|style| style.fg)
}

fn rendered_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn find_text_position(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
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

fn find_cell_style(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<ratatui::style::Style> {
    let area = buffer.area;
    for y in area.y..area.y + area.height {
        let mut row = String::new();
        for x in area.x..area.x + area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        let Some(start) = row.find(text) else {
            continue;
        };
        let x = area.x + u16::try_from(row[..start].chars().count()).ok()?;
        return Some(buffer[(x, y)].style());
    }
    None
}
