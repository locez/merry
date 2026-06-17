use super::controller::{ControllerEffect, handle_key_action};
use super::input::TextInput;
use super::keymap::{KeyAction, KeyBinding, Keymap};
use super::projector::TuiProjector;
use super::render::{render_to_buffer, render_to_text};
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
use ratatui::style::Color;
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
    assert_eq!(state.timeline_scroll_offset(), initial.saturating_add(1));

    assert_eq!(
        handle_key_action(KeyAction::ScrollDown, &mut state),
        ControllerEffect::None
    );
    assert_eq!(state.timeline_scroll_offset(), initial);
}

#[test]
fn default_keymap_maps_enter_to_submit_next_and_ctrl_b_to_backlog() {
    let keymap = Keymap::default();

    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(KeyAction::SubmitNext)
    );
    assert_eq!(
        keymap.action_for(KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL,)),
        Some(KeyAction::SubmitBacklog)
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
        SemanticColor::Selection,
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
    assert_eq!(title, "tool");
    assert_eq!(detail, "workspace_read_file path=AGENTS.md");
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
                json: r#"{"ok":true,"tool":"workspace_patch","changes":[{"path":"crates/merry-cli/src/tui/render.rs","hunks":1,"bytes_before":120,"bytes_after":121}]}"#.to_owned(),
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
            PatchLineView::Context("    let old = true;".to_owned()),
            PatchLineView::Remove("    lines.push(old);".to_owned()),
            PatchLineView::Add("    lines.push(new);".to_owned()),
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
                PatchLineView::Context("    let old = true;".to_owned()),
                PatchLineView::Remove("    lines.push(old);".to_owned()),
                PatchLineView::Add("    lines.push(new);".to_owned()),
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
    assert!(state.status_text().contains("13 tok"));
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
    assert!(scrolled.contains("line 8"));
    assert!(!scrolled.contains("line 11"));
}

#[test]
fn renderer_applies_configured_semantic_theme_colors() {
    let theme = TuiTheme::from_config(&crate::config::TuiThemeToml {
        status: Some("red".to_owned()),
        muted: Some("blue".to_owned()),
        focus: Some("magenta".to_owned()),
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

    let buffer = render_to_buffer(&state, 80, 18);

    assert_eq!(find_cell_color(&buffer, "gpt-test"), Some(Color::Red));
    assert_eq!(find_cell_color(&buffer, "tool"), Some(Color::Blue));
    assert_eq!(find_cell_color(&buffer, "patch"), Some(Color::Magenta));
    assert_eq!(find_cell_color(&buffer, "+added"), Some(Color::Green));
    assert_eq!(find_cell_color(&buffer, "-removed"), Some(Color::Yellow));
    assert_eq!(find_cell_color(&buffer, "Next"), Some(Color::Magenta));
    assert_eq!(find_cell_color(&buffer, "queued"), Some(Color::Blue));
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
                return Some(buffer[(x, y)].fg);
            }
        }
    }
    None
}
