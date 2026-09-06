use crate::tui::{
    keymap::Keymap,
    projector::TuiProjector,
    state::{TimelineItem, TuiState},
    tests::{pending_call_with_args, source, text_artifact},
    theme::TuiTheme,
};
use merry_core::{ErrorInfo, RuntimeEvent, ToolCallId, ToolCallResult, ToolOutput};
use serde_json::json;

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
    let TimelineItem::Expanded { title, body } = &state.timeline()[0] else {
        panic!("process call should expand with output preview");
    };
    assert_eq!(title, "Ran python3 hello_world.py (.)");
    assert_eq!(body, "  hello world");
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
    let TimelineItem::Expanded { title, body } = &state.timeline()[0] else {
        panic!("nonzero process exit should remain a command result");
    };
    assert_eq!(title, "Ran cargo test -p merry-cli (.) -> exit 101");
    assert_eq!(body, "  error: test failed\n  rerun with --exact");
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
                    "for_action": { "command": "cargo test", "cwd": null }
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

    let TimelineItem::Expanded { body, .. } = &state.timeline()[0] else {
        panic!("successful permission call should show an expanded admission result");
    };
    assert!(body.contains("allowed: The exact command is grounded in the user's task."));
    assert!(body.contains("profile: process.permission_request.approved"));
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

    let TimelineItem::Expanded { body, .. } = &state.timeline()[0] else {
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

    let TimelineItem::Expanded { body, .. } = &state.timeline()[0] else {
        panic!("process call should expand with output preview");
    };
    assert_eq!(body, "  one\n  two\n  three\n  four\n  five");
}
