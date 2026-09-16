//! Coverage for a file named by several sections: merged updates, rejected repeats, multi-file envelopes, atomic failure, and dropped context-only files.

use super::*;

#[test]
fn apply_patch_executor_combines_add_and_update_operations() {
    let temp = TempWorkspace::new("patch-add-update");
    temp.write_text("existing.txt", "old\n");
    let tools = tools_for(temp.path());
    let patch = r#"*** Begin Workspace Patch
*** Add File: new.txt
+created
*** Update File: existing.txt
-old
+updated
*** End Workspace Patch"#;

    let outcome = patch_text_outcome(&tools, patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&temp.path().join("new.txt")), "created\n");
    assert_eq!(read_text(&temp.path().join("existing.txt")), "updated\n");
    assert_eq!(
        json_content(&outcome)["changes"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn apply_patch_executor_applies_multi_file_patch_and_records_each_change() {
    let temp = TempWorkspace::new("patch-multi-file-success");
    temp.write_text("src/lib.rs", "alpha\nold lib\nomega\n");
    temp.write_text("tests/smoke.rs", "alpha\nold test\nomega\n");
    let tools = tools_for(temp.path());
    let patch = "\
*** Begin Workspace Patch
*** Update File: src/lib.rs
-old lib
+new lib
*** Update File: tests/smoke.rs
-old test
+new test
*** End Workspace Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        read_text(&temp.path().join("src/lib.rs")),
        "alpha\nnew lib\nomega\n"
    );
    assert_eq!(
        read_text(&temp.path().join("tests/smoke.rs")),
        "alpha\nnew test\nomega\n"
    );
    let payload = json_content(&outcome);
    assert_eq!(payload["tool"], APPLY_PATCH_TOOL);
    assert_eq!(
        payload["changes"]
            .as_array()
            .expect("changes should be an array")
            .len(),
        2
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
    assert_eq!(evidence.changes().len(), 2);
    assert_eq!(evidence.changes()[0].relative_path(), "src/lib.rs");
    assert_eq!(evidence.changes()[1].relative_path(), "tests/smoke.rs");
}

#[test]
fn apply_patch_merges_repeated_update_sections_for_one_file() {
    let temp = TempWorkspace::new("patch-merged-sections");
    temp.write_text("note.txt", "one\ntwo\nthree\nfour\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: note.txt
@@
-one
+ONE
*** Update File: note.txt
@@
-four
+FOUR
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "ONE\ntwo\nthree\nFOUR\n"
    );
    let payload = json_content(&outcome);
    assert_eq!(payload["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["changes"][0]["op"], "update");
    assert_eq!(payload["changes"][0]["hunks"], 2);
}

#[test]
fn apply_patch_rejects_repeated_add_sections_for_one_file() {
    let temp = TempWorkspace::new("patch-repeated-add");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Add File: new.txt
+first
*** Add File: new.txt
+second
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATCH_SYNTAX,
        Some("new.txt"),
        temp.path(),
    );
    assert!(
        outcome
            .diagnostic()
            .expect("diagnostic")
            .message()
            .contains("more than once")
    );
    assert!(!temp.path().join("new.txt").exists());
}

#[test]
fn apply_patch_rejects_mixed_sections_for_one_file() {
    let temp = TempWorkspace::new("patch-mixed-sections");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: note.txt
@@
-old
+new
*** Delete File: note.txt
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATCH_SYNTAX,
        Some("note.txt"),
        temp.path(),
    );
    assert!(
        outcome
            .diagnostic()
            .expect("diagnostic")
            .message()
            .contains("mixes")
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nold\nomega\n"
    );
}

#[test]
fn apply_patch_drops_a_context_only_file_next_to_edits() {
    let temp = TempWorkspace::new("patch-context-only-file");
    temp.write_text("edited.txt", "alpha\nold\n");
    temp.write_text("anchors.txt", "alpha\nold\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: edited.txt
@@
-old
+new
*** Update File: anchors.txt
@@
 alpha
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&temp.path().join("edited.txt")), "alpha\nnew\n");
    assert_eq!(read_text(&temp.path().join("anchors.txt")), "alpha\nold\n");
    let payload = json_content(&outcome);
    assert_eq!(payload["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(payload["changes"][0]["path"], "edited.txt");
}

#[test]
fn apply_patch_merged_sections_fail_atomically_when_one_hunk_misses() {
    let temp = TempWorkspace::new("patch-merged-atomic");
    temp.write_text("note.txt", "one\ntwo\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: note.txt
@@
-one
+ONE
*** Update File: note.txt
@@
-missing
+MISSING
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PREIMAGE_ABSENT,
        Some("note.txt"),
        temp.path(),
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "one\ntwo\n",
        "a merged section must not write when any hunk misses"
    );
}
