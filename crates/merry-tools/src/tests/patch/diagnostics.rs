//! Coverage for the failure text a caller recovers with: which line diverged, which lines matched twice, and CRLF as a match cause.

use super::*;

#[test]
fn apply_patch_preimage_miss_reports_the_first_differing_line() {
    let temp = TempWorkspace::new("patch-preimage-divergence");
    temp.write_text("note.txt", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: note.txt
@@
 alpha
-stale value
+new value
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PREIMAGE_ABSENT,
        Some("note.txt"),
        temp.path(),
    );
    let message = outcome
        .diagnostic()
        .expect("diagnostic")
        .message()
        .to_owned();
    assert_eq!(
        message,
        "workspace patch preimage was not found; the hunk's first line matches at line 1, but line 2 differs: the patch has \"stale value\" but the file has \"old value\""
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nold value\nomega\n"
    );
}

#[test]
fn apply_patch_preimage_miss_reports_an_unfindable_hunk_line() {
    let temp = TempWorkspace::new("patch-preimage-unfindable");
    temp.write_text("note.txt", "alpha\nbeta\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: note.txt
@@
-missing anchor line
+replacement
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PREIMAGE_ABSENT,
        Some("note.txt"),
        temp.path(),
    );
    let message = outcome
        .diagnostic()
        .expect("diagnostic")
        .message()
        .to_owned();
    assert!(
        message.contains("missing anchor line") && message.contains("not found in the file"),
        "diagnostic should quote the missing hunk line: {message}"
    );
}

#[test]
fn apply_patch_preimage_miss_reports_crlf_as_the_match_cause() {
    let temp = TempWorkspace::new("patch-preimage-crlf");
    temp.write_text("note.txt", "alpha\r\nold\r\nomega\r\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: note.txt
@@
-old
+new
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PREIMAGE_ABSENT,
        Some("note.txt"),
        temp.path(),
    );
    let message = outcome
        .diagnostic()
        .expect("diagnostic")
        .message()
        .to_owned();
    assert_eq!(
        message,
        "workspace patch preimage was not found; all 1 hunk line(s) match at line 2, but the file uses CRLF line endings and this tool matches bytes exactly; convert the file to LF first (for example with a process command) or edit it without apply_patch"
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\r\nold\r\nomega\r\n"
    );
}

#[test]
fn apply_patch_ambiguous_preimage_reports_every_match_line() {
    let temp = TempWorkspace::new("patch-preimage-ambiguity-lines");
    temp.write_text("note.txt", "dup\nmid\ndup\nmid\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Update File: note.txt
@@
-dup
-mid
+dup
+MID
*** End Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PREIMAGE_AMBIGUOUS,
        Some("note.txt"),
        temp.path(),
    );
    let message = outcome
        .diagnostic()
        .expect("diagnostic")
        .message()
        .to_owned();
    assert!(
        message.contains("lines 1, 3"),
        "diagnostic should list candidate lines: {message}"
    );
}
