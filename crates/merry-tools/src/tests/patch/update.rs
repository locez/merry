//! Coverage for `*** Update File:` hunks applied to one existing file.

use super::*;

#[test]
fn apply_patch_executor_replaces_one_hunk_in_existing_utf8_file() {
    let temp = TempWorkspace::new("patch-success");
    temp.write_text("dir/note.txt", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());
    let executor = ApplyPatchExecutor {
        state: Arc::clone(&tools.state),
    };
    let patch = update_patch("dir/note.txt", "old value", "new value");
    let call = pending_call_for(APPLY_PATCH_TOOL, json!({ "patch": patch }));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime should build");

    let outcome = runtime
        .block_on(executor.execute(call, ToolExecutionContext::default()))
        .expect("patch executor should succeed");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(
        payload,
        json!({
            "ok": true,
            "tool": APPLY_PATCH_TOOL,
            "changes": [{
                "path": "dir/note.txt",
                "op": "update",
                "hunks": 1,
                "lines_before": 3,
                "lines_after": 3,
                "bytes_before": 22,
                "bytes_after": 22,
                "lines": [
                    { "kind": "remove", "old_line": 2, "text": "old value" },
                    { "kind": "add", "new_line": 2, "text": "new value" }
                ]
            }]
        })
    );
    assert_eq!(
        read_text(&temp.path().join("dir/note.txt")),
        "alpha\nnew value\nomega\n"
    );
    let evidence = match outcome
        .execution_evidence()
        .expect("successful patch should include internal execution evidence")
    {
        ActionExecutionEvidence::WorkspacePatch(evidence) => evidence,
        ActionExecutionEvidence::ProcessAction(_) => {
            panic!("workspace patch execution must not produce process action evidence")
        }
    };
    assert_eq!(evidence.relative_path(), "dir/note.txt");
    assert_eq!(evidence.preimage_bytes(), "old value\n".len());
    assert_eq!(evidence.replacement_bytes(), "new value\n".len());
    assert_eq!(evidence.file_bytes_before(), 22);
    assert_eq!(evidence.file_bytes_after(), 22);
    assert_eq!(
        evidence.file_fingerprint_before(),
        &stable_content_fingerprint("alpha\nold value\nomega\n".as_bytes())
    );
    assert_eq!(
        evidence.file_fingerprint_after(),
        &stable_content_fingerprint("alpha\nnew value\nomega\n".as_bytes())
    );
    assert!(
        !outcome
            .content()
            .as_text()
            .expect("json content")
            .contains(temp.path().to_str().expect("temp path utf8")),
        "tool output must not include absolute host roots"
    );
}

#[test]
fn apply_patch_executor_ignores_context_only_hunks_when_editing() {
    let temp = TempWorkspace::new("patch-context-only-hunk");
    temp.write_text("note.txt", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Workspace Patch
*** Update File: note.txt
@@
-old value
+new value
@@
 omega
*** End Workspace Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nnew value\nomega\n"
    );
    assert_eq!(json_content(&outcome)["changes"][0]["hunks"], 1);
    assert_eq!(
        json_content(&outcome)["changes"][0]["ignored_context_hunks"],
        1,
        "dropped context-only hunks should stay visible in the success envelope"
    );
}

#[test]
fn apply_patch_executor_reports_old_and_new_line_numbers_after_prior_hunk_delta() {
    let temp = TempWorkspace::new("patch-line-number-delta");
    temp.write_text(
        "src/lib.rs",
        "one\ninsert anchor\nmiddle\nremove anchor\nlast\n",
    );
    let tools = tools_for(temp.path());
    let patch = "\
+intro
 insert anchor
@@
-remove anchor
+changed anchor";
    let patch = format!(
        "*** Begin Workspace Patch
*** Update File: src/lib.rs
{patch}
*** End Workspace Patch"
    );

    let outcome = patch_text_outcome(&tools, &patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        read_text(&temp.path().join("src/lib.rs")),
        "one\nintro\ninsert anchor\nmiddle\nchanged anchor\nlast\n"
    );
    let payload = json_content(&outcome);
    assert_eq!(
        payload["changes"][0]["lines"],
        json!([
            { "kind": "add", "new_line": 2, "text": "intro" },
            { "kind": "context", "old_line": 2, "new_line": 3, "text": "insert anchor" },
            { "kind": "remove", "old_line": 4, "text": "remove anchor" },
            { "kind": "add", "new_line": 5, "text": "changed anchor" }
        ])
    );
}
