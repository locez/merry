use crate::tui::{
    command_details::{CapturedOutput, apply_output, load_output},
    controller::{ControllerEffect, handle_key_event},
    keymap::Keymap,
    projector::TuiProjector,
    render::{prepare_viewport, render_to_text},
    state::TuiState,
    terminal::write_clipboard_request,
    tests::{pending_call_with_args, source, text_artifact},
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    ArtifactId, ArtifactRef, ErrorInfo, RuntimeEvent, SessionId, ToolCallId, ToolCallResult,
    ToolOutput,
};
use merry_runtime::{ArtifactContent, Runtime};
use ratatui::layout::Size;
use serde_json::json;

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
) -> ArtifactRef {
    let artifact = text_artifact(id);
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
        ToolCallResult::succeeded(call_id, artifact.clone())
    } else {
        ToolCallResult::failed(
            call_id,
            artifact.clone(),
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
    artifact
}

fn key(state: &mut TuiState, code: KeyCode) -> ControllerEffect {
    handle_key_event(KeyEvent::new(code, KeyModifiers::NONE), state)
}

fn open(state: &mut TuiState) -> ArtifactId {
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        state,
    );
    let ControllerEffect::LoadCommandOutput(artifact_id) = effect else {
        panic!("opening a completed command should request its artifact");
    };
    artifact_id
}

fn draw(state: &mut TuiState, width: u16, height: u16) -> String {
    prepare_viewport(state, Size::new(width, height));
    render_to_text(state, width, height)
}

#[test]
fn command_details_shortcut_toggles_without_changing_the_draft_or_reading_position() {
    let mut state = state();
    let artifact = completed_command(&mut state, "toggle", "pwd", 0, "");
    state.append_assistant_delta(None, &"context\n".repeat(40));
    state.insert_input_str("keep my draft");
    state.scroll_timeline_up_by(10);
    let before = draw(&mut state, 80, 24);

    for _ in 0..3 {
        assert_eq!(open(&mut state), *artifact.id());
        assert!(draw(&mut state, 80, 24).contains("Loading captured output"));
        assert_eq!(
            handle_key_event(
                KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
                &mut state,
            ),
            ControllerEffect::None
        );
        assert!(state.overlay().is_none());
        apply_output(
            &mut state,
            artifact.id(),
            Ok(CapturedOutput::Text("late output".into())),
        );
        assert_eq!(draw(&mut state, 80, 24), before);
    }
}

#[test]
fn command_output_hint_appears_only_when_completed_output_is_available() {
    let mut state = state();
    assert!(!draw(&mut state, 80, 24).contains("Ctrl+T output"));
    TuiProjector::default().apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args("pending", "run_process", json!({"command": "pwd"})),
            source: source(),
        },
        &mut state,
    );
    assert!(!draw(&mut state, 80, 24).contains("Ctrl+T output"));
    completed_command(&mut state, "ready", "pwd", 0, "");
    for width in [24, 80] {
        assert!(draw(&mut state, width, 24).contains("Ctrl+T output"));
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
    assert!(wide.contains("Ctrl+End latest"));
    assert!(wide.contains("Ctrl+T output"));
    let narrow = draw(&mut state, 40, 24);
    assert!(narrow.contains("New content"));
    assert!(narrow.contains("Ctrl+End latest"));
    assert!(!narrow.contains("Ctrl+T output"));
}

#[test]
fn command_details_close_hint_and_feedback_fit_narrow_terminals() {
    let mut state = state();
    completed_command(&mut state, "footer", "pwd", 0, "");
    open(&mut state);
    key(&mut state, KeyCode::Char('y'));
    for width in [24, 40, 80, 120] {
        let rendered = draw(&mut state, width, 24);
        assert!(rendered.contains("Ctrl+T / Esc close"), "{rendered}");
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
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
            &mut state,
        ),
        ControllerEffect::None
    );
    assert_eq!(draw(&mut state, 80, 24), before);
}

#[test]
fn hidden_success_output_is_inspectable_and_copy_preserves_the_actual_command() {
    let mut state = state();
    let command = "printf '%s' 'line one'\nprintf 'line two'";
    let artifact = completed_command(&mut state, "first", command, 0, "secret preview");
    state.insert_input_str("unfinished draft");
    let compact = draw(&mut state, 100, 26);
    assert!(!compact.contains("secret preview"));
    assert_eq!(open(&mut state), *artifact.id());
    assert!(draw(&mut state, 100, 26).contains("Loading captured output"));
    apply_output(
        &mut state,
        artifact.id(),
        Ok(CapturedOutput::from_artifact(ArtifactContent::json(
            json!({
                "kind": "process_action", "stdout": {"text": "first line\n\n  indented\nlast line"},
                "stderr": {"text": "an error"},
            })
            .to_string(),
        ))
        .unwrap()),
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
    assert_eq!(open(&mut state), *second.id());
    assert_eq!(
        key(&mut state, KeyCode::Left),
        ControllerEffect::LoadCommandOutput(first.id().clone())
    );
    apply_output(
        &mut state,
        second.id(),
        Ok(CapturedOutput::Text("STALE".into())),
    );
    let pending = draw(&mut state, 80, 24);
    assert!(pending.contains("Command 1/2"));
    assert!(!pending.contains("STALE"));
    assert_eq!(key(&mut state, KeyCode::Left), ControllerEffect::None);
    assert_eq!(
        key(&mut state, KeyCode::Right),
        ControllerEffect::LoadCommandOutput(second.id().clone())
    );
    apply_output(
        &mut state,
        second.id(),
        Ok(CapturedOutput::Text("COMPLETE ERROR".into())),
    );
    let failed = draw(&mut state, 80, 24);
    assert!(failed.contains("Exit: 128"));
    assert!(failed.contains("COMPLETE ERROR"));
    key(&mut state, KeyCode::Esc);
    apply_output(
        &mut state,
        second.id(),
        Ok(CapturedOutput::Text("late completion".into())),
    );
    assert!(state.overlay().is_none());
}

#[test]
fn full_output_scrolls_beyond_preview_and_u16_limits_without_truncating_text() {
    let mut state = state();
    let artifact = completed_command(&mut state, "large", "cargo test", 0, "");
    open(&mut state);
    let output = format!("{}TAIL-完整-output", "line\n".repeat(66_000));
    apply_output(&mut state, artifact.id(), Ok(CapturedOutput::Text(output)));
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
    let artifact = completed_command(&mut state, "limited", "build", 1, "");
    open(&mut state);
    apply_output(&mut state, artifact.id(), Ok(CapturedOutput::from_artifact(ArtifactContent::json(json!({
        "kind": "process_action", "stdout": {"text": "prefix", "truncated": true, "utf8": false}, "stderr": {},
    }).to_string())).unwrap()));
    let rendered = draw(&mut state, 110, 28);
    assert!(rendered.contains("capture truncated"));
    assert!(rendered.contains("Non-UTF-8"));
    apply_output(
        &mut state,
        artifact.id(),
        Err("artifact unavailable".into()),
    );
    assert!(draw(&mut state, 80, 24).contains("Could not load output: artifact unavailable"));
    assert_eq!(key(&mut state, KeyCode::Char('y')), ControllerEffect::None);
    assert!(CapturedOutput::from_artifact(ArtifactContent::binary(vec![0, 1])).is_err());
}

#[tokio::test]
async fn output_loader_reads_runtime_artifacts_and_reports_missing_records() {
    let runtime = Runtime::builder(SessionId::new("command-inspection").unwrap())
        .build()
        .unwrap();
    let artifact = text_artifact("capture");
    runtime
        .record_artifact(
            artifact.clone(),
            ArtifactContent::text("exact captured output\n"),
        )
        .await
        .unwrap();
    assert_eq!(
        load_output(&runtime, artifact.id())
            .await
            .unwrap()
            .copy_text(),
        "exact captured output\n"
    );
    assert!(
        load_output(&runtime, &ArtifactId::new("missing").unwrap())
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
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        &mut state,
    );
    assert_eq!(effect, ControllerEffect::None);
    assert!(draw(&mut state, 100, 24).contains("No completed commands yet"));
    key(&mut state, KeyCode::Esc);
    assert!(draw(&mut state, 100, 24).contains("keep my draft"));
}

#[test]
fn inspection_toggle_and_follow_shortcuts_respect_configured_bindings() {
    let config: crate::config::TuiKeymapToml =
        toml::from_str("open_command_details = 'ctrl+f'\nfollow_latest = 'ctrl+b'").unwrap();
    let keymap = Keymap::from_config(&config).unwrap();
    let mut state = TuiState::new("/repo".into(), "test".into(), keymap, TuiTheme::default());
    let artifact = completed_command(&mut state, "configured", "pwd", 0, "");
    let collapsed = draw(&mut state, 80, 24);
    assert!(collapsed.contains("Ctrl+F output"));
    assert!(!collapsed.contains("Ctrl+T output"));
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            &mut state
        ),
        ControllerEffect::LoadCommandOutput(artifact.id().clone())
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
    let artifact = completed_command(&mut state, "reopen", "pwd", 0, "");
    open(&mut state);
    let previous_generation = state.command_details_generation();
    key(&mut state, KeyCode::Esc);
    open(&mut state);
    key(&mut state, KeyCode::Char('y'));
    let generation = state.command_details_generation();
    CommandOutputCompletion::new(
        artifact.id().clone(),
        generation,
        Ok(CapturedOutput::Text("latest capture".into())),
    )
    .apply(&mut state);
    CommandOutputCompletion::new(
        artifact.id().clone(),
        previous_generation,
        Err("obsolete timeout".into()),
    )
    .apply(&mut state);
    let rendered = draw(&mut state, 100, 24);
    assert!(rendered.contains("latest capture"));
    assert!(!rendered.contains("obsolete timeout"));
    assert!(!rendered.contains("Output is not available yet"));
}
