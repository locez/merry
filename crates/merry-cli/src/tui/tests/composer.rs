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
fn path_completion_skips_build_dirs_ranks_basename_and_supports_nested_slashes_and_dotfiles() {
    let temp = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(temp.path().join("target/debug")).expect("mkdir target");
    fs::write(temp.path().join("target/debug/render.rs"), "").expect("write target file");
    fs::create_dir_all(temp.path().join("node_modules/pkg")).expect("mkdir node_modules");
    fs::write(temp.path().join("node_modules/pkg/render.js"), "").expect("write nm file");
    fs::create_dir_all(temp.path().join(".git/objects")).expect("mkdir .git");
    fs::write(temp.path().join(".git/HEAD"), "").expect("write .git/HEAD");
    fs::create_dir_all(temp.path().join(".merry/roles")).expect("mkdir .merry");
    fs::write(temp.path().join(".merry/roles/custom.toml"), "").expect("write role");
    fs::create_dir_all(temp.path().join("a_r/b_e/c_n/d_d/e_e")).expect("mkdir scattered");
    fs::write(temp.path().join("a_r/b_e/c_n/d_d/e_e/f_r.txt"), "").expect("write scattered");
    fs::create_dir_all(temp.path().join("src/tui")).expect("mkdir src/tui");
    fs::write(temp.path().join("src/tui/render.rs"), "").expect("write render.rs");
    fs::write(temp.path().join("build.rs"), "").expect("write build.rs");

    let sources = CompletionSources::from_skill_names(temp.path().to_path_buf(), &[]);

    let render = sources
        .menu_for_input("check @render", "check @render".len(), None)
        .expect("render completion");
    let values = render
        .items()
        .iter()
        .map(|item| item.value())
        .collect::<Vec<_>>();
    assert_eq!(values[0], "src/tui/render.rs");
    assert!(
        values
            .iter()
            .all(|v| !v.starts_with("target/") && !v.starts_with("node_modules/")),
        "build/dependency dirs should be excluded: {values:?}"
    );
    assert!(!render.items()[0].match_positions().is_empty());

    // Files named `build.rs` must not be filtered out by the `build/` directory rule.
    let build_file = sources
        .menu_for_input("@build", "@build".len(), None)
        .expect("build.rs completion");
    assert_eq!(build_file.items()[0].value(), "build.rs");

    // Slashes inside `@` path queries remain active.
    let slashed = sources
        .menu_for_input("edit @src/tui/ren", "edit @src/tui/ren".len(), None)
        .expect("slashed path completion");
    assert_eq!(slashed.items()[0].value(), "src/tui/render.rs");

    // Explicit leading dot queries surface dotfiles while keeping `.git/` ignored.
    let hidden = sources
        .menu_for_input("@.merry", "@.merry".len(), None)
        .expect("hidden path completion");
    assert!(
        hidden
            .items()
            .iter()
            .any(|item| item.value() == ".merry/roles/custom.toml")
    );
    assert!(
        hidden
            .items()
            .iter()
            .all(|item| !item.value().starts_with(".git/"))
    );
}

#[test]
fn skill_completion_supports_fuzzy_name_and_description_matching() {
    let skills = vec![
        SkillMetadata::new(
            "brainstorming",
            "Use for collaborative design work.",
            PathBuf::from("skills/brainstorming/SKILL.md"),
            PathBuf::from("/skills"),
        )
        .expect("valid skill"),
        SkillMetadata::new(
            "frontend-design",
            "Build polished web interfaces.",
            PathBuf::from("skills/frontend-design/SKILL.md"),
            PathBuf::from("/skills"),
        )
        .expect("valid skill"),
    ];
    let sources = CompletionSources::new(std::env::current_dir().expect("cwd"), skills);

    // Acronym fuzzy match on skill name.
    let fd = sources
        .menu_for_input("$fd", "$fd".len(), None)
        .expect("acronym skill match");
    assert_eq!(fd.items()[0].value(), "frontend-design");
    assert!(!fd.items()[0].match_positions().is_empty());

    // Description fallback match when name doesn't match.
    let collab = sources
        .menu_for_input("$collab", "$collab".len(), None)
        .expect("description skill match");
    assert_eq!(collab.items()[0].value(), "brainstorming");
}

#[test]
fn path_completion_supports_absolute_paths_directory_drilldown_and_parent_relative_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let external = temp.path().join("external");
    fs::create_dir_all(&workspace).expect("mkdir workspace");
    fs::create_dir_all(external.join("configs")).expect("mkdir configs");
    fs::write(external.join("configs/app.toml"), "").expect("write app.toml");
    fs::write(external.join("configs/.secret.env"), "").expect("write hidden file");
    fs::write(external.join("notes.md"), "").expect("write notes.md");

    let external_abs = external.to_string_lossy().replace('\\', "/");
    let mut state = TuiState::new(
        workspace,
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    // Absolute directory query fuzzy-matches entries outside the workspace.
    let query = format!("read @{external_abs}/cfg");
    state.insert_input_str(&query);
    let menu = state.completion_menu().expect("absolute path menu");
    assert_eq!(menu.items()[0].value(), format!("{external_abs}/configs/"));
    assert!(!menu.items()[0].match_positions().is_empty());

    // Accepting a directory keeps no trailing space and immediately opens its children.
    let effect = handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(state.input_text(), format!("read @{external_abs}/configs/"));
    let child_menu = state
        .completion_menu()
        .expect("directory completion drills into children");
    assert_eq!(
        child_menu.items()[0].value(),
        format!("{external_abs}/configs/app.toml")
    );

    // Accepting a file appends a trailing space and closes the menu.
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(effect, ControllerEffect::None);
    assert_eq!(
        state.input_text(),
        format!("read @{external_abs}/configs/app.toml ")
    );
    assert!(state.completion_menu().is_none());

    // Hidden files appear when the segment explicitly starts with `.`.
    state.clear_input();
    state.insert_input_str(&format!("@{external_abs}/configs/.sec"));
    let hidden_menu = state.completion_menu().expect("hidden absolute file menu");
    assert_eq!(
        hidden_menu.items()[0].value(),
        format!("{external_abs}/configs/.secret.env")
    );

    // Explicit `../` relative traversal also works cleanly.
    state.clear_input();
    state.insert_input_str("@../external/not");
    let parent_menu = state.completion_menu().expect("parent relative menu");
    assert_eq!(parent_menu.items()[0].value(), "../external/notes.md");
}

#[test]
fn path_completion_escapes_spaces_and_continues_drilling_into_spaced_directories() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let spaced_ws_dir = workspace.join("my docs");
    fs::create_dir_all(&spaced_ws_dir).expect("mkdir workspace spaced dir");
    fs::write(spaced_ws_dir.join("read me.md"), "").expect("write workspace spaced file");

    let external = temp.path().join("external");
    let spaced_ext_dir = external.join("My Folder").join("Sub Dir");
    fs::create_dir_all(&spaced_ext_dir).expect("mkdir external spaced dir");
    fs::write(spaced_ext_dir.join("hello world.txt"), "").expect("write external spaced file");

    let external_abs = external.to_string_lossy().replace('\\', "/");
    let mut state = TuiState::new(
        workspace,
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    // Absolute path with spaces: Tab escapes spaces and keeps drilling into children.
    state.insert_input_str(&format!("inspect @{external_abs}/My"));
    let level1 = state.completion_menu().expect("level 1 spaced dir menu");
    assert_eq!(
        level1.items()[0].value(),
        format!("{external_abs}/My Folder/")
    );

    handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert_eq!(
        state.input_text(),
        format!("inspect @{external_abs}/My\\ Folder/")
    );
    let level2 = state
        .completion_menu()
        .expect("drilling into escaped spaced directory stays open");
    assert_eq!(
        level2.items()[0].value(),
        format!("{external_abs}/My Folder/Sub Dir/")
    );

    // Second level spaced dir -> file with spaces.
    handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert_eq!(
        state.input_text(),
        format!("inspect @{external_abs}/My\\ Folder/Sub\\ Dir/")
    );
    state.insert_input_str("hel");
    let file_menu = state
        .completion_menu()
        .expect("filtering inside nested spaced directory");
    assert_eq!(
        file_menu.items()[0].value(),
        format!("{external_abs}/My Folder/Sub Dir/hello world.txt")
    );

    handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut state,
    );
    assert_eq!(
        state.input_text(),
        format!("inspect @{external_abs}/My\\ Folder/Sub\\ Dir/hello\\ world.txt ")
    );
    assert!(state.completion_menu().is_none());

    // Workspace-relative spaced directory also escapes and keeps completing.
    state.clear_input();
    state.insert_input_str("@my\\ docs/read");
    let ws_menu = state.completion_menu().expect("workspace spaced path menu");
    assert_eq!(ws_menu.items()[0].value(), "my docs/read me.md");
    handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut state);
    assert_eq!(state.input_text(), "@my\\ docs/read\\ me.md ");
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
    assert!(
        text.find("> Cargo.toml").unwrap() < text.find(crate::tui::glyphs::BRAND_RIBBON).unwrap()
    );
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
    input.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    input.insert_str("> ");
    input.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    input.insert_str(" tail");
    input.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    input.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    input.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

    assert_eq!(input.text(), "");
    assert_eq!(input.cursor_byte_index(), 0);

    input.insert_str("ab");
    input.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert_eq!(input.cursor_byte_index(), 1);
    input.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(input.text(), "a");
    input.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert_eq!(input.cursor_byte_index(), 1);
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

    assert_eq!(viewport.text, "a你\n好b");
    assert_eq!(viewport.cursor_row, 1);
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
fn multiline_input_moves_by_visual_row_and_keeps_home_on_the_line() {
    let mut input = TextInput::default();
    input.insert_str("abcdef\nab\nabcdef");
    assert!(input.move_vertical(-1, 20));
    assert_eq!(&input.text()[..input.cursor_byte_index()], "abcdef\nab");
    assert!(input.move_vertical(-1, 20));
    assert_eq!(&input.text()[..input.cursor_byte_index()], "abcdef");
    assert!(!input.move_vertical(-1, 20));

    input.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(input.cursor_byte_index(), 0);
    input.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(input.cursor_byte_index(), "abcdef".len());
    input.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert_eq!(input.text(), "abcdef\nab\nabcdef");
    input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    input.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert_eq!(input.text(), "abcde\nab\nabcdef");
}

#[test]
fn wrapped_input_up_down_follows_the_visible_rows() {
    let mut input = TextInput::default();
    input.insert_str("abcdefghij");
    let viewport = input.viewport_rows(4, 5);
    assert_eq!(viewport.text, "abcd\nefgh\nij");
    assert_eq!(viewport.cursor_row, 2);
    assert_eq!(viewport.cursor_column, 2);

    assert!(input.move_vertical(-1, 4));
    assert_eq!(&input.text()[..input.cursor_byte_index()], "abcdef");
    assert!(input.move_vertical(-1, 4));
    assert_eq!(&input.text()[..input.cursor_byte_index()], "ab");
}

#[test]
fn up_on_the_first_input_row_recalls_history() {
    let mut state = TuiState::new(
        "/repo".into(),
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.record_input_history("earlier");
    state.insert_input_str("alpha\nbeta");
    state.set_composer_columns(40);

    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "alpha\nbeta");
    assert!(state.input_mut().cursor_byte_index() <= "alpha".len());

    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "earlier");
}

#[test]
fn controller_supports_select_all_copy_cut_esc_deselect_and_shift_enter() {
    let mut state = TuiState::new(
        "/repo".into(),
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("hello world");

    // Ctrl+A selects all; Ctrl+C copies selection instead of clearing/quitting.
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            &mut state
        ),
        ControllerEffect::None
    );
    assert!(state.has_input_selection());
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut state
        ),
        ControllerEffect::CopyText("hello world".to_owned())
    );
    assert_eq!(state.input_text(), "hello world");

    // Esc clears composer selection cleanly.
    assert_eq!(
        handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut state),
        ControllerEffect::None
    );
    assert!(!state.has_input_selection());

    // Shift+Enter inserts a newline without submitting.
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            &mut state
        ),
        ControllerEffect::None
    );
    state.insert_input_str("second");
    assert_eq!(state.input_text(), "hello world\nsecond");

    // Cmd+A then Cmd+X cuts everything to clipboard; Ctrl+Z restores it.
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER),
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::SUPER),
            &mut state
        ),
        ControllerEffect::CopyText("hello world\nsecond".to_owned())
    );
    assert_eq!(state.input_text(), "");

    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(state.input_text(), "hello world\nsecond");
}

#[test]
fn composer_renders_selection_highlight_and_supports_mouse_click_and_drag() {
    use crate::tui::{
        controller::handle_mouse_input, render::render_to_buffer, text_interaction::MouseInput,
        theme::SemanticColor,
    };
    use ratatui::layout::{Position, Size};

    let mut state = TuiState::new(
        "/repo".into(),
        "model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.insert_input_str("hello");
    handle_key_event(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        &mut state,
    );

    let size = Size::new(80, 14);
    let buffer = render_to_buffer(&state, size.width, size.height);
    let expected_bg = state.theme().bg_color(SemanticColor::SelectionBackground);

    // Find "hello" in the composer row and verify its cells have SelectionBackground.
    let mut found = None;
    for y in 0..buffer.area.height {
        let mut row = String::new();
        let mut cols = Vec::new();
        for x in 0..buffer.area.width {
            let start = row.len();
            row.push_str(buffer[(x, y)].symbol());
            cols.push((start, x));
        }
        if let Some(byte_idx) = row.find("hello")
            && let Some((_, x)) = cols.into_iter().find(|(start, _)| *start == byte_idx)
        {
            found = Some((x, y));
            for offset in 0..5u16 {
                assert_eq!(buffer[(x + offset, y)].bg, expected_bg.unwrap());
            }
            break;
        }
    }
    let (hx, hy) = found.expect("composer should render 'hello'");

    // Clicking on 'l' (index 2) moves cursor there and clears selection.
    handle_mouse_input(
        MouseInput::Down(Position::new(hx + 2, hy)),
        size,
        &mut state,
    );
    assert_eq!(state.input_mut().cursor_byte_index(), 2);
    assert!(!state.has_input_selection());

    // Dragging to index 5 selects "llo".
    handle_mouse_input(
        MouseInput::Drag(Position::new(hx + 5, hy)),
        size,
        &mut state,
    );
    handle_mouse_input(MouseInput::Up(Position::new(hx + 5, hy)), size, &mut state);
    assert_eq!(state.selected_input_text().as_deref(), Some("llo"));
}
