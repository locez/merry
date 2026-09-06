use crate::tui::{
    completion::{CompletionKind, CompletionSources},
    controller::{ControllerEffect, handle_key_event},
    input::TextInput,
    keymap::Keymap,
    render::render_to_text,
    state::TuiState,
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_runtime::SkillMetadata;
use std::{fs, path::PathBuf};

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
