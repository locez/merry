use crate::tui::{
    keymap::Keymap,
    projector::TuiProjector,
    render::render_to_text,
    state::{PatchChangeView, PatchLineView, PatchOperationView, TimelineItem, TuiState},
    tests::{pending_call, pending_call_with_args, source, text_artifact},
    theme::TuiTheme,
};
use merry_core::{ErrorInfo, RuntimeEvent, ToolCallId, ToolCallResult, ToolOutput};
use merry_tools::APPLY_PATCH_TOOL;
use serde_json::json;

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
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"read_text","path":"AGENTS.md","bytes":19704,"content":"large raw content"}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    let TimelineItem::Expanded { title, body } = &state.timeline()[0] else {
        panic!("read tool result should expand to a compact preview");
    };
    assert_eq!(title, "Read read_text");
    assert!(!body.contains("AGENTS.md:1"));
    assert!(body.contains("large raw content"));
    assert!(!body.contains(r#""content":"#));
}

#[test]
fn projector_projects_apply_patch_using_patch_tool_format() {
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
            call: pending_call_with_args("call-patch", APPLY_PATCH_TOOL, json!({ "patch": patch })),
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
                json: r#"{"ok":true,"tool":"apply_patch","changes":[{"path":"crates/merry-cli/src/tui/render.rs","hunks":1,"bytes_before":120,"bytes_after":121,"lines":[{"kind":"context","old_line":10,"new_line":10,"text":"    let old = true;"},{"kind":"remove","old_line":11,"text":"    lines.push(old);"},{"kind":"add","new_line":11,"text":"    lines.push(new);"}]}]}"#.to_owned(),
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
        changes[0].operation,
        PatchOperationView::Update,
        "envelopes recorded before the op field describe updates"
    );
    assert_eq!(changes[0].lines_before, None);
    assert_eq!(changes[0].lines_after, None);
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
fn projector_projects_apply_patch_add_file_line_numbers() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    let patch = "*** Begin Workspace Patch\n*** Add File: hello.txt\n+hello\n+world\n*** End Workspace Patch";
    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-add-file",
                APPLY_PATCH_TOOL,
                json!({ "patch": patch }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-add-file").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"apply_patch","changes":[{"path":"hello.txt","hunks":1,"bytes_before":0,"bytes_after":12}]}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::Patch { changes } = &state.timeline()[0] else {
        panic!("workspace add patch should render as a patch view");
    };
    assert_eq!(
        changes[0].lines,
        vec![
            PatchLineView::add("hello", Some(1)),
            PatchLineView::add("world", Some(2)),
        ]
    );
}

#[test]
fn projector_reads_delete_and_line_counts_from_the_envelope() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();
    let patch = "*** Begin Patch\n*** Delete File: notes/old.md\n*** End Patch";

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-delete-file",
                APPLY_PATCH_TOOL,
                json!({ "patch": patch }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::succeeded(
                ToolCallId::new("call-delete-file").unwrap(),
                text_artifact("patch-output"),
            ),
            output: Some(ToolOutput::Json {
                json: r#"{"ok":true,"tool":"apply_patch","changes":[{"path":"notes/old.md","op":"delete","hunks":0,"lines_before":4,"lines_after":0,"bytes_before":30,"bytes_after":0,"lines":[]}]}"#.to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    let TimelineItem::Patch { changes } = &state.timeline()[0] else {
        panic!("workspace delete patch should render as a patch view");
    };
    assert_eq!(changes[0].operation, PatchOperationView::Delete);
    assert_eq!(changes[0].added, 0);
    assert_eq!(
        changes[0].removed, 4,
        "a delete removes the whole file, so its line count is the removal count"
    );
    assert_eq!(changes[0].lines_before, Some(4));
    assert_eq!(changes[0].lines_after, Some(0));

    let text = render_to_text(&state, 180, 32);
    assert!(text.contains("Deleted notes/old.md (-4)"));
    assert!(text.contains("4 -> 0 lines, 30 -> 0 bytes"));
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
                APPLY_PATCH_TOOL,
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
                json: r#"{"ok":true,"tool":"apply_patch","changes":[{"path":"hello_world.py","hunks":1,"bytes_before":209,"bytes_after":222}]}"#.to_owned(),
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
fn projector_replaces_failed_patch_row_without_leaving_stale_pending_row() {
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
-    lines.push(old);
+    lines.push(new);
*** End Patch";

    projector.apply(
        RuntimeEvent::ToolCallStarted {
            call: pending_call_with_args(
                "call-patch-fail",
                APPLY_PATCH_TOOL,
                json!({ "patch": patch }),
            ),
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::ToolCallFinished {
            result: ToolCallResult::failed(
                ToolCallId::new("call-patch-fail").unwrap(),
                text_artifact("patch-failure"),
                ErrorInfo::new(
                    "apply_patch_preimage_mismatch",
                    "preimage text was not found in the target file",
                )
                .unwrap(),
            ),
            output: Some(ToolOutput::Json {
                json: json!({
                    "ok": false,
                    "tool": "apply_patch",
                    "error": {
                        "code": "apply_patch_preimage_mismatch",
                        "message": "preimage text was not found in the target file"
                    },
                    "recovery": {
                        "instruction": "Read the current file content and retry with a matching preimage."
                    }
                })
                .to_string(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(
        state.timeline().len(),
        1,
        "failed patch tool should replace its pending row, not leave a stale row"
    );
    let TimelineItem::Diagnostic { title, body } = &state.timeline()[0] else {
        panic!("failed patch tool should replace the pending row with a diagnostic");
    };
    assert!(title.starts_with("Patch apply_patch"));
    assert!(title.contains("crates/merry-cli/src/tui/render.rs"));
    assert!(title.ends_with("-> failed"));
    assert!(body.contains("apply_patch_preimage_mismatch"));
    assert!(body.contains("preimage text was not found"));
    assert!(body.contains("Read the current file content and retry"));
    assert!(
        !body.contains("*** Begin Patch"),
        "failed patch body must not leak the full patch payload"
    );
}

#[test]
fn renderer_shows_apply_patch_as_edited_block() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "crates/merry-cli/src/tui/render.rs".to_owned(),
            operation: PatchOperationView::Update,
            added: 1,
            removed: 1,
            hunks: 1,
            lines_before: Some(810),
            lines_after: Some(911),
            bytes_before: Some(120),
            bytes_after: Some(121),
            lines: vec![
                PatchLineView::context("    let old = true;", Some(20)),
                PatchLineView::remove("    lines.push(old);", Some(21)),
                PatchLineView::add("    lines.push(new);", Some(21)),
            ],
        }],
    });
    let text = render_to_text(&state, 180, 16);

    assert!(text.contains("Edited crates/merry-cli/src/tui/render.rs (+1 -1)"));
    assert!(text.contains("1 hunk(s), 810 -> 911 lines, 120 -> 121 bytes"));
    assert!(!text.contains("\"changes\""));
}

#[test]
fn projector_expands_apply_patch_by_tool_name() {
    let mut state = TuiState::new(
        "/repo".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    let mut projector = TuiProjector::default();

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
            output: Some(ToolOutput::Json {
                json: r#"{"tool":"apply_patch","status":"applied"}"#.to_owned(),
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
                text: "--- a/src/lib.rs\n+++ b/src/lib.rs\n+not a patch result".to_owned(),
            }),
            source: source(),
        },
        &mut state,
    );

    assert_eq!(state.timeline().len(), 1);
    assert!(matches!(state.timeline()[0], TimelineItem::Expanded { .. }));
}

#[test]
fn renderer_shows_patch_summary_in_the_timeline() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "hello_world.py".to_owned(),
            operation: PatchOperationView::Update,
            added: 1,
            removed: 1,
            hunks: 1,
            lines_before: Some(6),
            lines_after: Some(6),
            bytes_before: Some(20),
            bytes_after: Some(21),
            lines: vec![
                PatchLineView::remove("print('old')", Some(7)),
                PatchLineView::add("print('new')", Some(7)),
            ],
        }],
    });
    let text = render_to_text(&state, 180, 32);

    assert!(text.contains("Edited hello_world.py (+1 -1)"));
    assert!(text.contains("1 hunk(s), 6 -> 6 lines, 20 -> 21 bytes"));
}

#[test]
fn renderer_names_created_and_deleted_files_instead_of_edits() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![
            PatchChangeView {
                path: "notes/new.md".to_owned(),
                operation: PatchOperationView::Add,
                added: 3,
                removed: 0,
                hunks: 1,
                lines_before: Some(0),
                lines_after: Some(3),
                bytes_before: Some(0),
                bytes_after: Some(24),
                lines: vec![PatchLineView::add("new", Some(1))],
            },
            PatchChangeView {
                path: "notes/old.md".to_owned(),
                operation: PatchOperationView::Delete,
                added: 0,
                removed: 4,
                hunks: 0,
                lines_before: Some(4),
                lines_after: Some(0),
                bytes_before: Some(30),
                bytes_after: Some(0),
                lines: vec![],
            },
        ],
    });
    let text = render_to_text(&state, 180, 32);

    assert!(text.contains("Created notes/new.md (+3)"));
    assert!(text.contains("0 -> 3 lines, 0 -> 24 bytes"));
    assert!(text.contains("Deleted notes/old.md (-4)"));
    assert!(text.contains("4 -> 0 lines, 30 -> 0 bytes"));
    assert!(
        !text.contains("hunk(s)"),
        "add and delete changes have no hunks to count: {text}"
    );
}

#[test]
fn renderer_keeps_byte_only_envelopes_readable() {
    let mut state = TuiState::new(
        "/repo/merry".into(),
        "gpt-test".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    );
    state.push_timeline_item(TimelineItem::Patch {
        changes: vec![PatchChangeView {
            path: "hello.txt".to_owned(),
            operation: PatchOperationView::Update,
            added: 2,
            removed: 0,
            hunks: 1,
            lines_before: None,
            lines_after: None,
            bytes_before: Some(12),
            bytes_after: Some(33),
            lines: vec![PatchLineView::add("hello", Some(3))],
        }],
    });
    let text = render_to_text(&state, 180, 32);

    assert!(text.contains("Edited hello.txt (+2 -0)"));
    assert!(text.contains("1 hunk(s), - -> - lines, 12 -> 33 bytes"));
}
