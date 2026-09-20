use crate::tui::{
    keymap::Keymap,
    projector::TuiProjector,
    render::render_to_text,
    state::{TimelineItem, TuiState},
    tests::{pending_call_with_args, source},
    theme::TuiTheme,
};
use merry_core::{
    ArtifactId, ArtifactKind, ArtifactRef, ErrorInfo, RuntimeEvent, ToolCallId, ToolCallResult,
    ToolOutput,
};
use merry_runtime::SessionTranscriptItem;
use serde_json::json;

#[test]
fn successful_read_output_follows_display_policy_live_and_on_resume() {
    for show_output in [false, true] {
        for replay in [false, true] {
            for output in [
                ToolOutput::Json {
                    json: json!({
                        "ok": true,
                        "tool": "read_text",
                        "path": "notes.txt",
                        "start_line": 2,
                        "end_line": 4,
                        "content": "read-output-marker\nsecond line\nthird line\n",
                        "truncated": true,
                    })
                    .to_string(),
                },
                ToolOutput::Text {
                    text: "read-output-marker".to_owned(),
                },
            ] {
                let mut state = read_state(show_output);
                let kind = match &output {
                    ToolOutput::Json { .. } => ArtifactKind::Json,
                    ToolOutput::Text { .. } => ArtifactKind::Text,
                };
                let result = successful_read_result(kind);

                project_read_result(
                    &mut state,
                    replay,
                    result,
                    output,
                    json!({"path": "notes.txt", "start_line": 2, "max_lines": 3}),
                );

                let rendered = render_to_text(&state, 120, 24);
                assert!(rendered.contains("Read read_text"));
                assert!(rendered.contains("path=notes.txt"));
                assert!(rendered.contains("start_line=2"));
                assert!(rendered.contains("max_lines=3"));
                assert_eq!(
                    rendered.contains("read-output-marker"),
                    show_output,
                    "read output policy must hold for live and replayed results: replay={replay}"
                );
            }
        }
    }
}

#[test]
fn failed_read_output_stays_visible_live_and_on_resume() {
    for show_output in [false, true] {
        for replay in [false, true] {
            let mut state = read_state(show_output);
            let result = ToolCallResult::failed(
                ToolCallId::new("read-call").unwrap(),
                ArtifactRef::new(ArtifactId::new("read-result").unwrap(), ArtifactKind::Json),
                ErrorInfo::new("file_not_found", "workspace file was not found").unwrap(),
            );
            let output = ToolOutput::Json {
                json: json!({
                    "ok": false,
                    "tool": "read_text",
                    "path": "notes.txt",
                    "error": {
                        "code": "file_not_found",
                        "message": "workspace file was not found",
                    },
                })
                .to_string(),
            };

            project_read_result(
                &mut state,
                replay,
                result,
                output,
                json!({"path": "notes.txt", "start_line": 2, "max_lines": 3}),
            );

            let rendered = render_to_text(&state, 120, 24);
            assert!(rendered.contains("path=notes.txt"));
            assert!(rendered.contains("failed"));
            assert!(rendered.contains("workspace file was not found"));
        }
    }
}

#[test]
fn read_preview_preserves_truncation_and_follows_render_time_settings() {
    for replay in [false, true] {
        for (content, source_truncated, preview_truncated) in [
            ("first\nsecond\nthird\nfourth\nfifth\n", false, false),
            ("first\nsecond\nthird\nfourth\nfifth\n", true, true),
            ("first\nsecond\nthird\nfourth\nfifth\nsixth\n", false, true),
            (
                "\nfirst\n\nsecond\nthird\nfourth\nfifth\nsixth\n",
                false,
                true,
            ),
            ("first\n", true, true),
        ] {
            let mut state = read_state(false);
            let result = successful_read_result(ArtifactKind::Json);
            let output = ToolOutput::Json {
                json: json!({
                    "ok": true,
                    "tool": "read_text",
                    "path": "notes.txt",
                    "content": content,
                    "truncated": source_truncated,
                })
                .to_string(),
            };
            project_read_result(
                &mut state,
                replay,
                result,
                output,
                json!({"path": "notes.txt", "start_line": 2, "max_lines": 3}),
            );

            assert!(!render_to_text(&state, 120, 24).contains("first"));
            state = state.with_successful_command_output(true);
            let rendered = render_to_text(&state, 120, 24);
            assert!(rendered.contains("first"));
            assert!(!rendered.contains("sixth"));
            assert_eq!(rendered.contains("..."), preview_truncated);
            state = state.with_successful_command_output(false);
            assert!(!render_to_text(&state, 120, 24).contains("first"));
        }
    }
}

#[test]
fn successful_read_shows_decoded_content_and_arguments_without_completion_noise() {
    for arguments in [json!({}), json!({"path": "AGENTS.md"})] {
        let has_path = arguments.get("path").is_some();
        let mut state = read_state(true);
        let result = successful_read_result(ArtifactKind::Json);
        let output = ToolOutput::Json {
            json: json!({
                "ok": true,
                "tool": "read_text",
                "path": "AGENTS.md",
                "bytes": 19704,
                "content": "large raw content",
            })
            .to_string(),
        };

        project_read_result(&mut state, false, result, output, arguments);

        assert_eq!(state.timeline().len(), 1);
        let rendered = render_to_text(&state, 120, 24);
        assert!(rendered.contains("Read read_text"));
        assert_eq!(rendered.contains("path=AGENTS.md"), has_path);
        assert!(rendered.contains("large raw content"));
        assert!(!rendered.contains("AGENTS.md:1"));
        assert!(!rendered.contains("completed"));
        assert!(!rendered.contains(r#""content":"#));
    }
}

#[test]
fn read_projection_retains_only_bounded_previews_for_json_text_and_empty_files() {
    let content = "界🚀".repeat(150_000);
    for replay in [false, true] {
        for output in [
            ToolOutput::Text {
                text: content.clone(),
            },
            ToolOutput::Json {
                json: json!({
                    "ok": true, "tool": "read_text", "path": "notes.txt", "content": content,
                })
                .to_string(),
            },
            ToolOutput::Json {
                json: json!({
                    "ok": true, "tool": "read_text", "path": content, "content": "",
                })
                .to_string(),
            },
        ] {
            let kind = match &output {
                ToolOutput::Json { .. } => ArtifactKind::Json,
                ToolOutput::Text { .. } => ArtifactKind::Text,
            };
            let mut state = read_state(false);
            project_read_result(
                &mut state,
                replay,
                successful_read_result(kind),
                output,
                json!({"path": "notes.txt"}),
            );

            let TimelineItem::Read { preview, .. } = &state.timeline()[0] else {
                panic!("read result must retain a bounded preview");
            };
            assert!(preview.lines.iter().map(String::len).sum::<usize>() <= 720);
            assert!(preview.truncated);
            assert!(!render_to_text(&state, 400, 24).contains('界'));
            state = state.with_successful_command_output(true);
            let rendered = render_to_text(&state, 400, 24);
            assert!(rendered.contains('界'));
            assert!(rendered.contains("..."));
        }
    }
}

fn read_state(show_output: bool) -> TuiState {
    TuiState::new(
        "/repo".into(),
        "test-model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
    .with_successful_command_output(show_output)
}

fn successful_read_result(kind: ArtifactKind) -> ToolCallResult {
    ToolCallResult::succeeded(
        ToolCallId::new("read-call").unwrap(),
        ArtifactRef::new(ArtifactId::new("read-result").unwrap(), kind),
    )
}

fn project_read_result(
    state: &mut TuiState,
    replay: bool,
    result: ToolCallResult,
    output: ToolOutput,
    arguments: serde_json::Value,
) {
    let mut projector = TuiProjector::default();
    let call = pending_call_with_args(result.call_id().as_str(), "read_text", arguments);
    if replay {
        projector.apply_transcript_item(SessionTranscriptItem::ToolCall { call }, state);
        projector.apply_transcript_item(
            SessionTranscriptItem::ToolResult {
                call_id: result.call_id().clone(),
                result,
                output: Some(output),
            },
            state,
        );
    } else {
        projector.apply(
            RuntimeEvent::ToolCallStarted {
                call,
                source: source(),
            },
            state,
        );
        projector.apply(
            RuntimeEvent::ToolCallFinished {
                result,
                output: Some(output),
                source: source(),
            },
            state,
        );
    }
}
