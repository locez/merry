use super::controller::{ControllerEffect, handle_key_action, handle_key_event};
use super::input::TextInput;
use super::keymap::{KeyAction, KeyBinding, Keymap};
use super::projector::TuiProjector;
use super::render::{render_to_buffer, render_to_buffer_and_cursor, render_to_text};
use super::state::{PatchChangeView, PatchLineView, QueuePreview, TimelineItem, TuiState};
use super::theme::{SemanticColor, TuiTheme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ContextWindowSource, ErrorInfo, ModelUsage,
    PendingToolCall, QueuedInputLane, QueuedInputView, RuntimeEvent, RuntimeEventSource, SessionId,
    SessionUsage, ToolCallArguments, ToolCallId, ToolCallResult, ToolName, ToolOutput,
    UsageContextWindow,
};
use merry_tool_workspace::WORKSPACE_PATCH_TOOL;
use ratatui::style::{Color, Modifier};
use serde_json::json;

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

    assert_eq!(effect, ControllerEffect::SubmitNext("now".to_owned()));
    assert_eq!(state.input_text(), "");
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
        ControllerEffect::SubmitNext("first".to_owned())
    );
    state.input_mut().insert_str("second");
    assert_eq!(
        handle_key_action(KeyAction::SubmitNext, &mut state),
        ControllerEffect::SubmitNext("second".to_owned())
    );

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
        ControllerEffect::SubmitNext("sent".to_owned())
    );
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
        ControllerEffect::SubmitNext("draft".to_owned())
    );
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
        Some(KeyAction::Quit)
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
    assert!(matches!(state.timeline()[0], TimelineItem::Muted { .. }));
    assert!(matches!(state.timeline()[1], TimelineItem::Expanded { .. }));
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
    assert!(matches!(state.timeline()[0], TimelineItem::Muted { .. }));
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
    let TimelineItem::Muted { title, detail } = &state.timeline()[0] else {
        panic!("read tool call should render as a compact muted line");
    };
    assert_eq!(title, "Read");
    assert_eq!(detail, "AGENTS.md");
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
    assert_eq!(detail, ".");
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
                json!({ "argv": ["python3", "hello_world.py"], "cwd": "." }),
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
    let TimelineItem::Expanded { title, body } = &state.timeline()[0] else {
        panic!("process call should expand with output preview");
    };
    assert_eq!(title, "Ran python3 hello_world.py (cwd: .)");
    assert!(!body.contains("python3 hello_world.py (cwd: .)"));
    assert!(body.contains("  stdout: hello world"));
}

#[test]
fn projector_renders_failed_process_calls_as_ran_with_error_preview() {
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
                json!({ "argv": ["cargo", "test", "-p", "merry-cli"], "cwd": "." }),
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
    let TimelineItem::Expanded { title, body } = &state.timeline()[0] else {
        panic!("failed process call should expand with output preview");
    };
    assert_eq!(title, "Ran cargo test -p merry-cli (cwd: .)");
    assert!(!body.contains("cargo test -p merry-cli (cwd: .)"));
    assert!(body.contains("  exit 101"));
    assert!(body.contains("  stderr: error: test failed"));
    assert!(body.contains("    rerun with --exact"));
}

#[test]
fn projector_truncates_process_preview_lines() {
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
                json!({ "argv": ["cargo", "test"], "cwd": "." }),
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

    let TimelineItem::Expanded { body, .. } = &state.timeline()[0] else {
        panic!("process call should expand with output preview");
    };
    assert!(body.contains("  stdout: "));
    assert!(body.contains("..."));
    assert!(!body.contains(&"x".repeat(150)));
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
                    "for_action": { "argv": ["cargo", "test"] }
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
                json: r#"{"error":{"code":"permission_review_failed","message":"permission review failed: provider stream Protocol: stream line must start with data:"},"guidance":{"kind":"permission_review_failed","message":"Do not assume the requested capability was granted."},"status":"review_failed","tool_call_id":"call-permission"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 2);
    let TimelineItem::Diagnostic { title, body } = &state.timeline()[1] else {
        panic!("failed tool should render as a compact diagnostic");
    };
    assert_eq!(title, "tool failed: request_permissions");
    assert!(body.contains("permission_review_failed"));
    assert!(body.contains("Do not assume the requested capability was granted."));
    assert!(!body.contains("\"tool_call_id\""));
    assert!(!body.contains("call-permission"));
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

    let text = render_to_text(&state, 96, 16);

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
    assert!(matches!(state.timeline()[0], TimelineItem::Muted { .. }));
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
                total: ModelUsage::new(10, 3),
                last: ModelUsage::new(10, 3),
                context: Some(UsageContextWindow {
                    resolved_model_window_tokens: 128000,
                    effective_window_tokens: 128000,
                    source: ContextWindowSource::ProviderCapabilities,
                }),
                compaction: None,
            },
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.queue_preview().next[0].text, "urgent");
    assert!(state.timeline().is_empty());
    assert!(
        state
            .status_text()
            .contains("last in 10 out 3 | total 13 tok")
    );
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

    let text = render_to_text(&state, 80, 24);

    assert!(text.contains("gpt-test"));
    assert!(text.contains("assistant says hello"));
    assert!(text.contains("Next"));
    assert!(text.contains("next item"));
    assert!(text.contains("Backlog"));
    assert!(text.contains("backlog item"));
    assert!(text.contains("hi"));
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

    let text = render_to_text(&state, 80, 16);

    assert!(text.contains("user"));
    assert!(text.contains("baidu.com"));
}

#[test]
fn renderer_shows_empty_queue_lanes() {
    let state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );

    let text = render_to_text(&state, 80, 16);

    assert!(text.contains("Next"));
    assert!(text.contains("Suspended"));
    assert!(text.contains("Backlog"));
    assert!(text.contains("--"));
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
    assert!(second.contains("user: second request"));
    assert!(!second.contains("first request"));

    state.jump_to_previous_user_input();
    let first = render_to_text(&state, 80, 18);
    assert!(first.contains("user: first request"));
    assert!(first.contains("first answer"));

    state.exit_timeline_review();
    let bottom = render_to_text(&state, 80, 18);
    assert!(bottom.contains("second answer"));
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
        title: "Ran cargo test (cwd: .)".to_owned(),
        body: "  stdout: ok".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Expanded {
        title: "patch".to_owned(),
        body: "+added\n-removed".to_owned(),
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

    let buffer = render_to_buffer(&state, 80, 24);

    assert_eq!(find_cell_color(&buffer, "gpt-test"), Some(Color::Red));
    assert_eq!(find_cell_color(&buffer, "tool"), Some(Color::Blue));
    assert_eq!(find_cell_color(&buffer, "Ran"), Some(Color::Cyan));
    assert_eq!(find_cell_color(&buffer, "cargo"), Some(Color::LightBlue));
    assert_eq!(find_cell_color(&buffer, "ok"), Some(Color::Blue));
    assert_eq!(find_cell_color(&buffer, "patch"), Some(Color::Magenta));
    assert_eq!(find_cell_color(&buffer, "+added"), Some(Color::Green));
    assert_eq!(find_cell_color(&buffer, "-removed"), Some(Color::Yellow));
    assert_eq!(find_cell_color(&buffer, "Next"), Some(Color::Magenta));
    assert_eq!(find_cell_color(&buffer, "queued"), Some(Color::Blue));
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

    let text = render_to_text(&state, 80, 18);
    eprintln!("{text}");
    assert!(text.contains("Output:"));
    assert!(text.contains("  hello world"));
    assert!(text.contains("Done"));
    assert!(!text.contains("```"));
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
fn renderer_colors_ran_title_and_indents_process_preview() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Expanded {
        title: "Ran python3 hello_world.py (cwd: .)".to_owned(),
        body: "  stdout: hello world".to_owned(),
    });

    let text = render_to_text(&state, 96, 16);
    assert!(text.contains("Ran python3 hello_world.py (cwd: .)"));
    assert!(text.contains("  stdout: hello world"));

    let buffer = render_to_buffer(&state, 96, 16);
    assert_eq!(find_cell_color(&buffer, "Ran"), Some(Color::LightCyan));
    assert_eq!(find_cell_color(&buffer, "python3"), Some(Color::LightBlue));
    assert_eq!(
        find_cell_color(&buffer, "hello world"),
        Some(Color::DarkGray)
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
fn renderer_keeps_input_region_stable_when_queue_count_changes() {
    let empty_state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut queued_state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    queued_state.update_queue_preview(QueuePreview {
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

    let empty_text = render_to_text(&empty_state, 80, 18);
    let queued_text = render_to_text(&queued_state, 80, 18);
    let empty_input_row = empty_text
        .lines()
        .position(|line| line.contains("input"))
        .expect("empty queue render should show input");
    let queued_input_row = queued_text
        .lines()
        .position(|line| line.contains("input"))
        .expect("populated queue render should show input");

    assert_eq!(empty_input_row, queued_input_row);
}

fn find_cell_color(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<Color> {
    find_cell_style(buffer, text).and_then(|style| style.fg)
}

fn find_cell_style(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<ratatui::style::Style> {
    let area = buffer.area;
    let first = text.chars().next()?.to_string();
    for y in area.y..area.y + area.height {
        let mut row = String::new();
        for x in area.x..area.x + area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        let Some(start) = row.find(text) else {
            continue;
        };
        for x in area.x + start as u16..area.x + area.width {
            if buffer[(x, y)].symbol() == first {
                return Some(buffer[(x, y)].style());
            }
        }
    }
    None
}
