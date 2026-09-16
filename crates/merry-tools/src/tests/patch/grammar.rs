//! Coverage for envelope-level grammar: begin and end markers, the standard alias, the anchor a hunk needs, and a patch with no edit.

use super::*;

#[test]
fn apply_patch_executor_accepts_standard_patch_envelope_alias() {
    let temp = TempWorkspace::new("patch-standard-envelope-alias");
    temp.write_text("src/lib.rs", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());
    let patch = "\
*** Begin Patch
*** Update File: src/lib.rs
-old value
+new value
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        read_text(&temp.path().join("src/lib.rs")),
        "alpha\nnew value\nomega\n"
    );
}

#[test]
fn apply_patch_executor_rejects_a_context_only_envelope_with_recovery_hint() {
    let temp = TempWorkspace::new("patch-only-context-hunk");
    temp.write_text("note.txt", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Workspace Patch
*** Update File: note.txt
@@
 old value
*** End Workspace Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATCH_NOOP,
        Some("note.txt"),
        temp.path(),
    );
    assert_eq!(
        outcome.diagnostic().expect("diagnostic").message(),
        "workspace patch contains no `+` or `-` lines, so nothing would change. Send the added or removed lines as `+`/`-` hunk lines to edit the file, or use `read_text` when you only need to inspect the current content"
    );
    assert!(
        json_content(&outcome)["guidance"]["message"]
            .as_str()
            .expect("guidance text")
            .contains("nothing was written")
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nold value\nomega\n"
    );
}

#[test]
fn apply_patch_executor_rejects_duplicate_begin_marker_without_mutation() {
    let temp = TempWorkspace::new("patch-duplicate-begin");
    temp.write_text("note.txt", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Workspace Patch
*** Begin Patch
*** Update File: note.txt
-old value
+new value
*** End Workspace Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATCH_SYNTAX,
        None,
        temp.path(),
    );
    assert!(
        outcome
            .diagnostic()
            .expect("diagnostic")
            .message()
            .contains("duplicate begin marker")
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nold value\nomega\n"
    );
}

#[test]
fn apply_patch_rejects_a_missing_begin_marker_with_the_offending_line() {
    let temp = TempWorkspace::new("patch-missing-begin-marker");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());
    let patch = "*** Update File: note.txt
-old
+new
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATCH_SYNTAX,
        None,
        temp.path(),
    );
    let message = outcome
        .diagnostic()
        .expect("diagnostic")
        .message()
        .to_owned();
    assert!(
        message.contains("*** Update File: note.txt"),
        "diagnostic should quote the first non-blank line: {message}"
    );
    assert_eq!(
        json_content(&outcome)["guidance"]["kind"],
        "apply_patch_syntax"
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nold\nomega\n"
    );
}

#[test]
fn apply_patch_rejects_a_hunk_without_an_anchor_line() {
    let temp = TempWorkspace::new("patch-no-anchor");
    temp.write_text("note.txt", "alpha\nbeta\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: note.txt
@@
+inserted
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
            .contains("only + lines and no anchor")
    );
    assert_eq!(read_text(&temp.path().join("note.txt")), "alpha\nbeta\n");
}
