use crate::tui::{
    keymap::Keymap,
    projector::TuiProjector,
    render::render_to_text,
    state::{TimelineItem, TuiState},
    tests::{pending_call_with_args, source, text_artifact},
    theme::TuiTheme,
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, QueuedInputLane, QueuedInputView,
    RuntimeEvent, ToolCallId, ToolCallResult, ToolOutput,
};
use merry_runtime::SessionTranscriptItem;
use serde_json::json;

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
    let call = pending_call_with_args("call-read", "read_text", json!({"path": "hello_world.py"}));
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
                    "tool": "read_text",
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
        TimelineItem::Expanded { title, body }
            if title == "Read read_text path=hello_world.py"
                && body.contains("print('hi')")
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
            diagnostic: ErrorInfo::new(
                "auto_compaction",
                "OpenAI Responses request to host api.example.test:443 returned HTTP 400 (type: invalid_request_error) (code: invalid_json_schema) (param: text.format.schema) (server error: Invalid schema for response_format 'compacted_checkpoint_candidate': Missing 'rationale'.)",
            )
            .unwrap(),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline(),
        [TimelineItem::Diagnostic {
            title: "compaction failed".to_owned(),
            body: "OpenAI Responses request to host api.example.test:443 returned HTTP 400 (type: invalid_request_error) (code: invalid_json_schema) (param: text.format.schema) (server error: Invalid schema for response_format 'compacted_checkpoint_candidate': Missing 'rationale'.)".to_owned(),
        }]
    );
    let rendered = render_to_text(&state, 120, 10);
    assert!(rendered.contains("HTTP 400"));
    assert!(rendered.contains("api.example.test:443"));
    assert!(rendered.contains("invalid_json_schema"));
    assert!(rendered.contains("Missing 'rationale'."));
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
