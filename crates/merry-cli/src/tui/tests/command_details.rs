use crate::tui::{
    command_details::{CapturedOutput, apply_output, load_output},
    controller::{
        ControllerEffect, handle_key_event, handle_mouse_input, handle_mouse_scroll_down,
        handle_mouse_scroll_up,
    },
    keymap::Keymap,
    projector::TuiProjector,
    render::{prepare_viewport, render_to_buffer, render_to_text},
    state::TuiState,
    terminal::write_clipboard_request,
    tests::{pending_call_with_args, source},
    text_interaction::MouseInput,
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry::events::{ErrorInfo, RuntimeEvent, SessionId, ToolCallId, ToolCallResult, ToolOutput};
use merry::runtime::Runtime;
use ratatui::{
    layout::{Position, Size},
    style::Color,
};
use serde_json::json;

fn output_hint() -> String {
    format!("{} output", crate::tui::keymap::alt_chord("T"))
}

fn close_hint() -> String {
    format!("{} / Esc close", crate::tui::keymap::alt_chord("T"))
}

fn state() -> TuiState {
    TuiState::new(
        "/repo".into(),
        "test".into(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

fn completed_command(
    state: &mut TuiState,
    id: &str,
    command: &str,
    status: i64,
    output: &str,
) -> ToolCallId {
    let mut projector = TuiProjector::default();
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                id,
                "run_process",
                json!({"command": command, "cwd": "src"}),
            ),
            source: source(),
        },
        state,
    );
    let call_id = ToolCallId::new(id).unwrap();
    let result = if status == 0 {
        ToolCallResult::succeeded(call_id.clone())
    } else {
        ToolCallResult::failed(
            call_id.clone(),
            ErrorInfo::new("process_action_failed", "command failed").unwrap(),
        )
    };
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result,
            output: Some(ToolOutput::Json {
                json: json!({
                    "kind": "process_action", "status": status,
                    "stdout": {"text": output}, "stderr": {"text": ""},
                })
                .to_string(),
            }),
            source: source(),
        },
        state,
    );
    call_id
}

fn key(state: &mut TuiState, code: KeyCode) -> ControllerEffect {
    handle_key_event(KeyEvent::new(code, KeyModifiers::NONE), state)
}

fn open(state: &mut TuiState) -> ToolCallId {
    let effect = handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT), state);
    let ControllerEffect::LoadCommandOutput(call_id) = effect else {
        panic!("opening a completed command should request its tool call output");
    };
    call_id
}

fn draw(state: &mut TuiState, width: u16, height: u16) -> String {
    prepare_viewport(state, Size::new(width, height));
    render_to_text(state, width, height)
}

#[test]
fn command_details_shortcut_toggles_without_changing_the_draft_or_reading_position() {
    let mut state = state();
    let call_id = completed_command(&mut state, "toggle", "pwd", 0, "");
    state.append_assistant_delta(None, &"context\n".repeat(40));
    state.insert_input_str("keep my draft");
    state.scroll_timeline_up_by(10);
    let before = draw(&mut state, 80, 24);

    for _ in 0..3 {
        assert_eq!(open(&mut state), call_id);
        assert!(draw(&mut state, 80, 24).contains("Loading captured output"));
        assert_eq!(
            handle_key_event(
                KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT),
                &mut state,
            ),
            ControllerEffect::None
        );
        assert!(state.overlay().is_none());
        apply_output(
            &mut state,
            &call_id,
            Ok(CapturedOutput::Text("late output".into())),
        );
        assert_eq!(draw(&mut state, 80, 24), before);
    }
}

#[test]
fn command_output_hint_appears_only_when_completed_output_is_available() {
    let mut state = state();
    assert!(!draw(&mut state, 80, 24).contains(&output_hint()));
    TuiProjector::default().apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args("pending", "run_process", json!({"command": "pwd"})),
            source: source(),
        },
        &mut state,
    );
    assert!(!draw(&mut state, 80, 24).contains(&output_hint()));
    completed_command(&mut state, "ready", "pwd", 0, "");
    for width in [24, 80] {
        assert!(draw(&mut state, width, 24).contains(&output_hint()));
    }
}

#[test]
fn command_output_hint_does_not_overwrite_new_content_navigation() {
    let mut state = state();
    completed_command(&mut state, "hint", "pwd", 0, "");
    state.append_assistant_delta(None, &"context\n".repeat(40));
    state.scroll_timeline_up_by(10);
    draw(&mut state, 80, 24);
    state.append_assistant_delta(None, "new content");

    let wide = draw(&mut state, 80, 24);
    assert!(wide.contains("New content"));
    assert!(wide.contains("Enter latest"));
    assert!(wide.contains(&output_hint()));
    let narrow = draw(&mut state, 40, 24);
    assert!(narrow.contains("New content"));
    assert!(narrow.contains("Enter latest"));
    assert!(!narrow.contains(&output_hint()));
}

#[test]
fn command_details_close_hint_and_feedback_fit_narrow_terminals() {
    let mut state = state();
    completed_command(&mut state, "footer", "pwd", 0, "");
    open(&mut state);
    key(&mut state, KeyCode::Char('y'));
    for width in [24, 40, 80, 120] {
        let rendered = draw(&mut state, width, 24);
        assert!(rendered.contains(&close_hint()), "{rendered}");
        assert!(rendered.contains("↑/↓"), "{rendered}");
        assert!(
            rendered.contains("C/Y copy") || rendered.contains("C copy command"),
            "{rendered}"
        );
        assert!(rendered.contains("Output is not"), "{rendered}");
        assert!(rendered.contains("available yet"), "{rendered}");
    }
}

#[test]
fn command_details_shortcut_does_not_dismiss_unrelated_dialogs() {
    let mut state = state();
    completed_command(&mut state, "dialog", "pwd", 0, "");
    state.show_info_dialog("Keep this dialog", "Unrelated message".into());
    let before = draw(&mut state, 80, 24);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT),
            &mut state,
        ),
        ControllerEffect::None
    );
    assert_eq!(draw(&mut state, 80, 24), before);
}

#[test]
fn hidden_success_output_is_inspectable_and_copy_preserves_the_actual_command() {
    let mut state = state().with_successful_command_output(false);
    let command = "printf '%s' 'line one'\nprintf 'line two'";
    let call_id = completed_command(&mut state, "first", command, 0, "secret preview");
    state.insert_input_str("unfinished draft");
    let compact = draw(&mut state, 100, 26);
    assert!(!compact.contains("secret preview"));
    assert_eq!(open(&mut state), call_id);
    assert!(draw(&mut state, 100, 26).contains("Loading captured output"));
    apply_output(
        &mut state,
        &call_id,
        Ok(CapturedOutput::from_tool_output(ToolOutput::json(
            json!({
                "kind": "process_action", "stdout": {"text": "first line\n\n  indented\nlast line"},
                "stderr": {"text": "an error"},
            })
            .to_string(),
        ))),
    );
    let expanded = draw(&mut state, 100, 26);
    assert!(expanded.contains("Directory: src"));
    assert!(expanded.contains("  indented"));
    assert!(expanded.contains("last line"));
    assert!(expanded.contains("an error"));
    assert_eq!(
        key(&mut state, KeyCode::Char('c')),
        ControllerEffect::CopyText(command.into())
    );
    assert_eq!(
        key(&mut state, KeyCode::Char('y')),
        ControllerEffect::CopyText("first line\n\n  indented\nlast line\nan error".into())
    );
    key(&mut state, KeyCode::Esc);
    assert!(state.overlay().is_none());
    let collapsed = draw(&mut state, 100, 26);
    assert!(collapsed.contains("unfinished draft"));
    assert!(!collapsed.contains("last line"));
    assert!(!state.show_successful_command_output());
}

#[test]
fn command_navigation_ignores_stale_loads_and_keeps_failed_output_accessible() {
    let mut state = state();
    let first = completed_command(&mut state, "first", "pwd", 0, "");
    let second = completed_command(&mut state, "second", "git status", 128, "line one");
    assert_eq!(open(&mut state), second);
    assert_eq!(
        key(&mut state, KeyCode::Left),
        ControllerEffect::LoadCommandOutput(first.clone())
    );
    apply_output(
        &mut state,
        &second,
        Ok(CapturedOutput::Text("STALE".into())),
    );
    let pending = draw(&mut state, 80, 24);
    assert!(pending.contains("Command 1/2"));
    assert!(!pending.contains("STALE"));
    assert_eq!(key(&mut state, KeyCode::Left), ControllerEffect::None);
    assert_eq!(
        key(&mut state, KeyCode::Right),
        ControllerEffect::LoadCommandOutput(second.clone())
    );
    apply_output(
        &mut state,
        &second,
        Ok(CapturedOutput::Text("COMPLETE ERROR".into())),
    );
    let failed = draw(&mut state, 80, 24);
    assert!(failed.contains("Exit: 128"));
    assert!(failed.contains("COMPLETE ERROR"));
    key(&mut state, KeyCode::Esc);
    apply_output(
        &mut state,
        &second,
        Ok(CapturedOutput::Text("late completion".into())),
    );
    assert!(state.overlay().is_none());
}

#[test]
fn full_output_scrolls_beyond_preview_and_u16_limits_without_truncating_text() {
    let mut state = state();
    let call_id = completed_command(&mut state, "large", "cargo test", 0, "");
    open(&mut state);
    let output = format!("{}TAIL-完整-output", "line\n".repeat(66_000));
    apply_output(&mut state, &call_id, Ok(CapturedOutput::Text(output)));
    for width in [24, 80] {
        key(&mut state, KeyCode::End);
        let bottom = draw(&mut state, width, 24);
        assert!(
            bottom
                .split_whitespace()
                .collect::<String>()
                .contains("TAIL-完整-output"),
            "{bottom}"
        );
        assert!(bottom.contains("Esc close"));
        key(&mut state, KeyCode::Home);
        let top = draw(&mut state, width, 24);
        assert!(top.contains("cargo test"));
        assert!(!top.contains("TAIL-完整-output"));
    }
}

#[test]
fn upstream_truncation_binary_text_and_load_failures_are_explicit() {
    let mut state = state();
    let call_id = completed_command(&mut state, "limited", "build", 1, "");
    open(&mut state);
    apply_output(
        &mut state,
        &call_id,
        Ok(CapturedOutput::from_tool_output(ToolOutput::json(
            json!({
                "kind": "process_action", "stdout": {"text": "prefix", "truncated": true, "utf8": false}, "stderr": {"text": ""},
            })
            .to_string(),
        ))),
    );
    let rendered = draw(&mut state, 110, 28);
    assert!(rendered.contains("capture truncated"));
    assert!(rendered.contains("Non-UTF-8"));
    apply_output(&mut state, &call_id, Err("output unavailable".into()));
    assert!(draw(&mut state, 80, 24).contains("Could not load output: output unavailable"));
    assert_eq!(key(&mut state, KeyCode::Char('y')), ControllerEffect::None);
}

#[tokio::test]
async fn output_loader_reads_runtime_tool_outputs_and_reports_missing_records() {
    use futures_util::StreamExt;
    use merry::runtime::{StepContext, StepInput};
    use std::sync::Arc;

    let event = crate::testing::process_tool_call("capture", &["echo", "hi"], Some(".")).unwrap();
    let provider = crate::testing::ScriptedProvider::new(vec![vec![Ok(event)]]);
    let runtime = Runtime::builder(SessionId::new("command-inspection").unwrap())
        .model_provider(Arc::new(provider), crate::testing::model_name())
        .build()
        .unwrap();
    let _events = runtime
        .step(
            StepInput::user_text("run command").unwrap(),
            StepContext::default(),
        )
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let call_id = ToolCallId::new("capture").unwrap();
    runtime
        .submit_tool_result(
            ToolCallResult::succeeded(call_id.clone()),
            ToolOutput::text("exact captured output\n"),
        )
        .await
        .unwrap();
    assert_eq!(
        load_output(
            &merry::coding::CodingSessionHandle::from_runtime(runtime.clone()),
            &call_id,
        )
        .await
        .unwrap()
        .copy_text(),
        "exact captured output\n"
    );
    assert!(
        load_output(
            &merry::coding::CodingSessionHandle::from_runtime(runtime.clone()),
            &ToolCallId::new("missing").unwrap(),
        )
        .await
        .is_err()
    );
}

#[test]
fn copy_uses_encoded_terminal_requests_and_rejects_oversized_payloads() {
    let mut bytes = Vec::new();
    write_clipboard_request(&mut bytes, "hello").unwrap();
    let request = String::from_utf8(bytes).unwrap();
    assert!(request.starts_with("\u{1b}]52;c;"));
    assert!(request.contains("aGVsbG8="));
    let mut bytes = Vec::new();
    write_clipboard_request(&mut bytes, "\u{1b}[31m\n").unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains("\u{1b}[31m"));
    let mut bytes = Vec::new();
    assert!(write_clipboard_request(&mut bytes, &"x".repeat(1_048_577)).is_err());
    assert!(bytes.is_empty());
    let mut buffer = [0_u8; 1];
    assert!(write_clipboard_request(&mut &mut buffer[..], "hello").is_err());
}

#[test]
fn opening_before_any_command_completes_preserves_the_draft() {
    let mut state = state();
    state.insert_input_str("keep my draft");
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT),
        &mut state,
    );
    assert_eq!(effect, ControllerEffect::None);
    assert!(draw(&mut state, 100, 24).contains("No completed commands yet"));
    key(&mut state, KeyCode::Esc);
    assert!(draw(&mut state, 100, 24).contains("keep my draft"));
}

#[test]
fn inspection_toggle_and_submit_shortcuts_respect_configured_bindings() {
    let config: crate::config::TuiKeymapToml =
        toml::from_str("open_command_details = 'ctrl+f'\nsubmit = 'ctrl+b'").unwrap();
    let keymap = Keymap::from_config(&config).unwrap();
    let mut state = TuiState::new("/repo".into(), "test".into(), keymap, TuiTheme::default());
    let call_id = completed_command(&mut state, "configured", "pwd", 0, "");
    let collapsed = draw(&mut state, 80, 24);
    assert!(collapsed.contains("Ctrl+F output"));
    assert!(!collapsed.contains(&output_hint()));
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            &mut state
        ),
        ControllerEffect::LoadCommandOutput(call_id)
    );
    let expanded = draw(&mut state, 80, 24);
    assert!(expanded.contains("Ctrl+F / Esc close"));
    handle_key_event(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        &mut state,
    );
    assert_eq!(draw(&mut state, 80, 24), expanded);
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );
    assert!(state.overlay().is_none());
    assert_eq!(draw(&mut state, 80, 24), collapsed);
    state.scroll_timeline_up_by(10);
    assert!(draw(&mut state, 80, 24).contains("Ctrl+B latest"));
    handle_key_event(
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        &mut state,
    );
    assert!(!state.is_timeline_detached());
}

#[test]
fn reopening_the_same_command_rejects_older_load_completions() {
    use crate::tui::controller::CommandOutputCompletion;
    let mut state = state();
    let call_id = completed_command(&mut state, "reopen", "pwd", 0, "");
    open(&mut state);
    let previous_generation = state.command_details_generation();
    key(&mut state, KeyCode::Esc);
    open(&mut state);
    key(&mut state, KeyCode::Char('y'));
    let generation = state.command_details_generation();
    CommandOutputCompletion::new(
        call_id.clone(),
        generation,
        Ok(CapturedOutput::Text("latest capture".into())),
    )
    .apply(&mut state);
    CommandOutputCompletion::new(call_id, previous_generation, Err("obsolete timeout".into()))
        .apply(&mut state);
    let rendered = draw(&mut state, 100, 24);
    assert!(rendered.contains("latest capture"));
    assert!(!rendered.contains("obsolete timeout"));
    assert!(!rendered.contains("Output is not available yet"));
}

fn find_label(rendered: &str, needle: &str) -> Position {
    for (row, line) in rendered.lines().enumerate() {
        if let Some(byte_offset) = line.find(needle) {
            let col = unicode_width::UnicodeWidthStr::width(&line[..byte_offset]);
            return Position::new(u16::try_from(col).unwrap(), u16::try_from(row).unwrap());
        }
    }
    panic!("label {needle:?} not found in rendered screen:\n{rendered}");
}

#[test]
fn mouse_wheel_stays_within_output_bounds_and_switches_only_on_history_rail() {
    let mut state = state();
    let first = completed_command(&mut state, "first", "pwd", 0, "");
    let second = completed_command(&mut state, "second", "cargo test", 0, "");
    let third = completed_command(&mut state, "third", "git status", 0, "");
    let size = Size::new(80, 20);
    let body_pos = Position::new(50, 10);

    assert_eq!(open(&mut state), third);
    apply_output(&mut state, &third, Ok(CapturedOutput::Text("clean".into())));
    let rendered = draw(&mut state, size.width, size.height);
    assert!(rendered.contains("HISTORY (3/3)"), "{rendered}");

    // Scrolling up or down inside the output pane never jumps across commands.
    assert_eq!(
        handle_mouse_scroll_up(body_pos, size, &mut state),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_scroll_down(body_pos, size, &mut state),
        ControllerEffect::None
    );
    assert!(draw(&mut state, size.width, size.height).contains("Command 3/3"));

    // Hovering the left history rail switches commands directly with the wheel.
    let rail_pos = find_label(&rendered, "HISTORY (3/3)");
    assert_eq!(
        handle_mouse_scroll_up(rail_pos, size, &mut state),
        ControllerEffect::LoadCommandOutput(second.clone())
    );
    let long_output = (1..=60)
        .map(|n| format!("output line {n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    apply_output(&mut state, &second, Ok(CapturedOutput::Text(long_output)));
    draw(&mut state, size.width, size.height);

    // Scrolling aggressively past the bottom of #2 stops cleanly at the bottom without switching.
    for _ in 0..40 {
        assert_eq!(
            handle_mouse_scroll_down(body_pos, size, &mut state),
            ControllerEffect::None
        );
    }
    let at_bottom = draw(&mut state, size.width, size.height);
    assert!(at_bottom.contains("Command 2/3"), "{at_bottom}");
    assert!(at_bottom.contains("output line 60"), "{at_bottom}");

    // Scrolling aggressively past the top of #2 stops cleanly at the top without switching.
    for _ in 0..40 {
        assert_eq!(
            handle_mouse_scroll_up(body_pos, size, &mut state),
            ControllerEffect::None
        );
    }
    let at_top = draw(&mut state, size.width, size.height);
    assert!(at_top.contains("Command 2/3"), "{at_top}");
    assert!(at_top.contains("output line 01"), "{at_top}");

    // History rail wheel still steps backward to #1 and forward to #2 -> #3.
    assert_eq!(
        handle_mouse_scroll_up(rail_pos, size, &mut state),
        ControllerEffect::LoadCommandOutput(first.clone())
    );
    assert_eq!(
        handle_mouse_scroll_down(rail_pos, size, &mut state),
        ControllerEffect::LoadCommandOutput(second.clone())
    );
    assert_eq!(
        handle_mouse_scroll_down(rail_pos, size, &mut state),
        ControllerEffect::LoadCommandOutput(third.clone())
    );
}

#[test]
fn wide_history_rail_supports_click_and_wheel_navigation_and_footer_chips() {
    let mut state = state();
    let first = completed_command(&mut state, "first", "pwd", 0, "");
    let second = completed_command(&mut state, "second", "git diff", 0, "");
    let third = completed_command(&mut state, "third", "cargo check", 1, "");
    let size = Size::new(120, 26);

    assert_eq!(open(&mut state), third);
    apply_output(
        &mut state,
        &third,
        Ok(CapturedOutput::Text("error: build failed".into())),
    );
    let rendered = draw(&mut state, size.width, size.height);
    assert!(rendered.contains("HISTORY (3/3)"), "{rendered}");
    assert!(rendered.contains("[EXIT 1]"), "{rendered}");
    assert!(rendered.contains("[◀ Prev]"), "{rendered}");
    assert!(rendered.contains("[Next ▶]"), "{rendered}");
    assert!(rendered.contains("[Copy cmd]"), "{rendered}");
    assert!(rendered.contains("[Copy out]"), "{rendered}");
    assert!(rendered.contains("[Close]"), "{rendered}");

    // Click row #1 in the left history rail.
    let first_row = find_label(&rendered, "1 ✓ pwd");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(first_row), size, &mut state),
        ControllerEffect::LoadCommandOutput(first.clone())
    );
    apply_output(&mut state, &first, Ok(CapturedOutput::Text("/repo".into())));
    let rendered_first = draw(&mut state, size.width, size.height);
    assert!(rendered_first.contains("Command 1/3"), "{rendered_first}");

    // Wheel down over the left history rail switches directly to #2.
    assert_eq!(
        handle_mouse_scroll_down(first_row, size, &mut state),
        ControllerEffect::LoadCommandOutput(second.clone())
    );
    apply_output(
        &mut state,
        &second,
        Ok(CapturedOutput::Text("diff output".into())),
    );
    let rendered_second = draw(&mut state, size.width, size.height);
    assert!(rendered_second.contains("Command 2/3"), "{rendered_second}");

    // Click footer chips: [Next ▶], [◀ Prev], [Copy cmd], [Close].
    let next_chip = find_label(&rendered_second, "[Next ▶]");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(next_chip), size, &mut state),
        ControllerEffect::LoadCommandOutput(third.clone())
    );
    let rendered_third = draw(&mut state, size.width, size.height);
    let prev_chip = find_label(&rendered_third, "[◀ Prev]");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(prev_chip), size, &mut state),
        ControllerEffect::LoadCommandOutput(second.clone())
    );
    let rendered_again = draw(&mut state, size.width, size.height);
    let copy_cmd_chip = find_label(&rendered_again, "[Copy cmd]");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(copy_cmd_chip), size, &mut state),
        ControllerEffect::CopyText("git diff".into())
    );
    let close_chip = find_label(&rendered_again, "[Close]");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(close_chip), size, &mut state),
        ControllerEffect::None
    );
    assert!(state.overlay().is_none());
}

#[test]
fn left_hand_shortcuts_navigate_scroll_and_close() {
    let mut state = state();
    let first = completed_command(&mut state, "first", "echo one", 0, "");
    let second = completed_command(&mut state, "second", "echo two", 0, "");
    assert_eq!(open(&mut state), second);

    assert_eq!(
        key(&mut state, KeyCode::Char('a')),
        ControllerEffect::LoadCommandOutput(first.clone())
    );
    let output = (1..=40)
        .map(|n| format!("line-{n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    apply_output(&mut state, &first, Ok(CapturedOutput::Text(output)));
    let top = draw(&mut state, 80, 20);
    assert!(top.contains("line-01"));

    for _ in 0..15 {
        key(&mut state, KeyCode::Char('s'));
    }
    let scrolled = draw(&mut state, 80, 20);
    assert!(!scrolled.contains("line-01"));
    assert!(scrolled.contains("line-20"));

    for _ in 0..15 {
        key(&mut state, KeyCode::Char('w'));
    }
    assert!(draw(&mut state, 80, 20).contains("line-01"));

    assert_eq!(
        key(&mut state, KeyCode::Char('d')),
        ControllerEffect::LoadCommandOutput(second.clone())
    );
    assert_eq!(key(&mut state, KeyCode::Char('q')), ControllerEffect::None);
    assert!(state.overlay().is_none());
}

#[test]
fn ansi_diff_and_json_outputs_render_with_highlighting_without_escape_leaks() {
    let mut state = state();
    let ansi_cmd = completed_command(&mut state, "ansi", "pytest", 0, "");
    let diff_cmd = completed_command(&mut state, "diff", "git diff", 0, "");
    let json_cmd = completed_command(&mut state, "json", "cat config.json", 0, "");

    assert_eq!(open(&mut state), json_cmd);
    apply_output(
        &mut state,
        &json_cmd,
        Ok(CapturedOutput::Text(
            "{\n  \"enabled\": true,\n  \"retries\": 3\n}".into(),
        )),
    );
    prepare_viewport(&mut state, Size::new(100, 24));
    let json_buf = render_to_buffer(&state, 100, 24);
    let json_text = render_to_text(&state, 100, 24);
    let bool_pos = find_label(&json_text, "true");
    let bool_fg = json_buf[(bool_pos.x, bool_pos.y)].fg;
    assert!(
        matches!(bool_fg, Color::Rgb(_, _, _)),
        "expected syntect RGB color on JSON boolean, got {bool_fg:?}"
    );

    // Switch to git diff and verify syntect's Diff grammar colors additions vs deletions distinctly.
    assert_eq!(
        key(&mut state, KeyCode::Char('a')),
        ControllerEffect::LoadCommandOutput(diff_cmd.clone())
    );
    apply_output(
        &mut state,
        &diff_cmd,
        Ok(CapturedOutput::Text(
            "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-old_line\n+new_line".into(),
        )),
    );
    prepare_viewport(&mut state, Size::new(100, 24));
    let diff_buf = render_to_buffer(&state, 100, 24);
    let diff_text = render_to_text(&state, 100, 24);
    let add_pos = find_label(&diff_text, "+new_line");
    let del_pos = find_label(&diff_text, "-old_line");
    let add_fg = diff_buf[(add_pos.x, add_pos.y)].fg;
    let del_fg = diff_buf[(del_pos.x, del_pos.y)].fg;
    assert!(
        matches!(add_fg, Color::Rgb(_, _, _))
            && matches!(del_fg, Color::Rgb(_, _, _))
            && add_fg != del_fg,
        "expected distinct syntect Diff RGB colors for +/- lines, got add={add_fg:?}, del={del_fg:?}"
    );

    // Switch to ANSI output and verify SGR green applies without leaking "[32m" or OSC title.
    assert_eq!(
        key(&mut state, KeyCode::Char('a')),
        ControllerEffect::LoadCommandOutput(ansi_cmd.clone())
    );
    apply_output(
        &mut state,
        &ansi_cmd,
        Ok(CapturedOutput::Text(
            "\x1b]0;window-title\x07\x1b[32mALL_PASSED\x1b[0m plain".into(),
        )),
    );
    prepare_viewport(&mut state, Size::new(100, 24));
    let ansi_buf = render_to_buffer(&state, 100, 24);
    let ansi_text = render_to_text(&state, 100, 24);
    assert!(!ansi_text.contains("[32m"), "{ansi_text}");
    assert!(!ansi_text.contains("window-title"), "{ansi_text}");
    let pass_pos = find_label(&ansi_text, "ALL_PASSED");
    assert_eq!(ansi_buf[(pass_pos.x, pass_pos.y)].fg, Color::Green);
}

#[test]
fn split_stdout_and_stderr_panes_scroll_independently_by_hover_click_and_key() {
    let mut state = state();
    let stderr_only = completed_command(&mut state, "stderr_only", "cargo check", 1, "");
    let both = completed_command(&mut state, "both", "cargo test", 0, "");
    let size = Size::new(100, 26);

    assert_eq!(open(&mut state), both);
    let stdout_text = (1..=30)
        .map(|n| format!("stdout-line-{n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let stderr_text = (1..=30)
        .map(|n| format!("stderr-line-{n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    apply_output(
        &mut state,
        &both,
        Ok(CapturedOutput::from_tool_output(ToolOutput::json(
            json!({
                "kind": "process_action",
                "stdout": {"text": stdout_text},
                "stderr": {"text": stderr_text},
            })
            .to_string(),
        ))),
    );

    let initial = draw(&mut state, size.width, size.height);
    assert!(initial.contains("▸ stdout"), "{initial}");
    assert!(initial.contains("stdout-line-01"), "{initial}");
    assert!(initial.contains("stderr-line-01"), "{initial}");

    // Hovering stdout and scrolling down moves only stdout, leaving stderr at line 01.
    let stdout_pos = find_label(&initial, "stdout-line-01");
    let stderr_pos = find_label(&initial, "stderr-line-01");
    assert_eq!(
        handle_mouse_scroll_down(stdout_pos, size, &mut state),
        ControllerEffect::None
    );
    let after_stdout_scroll = draw(&mut state, size.width, size.height);
    assert!(!after_stdout_scroll.contains("stdout-line-01"));
    assert!(after_stdout_scroll.contains("stdout-line-04"));
    assert!(after_stdout_scroll.contains("stderr-line-01"));

    // Hovering stderr and scrolling down moves only stderr and focuses it.
    assert_eq!(
        handle_mouse_scroll_down(stderr_pos, size, &mut state),
        ControllerEffect::None
    );
    let after_stderr_scroll = draw(&mut state, size.width, size.height);
    assert!(
        after_stderr_scroll.contains("▸ stderr"),
        "{after_stderr_scroll}"
    );
    assert!(!after_stderr_scroll.contains("stderr-line-01"));
    assert!(after_stderr_scroll.contains("stderr-line-04"));
    assert!(after_stderr_scroll.contains("stdout-line-04"));

    // Pressing `e` toggles keyboard focus back to stdout, and `g` jumps stdout to top.
    key(&mut state, KeyCode::Char('e'));
    key(&mut state, KeyCode::Char('g'));
    let after_toggle = draw(&mut state, size.width, size.height);
    assert!(after_toggle.contains("▸ stdout"), "{after_toggle}");
    assert!(after_toggle.contains("stdout-line-01"), "{after_toggle}");
    assert!(!after_toggle.contains("stderr-line-01"), "{after_toggle}");

    // Clicking stderr refocuses stderr.
    assert_eq!(
        handle_mouse_input(MouseInput::Down(stderr_pos), size, &mut state),
        ControllerEffect::None
    );
    let after_click = draw(&mut state, size.width, size.height);
    assert!(after_click.contains("▸ stderr"), "{after_click}");

    // Switching to a stderr-only command collapses stdout to empty and auto-focuses stderr.
    assert_eq!(
        key(&mut state, KeyCode::Char('a')),
        ControllerEffect::LoadCommandOutput(stderr_only.clone())
    );
    apply_output(
        &mut state,
        &stderr_only,
        Ok(CapturedOutput::from_tool_output(ToolOutput::json(
            json!({
                "kind": "process_action",
                "stdout": {"text": ""},
                "stderr": {"text": stderr_text},
            })
            .to_string(),
        ))),
    );
    let stderr_only_view = draw(&mut state, size.width, size.height);
    assert!(
        stderr_only_view.contains("stdout · (empty)"),
        "{stderr_only_view}"
    );
    assert!(
        stderr_only_view.contains("stderr-line-01"),
        "{stderr_only_view}"
    );
    key(&mut state, KeyCode::Char('1'));
    key(&mut state, KeyCode::PageDown);
    let stderr_scrolled = draw(&mut state, size.width, size.height);
    assert!(!stderr_scrolled.contains("stderr-line-01"));
    assert!(stderr_scrolled.contains("stderr-line-11"));
}

#[test]
fn syntect_log_and_source_grammars_highlight_stdout_and_stderr_without_special_cases() {
    let mut state = state();
    let wrapped_src = completed_command(
        &mut state,
        "wrapped",
        "bash -lc 'cat src/lib.rs | head -n 5'",
        0,
        "",
    );
    let log_cmd = completed_command(&mut state, "log", "cargo check", 1, "");

    assert_eq!(open(&mut state), log_cmd);
    apply_output(
        &mut state,
        &log_cmd,
        Ok(CapturedOutput::from_tool_output(ToolOutput::json(
            json!({
                "kind": "process_action",
                "stdout": {"text": "2026-11-19 INFO https://example.com \"ready\" 42"},
                "stderr": {"text": "2026-11-19 ERROR failed at src/lib.rs:42\nplain diagnostic context"},
            })
            .to_string(),
        ))),
    );
    prepare_viewport(&mut state, Size::new(100, 26));
    let buf = render_to_buffer(&state, 100, 26);
    let text = render_to_text(&state, 100, 26);

    let date_pos = find_label(&text, "2026-11-19");
    let str_pos = find_label(&text, "\"ready\"");
    let num_pos = find_label(&text, "42");
    let plain_pos = find_label(&text, "plain diagnostic context");
    let date_fg = buf[(date_pos.x, date_pos.y)].fg;
    let str_fg = buf[(str_pos.x, str_pos.y)].fg;
    let num_fg = buf[(num_pos.x, num_pos.y)].fg;
    let plain_fg = buf[(plain_pos.x, plain_pos.y)].fg;
    assert!(
        matches!(date_fg, Color::Rgb(_, _, _))
            && matches!(str_fg, Color::Rgb(_, _, _))
            && matches!(num_fg, Color::Rgb(_, _, _)),
        "expected syntect log RGB colors on stdout/stderr tokens, got date={date_fg:?}, str={str_fg:?}, num={num_fg:?}"
    );
    assert_ne!(
        str_fg, plain_fg,
        "expected highlighted log tokens to stand out from plain diagnostic context"
    );
    assert_ne!(
        num_fg, plain_fg,
        "expected highlighted numbers to stand out from plain diagnostic context"
    );

    // Switch to shell-wrapped Rust file output and confirm syntect RGB highlighting applies.
    assert_eq!(
        key(&mut state, KeyCode::Char('a')),
        ControllerEffect::LoadCommandOutput(wrapped_src.clone())
    );
    apply_output(
        &mut state,
        &wrapped_src,
        Ok(CapturedOutput::from_tool_output(ToolOutput::json(
            json!({
                "kind": "process_action",
                "stdout": {"text": "pub fn greet() -> &'static str {\n    \"hello\"\n}"},
                "stderr": {"text": ""},
            })
            .to_string(),
        ))),
    );
    prepare_viewport(&mut state, Size::new(100, 24));
    let src_buf = render_to_buffer(&state, 100, 24);
    let src_text = render_to_text(&state, 100, 24);
    let fn_pos = find_label(&src_text, "fn");
    assert!(
        matches!(src_buf[(fn_pos.x, fn_pos.y)].fg, Color::Rgb(_, _, _)),
        "expected syntect RGB color on Rust keyword in shell-wrapped cat output, got {:?}",
        src_buf[(fn_pos.x, fn_pos.y)].fg
    );
}

#[test]
fn adversarial_stderr_tokens_multibyte_escapes_and_tiny_viewports_never_panic() {
    let mut state = state();
    let cmd = completed_command(&mut state, "edge", "bash -lc 'ls /missing'", 2, "");
    assert_eq!(open(&mut state), cmd);
    apply_output(
        &mut state,
        &cmd,
        Ok(CapturedOutput::from_tool_output(ToolOutput::json(
            json!({
                "kind": "process_action",
                "stdout": {
                    "text": "\x1b中\x1b[32m正常\x1b[\x1b]0;unterminated"
                },
                "stderr": {
                    "text": concat!(
                        "bash: /usr/bin/foo: command not found\n",
                        "ls: cannot access '/missing/path': No such file\n",
                        "src/dir/: trailing colon path\n",
                        "src/lib.rs:42:10:\n",
                        "at fn ()\n",
                        "  | \n",
                        "中文路径/测试.rs:12:\n"
                    )
                },
            })
            .to_string(),
        ))),
    );

    for (width, height) in [(1, 1), (8, 4), (24, 10), (80, 24), (160, 40)] {
        let rendered = draw(&mut state, width, height);
        if width >= 80 {
            assert!(rendered.contains("command not found"), "{rendered}");
            assert!(rendered.contains("trailing colon path"), "{rendered}");
        }
    }
}

#[test]
fn timeline_fold_arrows_do_not_show_beside_the_history_numbers() {
    let mut state = state();
    completed_command(&mut state, "one", "echo one", 0, "");
    completed_command(&mut state, "two", "echo two", 0, "");
    completed_command(&mut state, "three", "echo three", 0, "");
    open(&mut state);
    let width = 160;
    let height = 28;
    let buffer = render_to_buffer(&state, width, height);
    let text = render_to_text(&state, width, height);
    assert!(text.contains("HISTORY"), "{text}");
    let dialog_width = 140u16;
    let left = (width - dialog_width) / 2;
    for x in 0..left {
        for y in 0..height {
            let symbol = buffer[(x, y)].symbol();
            assert!(
                symbol != "▸" && symbol != "▾",
                "timeline fold marker leaked outside the dialog at ({x}, {y})"
            );
        }
    }
}

#[test]
fn history_position_follows_commands_that_finish_while_the_viewer_is_open() {
    let mut state = state();
    let mut projector = TuiProjector::default();
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "early",
                "run_process",
                json!({"command": "early-cmd", "cwd": "src"}),
            ),
            source: source(),
        },
        &mut state,
    );
    let middle = completed_command(&mut state, "middle", "middle-cmd", 0, "");
    let latest = completed_command(&mut state, "latest", "latest-cmd", 0, "");
    assert_eq!(open(&mut state), latest);
    apply_output(
        &mut state,
        &latest,
        Ok(CapturedOutput::Text("latest output".into())),
    );
    let before = draw(&mut state, 120, 26);
    assert!(before.contains("Command 2/2"), "{before}");
    assert!(before.contains("HISTORY (2/2)"), "{before}");

    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(ToolCallId::new("early").unwrap()),
            output: Some(ToolOutput::Json {
                json: json!({
                    "kind": "process_action",
                    "status": 0,
                    "stdout": {"text": "early output"},
                    "stderr": {"text": ""},
                })
                .to_string(),
            }),
            source: source(),
        },
        &mut state,
    );
    let after_early = draw(&mut state, 120, 26);
    assert!(after_early.contains("Command 3/3"), "{after_early}");
    assert!(after_early.contains("HISTORY (3/3)"), "{after_early}");
    assert!(after_early.contains("latest-cmd"), "{after_early}");
    assert!(
        after_early
            .lines()
            .any(|line| line.contains("▸ 3") && line.contains("latest-cmd")),
        "rail marker moved off the open command:\n{after_early}"
    );
    let marker_pos = find_label(&after_early, "▸ 3");
    assert_eq!(
        handle_mouse_input(MouseInput::Down(marker_pos), Size::new(120, 26), &mut state),
        ControllerEffect::None
    );

    apply_output(
        &mut state,
        &middle,
        Ok(CapturedOutput::Text("middle output".into())),
    );
    key(&mut state, KeyCode::Char('a'));
    let on_middle = draw(&mut state, 120, 26);
    assert!(on_middle.contains("Command 2/3"), "{on_middle}");
    let trailing = completed_command(&mut state, "trailing", "trailing-cmd", 0, "");
    let after_trailing = draw(&mut state, 120, 26);
    assert!(after_trailing.contains("Command 2/4"), "{after_trailing}");
    assert!(after_trailing.contains("middle-cmd"), "{after_trailing}");
    assert_eq!(
        key(&mut state, KeyCode::Char('d')),
        ControllerEffect::LoadCommandOutput(latest.clone())
    );
    let _ = trailing;
}

#[test]
fn wheel_outside_a_pane_keeps_the_focused_stream() {
    let mut state = state();
    let command = completed_command(&mut state, "split-wheel", "cargo test", 0, "");
    assert_eq!(open(&mut state), command);
    let stdout_text = (1..=20)
        .map(|n| format!("stdout-line-{n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let stderr_text = (1..=20)
        .map(|n| format!("stderr-line-{n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    apply_output(
        &mut state,
        &command,
        Ok(CapturedOutput::from_tool_output(ToolOutput::json(
            json!({
                "kind": "process_action",
                "stdout": {"text": stdout_text},
                "stderr": {"text": stderr_text},
            })
            .to_string(),
        ))),
    );
    let size = Size::new(100, 26);
    key(&mut state, KeyCode::Char('2'));
    let focused = draw(&mut state, size.width, size.height);
    assert!(focused.contains("▸ stderr"), "{focused}");
    let footer = find_label(&focused, "[Copy cmd]");
    assert_eq!(
        handle_mouse_scroll_down(footer, size, &mut state),
        ControllerEffect::None
    );
    let after_footer = draw(&mut state, size.width, size.height);
    assert!(after_footer.contains("▸ stderr"), "{after_footer}");
    assert!(after_footer.contains("stdout-line-01"), "{after_footer}");
    assert!(!after_footer.contains("stderr-line-01"), "{after_footer}");

    assert_eq!(
        handle_mouse_scroll_down(Position::new(0, 0), size, &mut state),
        ControllerEffect::None
    );
    let outside = draw(&mut state, size.width, size.height);
    assert!(outside.contains("stdout-line-01"), "{outside}");
}

#[test]
fn single_buffer_ignores_the_stderr_focus_key() {
    let mut state = state();
    let command = completed_command(&mut state, "single-key", "echo lines", 0, "");
    assert_eq!(open(&mut state), command);
    let output = (1..=30)
        .map(|n| format!("line-{n:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    apply_output(&mut state, &command, Ok(CapturedOutput::Text(output)));
    draw(&mut state, 80, 20);
    key(&mut state, KeyCode::Char('2'));
    key(&mut state, KeyCode::Char('e'));
    for _ in 0..8 {
        key(&mut state, KeyCode::Char('s'));
    }
    let scrolled = draw(&mut state, 80, 20);
    assert!(!scrolled.contains("line-01"), "{scrolled}");
    assert!(scrolled.contains("line-12"), "{scrolled}");
}
