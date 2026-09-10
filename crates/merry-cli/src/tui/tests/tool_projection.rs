use crate::tui::{
    keymap::Keymap,
    projector::TuiProjector,
    render::{render_to_buffer, render_to_text},
    state::{TimelineItem, TuiState},
    tests::{
        find_cell_color, pending_batch, pending_call, pending_call_with_args, source, text_artifact,
    },
    theme::TuiTheme,
};
use merry_core::{ErrorInfo, RuntimeEvent, ToolCallId, ToolCallResult, ToolOutput};
use merry_tools::APPLY_PATCH_TOOL;
use ratatui::style::Color;
use serde_json::json;

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
            call: pending_call("call-read", "read_text"),
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
            call: pending_call("call-patch", APPLY_PATCH_TOOL),
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
                    pending_call_with_args("call-first", "read_text", json!({"path": "first.rs"})),
                    pending_call_with_args(
                        "call-second",
                        "run_process",
                        json!({"command": "rg --files", "cwd": "src"}),
                    ),
                ],
            ),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 2);
    let rendered = render_to_text(&state, 120, 24);
    let read_position = rendered
        .find("Read read_text path=first.rs")
        .expect("read call is visible");
    let process_position = rendered.find("Running ").expect("process call is running");
    assert!(read_position < process_position);
    assert!(rendered.contains("rg --files (src)"));
    assert!(!rendered.contains("Ran "));
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
            call: pending_call_with_args("call-read", "read_text", json!({ "path": "AGENTS.md" })),
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
                json: r#"{"ok":true,"tool":"read_text","path":"AGENTS.md","bytes":19704,"content":"large raw content"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::Expanded { title, body } = &state.timeline()[0] else {
        panic!("read tool call should expand to a compact preview");
    };
    assert_eq!(title, "Read read_text path=AGENTS.md");
    assert!(!body.contains("AGENTS.md:1"));
    assert!(body.contains("large raw content"));
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
        title: "Ran .".to_owned(),
        body: "Cargo.toml".to_owned(),
    });

    let buffer = render_to_buffer(&state, 79, 18);

    assert_eq!(find_cell_color(&buffer, "Searched"), Some(Color::LightCyan));
    assert_eq!(find_cell_color(&buffer, "Ran"), Some(Color::LightCyan));
    assert_eq!(
        find_cell_color(&buffer, "query=hello_world.py"),
        Some(Color::White)
    );
}
