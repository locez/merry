use super::read::read_text_range_outcome;
use super::*;

fn model_body(outcome: &ToolExecutionOutcome) -> &str {
    outcome
        .model_content()
        .expect("model body")
        .as_text()
        .expect("text or JSON")
}

#[test]
fn read_model_body_preserves_whitespace_range_and_truncation() {
    let temp = TempWorkspace::new("model-read");
    temp.write_text("note.txt", "one\n  two\r\nthree\nfour\n");
    let tools = tools_for(temp.path());
    let outcome = read_text_range_outcome(&tools, "note.txt", 2, 2);
    assert_eq!(
        model_body(&outcome),
        "note.txt:2-3\n  two\r\nthree\n[truncated: more file content remains; read from line 4]"
    );
    assert_eq!(json_content(&outcome)["content"], "  two\r\nthree\n");
    let tail = read_text_range_outcome(&tools, "note.txt", 4, 2);
    assert_eq!(model_body(&tail), "note.txt:4-4\nfour\n");
}

#[test]
fn empty_and_out_of_range_reads_report_the_requested_start_without_inventing_lines() {
    let temp = TempWorkspace::new("model-read-empty");
    temp.write_text("empty.txt", "");
    temp.write_text("short.txt", "one\n");
    let tools = tools_for(temp.path());
    assert_eq!(
        model_body(&read_outcome(&tools, "empty.txt")),
        "empty.txt: empty file"
    );
    assert_eq!(
        model_body(&read_text_range_outcome(&tools, "short.txt", 10, 2)),
        "short.txt: no lines returned from line 10 (past end of file)"
    );
}

#[test]
fn patch_model_body_reports_actual_operations_without_echoing_the_patch() {
    let temp = TempWorkspace::new("model-patch");
    temp.write_text("update.txt", "old\n");
    temp.write_text("delete.txt", "remove\n");
    let tools = tools_for(temp.path());
    let outcome = patch_text_outcome(
        &tools,
        "*** Begin Patch\n*** Add File: add.txt\n+new\n*** Update File: update.txt\n@@\n-old\n+replacement\n*** Delete File: delete.txt\n*** End Patch",
    );
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        model_body(&outcome),
        "Applied patch:\nadded add.txt\nupdated update.txt\ndeleted delete.txt"
    );
    assert_eq!(read_text(&temp.path().join("add.txt")), "new\n");
    assert_eq!(read_text(&temp.path().join("update.txt")), "replacement\n");
    assert!(!temp.path().join("delete.txt").exists());
    assert!(
        outcome
            .content()
            .as_text()
            .expect("full result")
            .contains("replacement")
    );
    assert!(model_body(&outcome).len() * 2 < outcome.content().as_bytes().len());
}

#[test]
fn patch_model_body_keeps_ignored_hunk_warning() {
    let temp = TempWorkspace::new("model-patch-context");
    temp.write_text("note.txt", "one\nold\n");
    let tools = tools_for(temp.path());
    let outcome = patch_text_outcome(
        &tools,
        "*** Begin Patch\n*** Update File: note.txt\n@@\n one\n@@\n-old\n+new\n*** End Patch",
    );
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        model_body(&outcome),
        "Applied patch:\nupdated note.txt (ignored 1 context-only hunks)"
    );
}

#[test]
fn failed_model_body_preserves_exact_error_path_and_applicable_guidance() {
    let temp = TempWorkspace::new("model-failure");
    temp.write_text("note.txt", "current\n");
    let tools = tools_for(temp.path());
    let outcomes = [
        read_outcome(&tools, "missing.txt"),
        patch_text_outcome(
            &tools,
            "*** Begin Patch\n*** Update File: note.txt\n@@\n-stale\n+new\n*** End Patch",
        ),
    ];
    for outcome in outcomes {
        assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
        let full = json_content(&outcome);
        let model: Value = serde_json::from_str(model_body(&outcome)).expect("model JSON");
        assert_eq!(model["error"], full["error"]);
        assert_eq!(model["path"], full["path"]);
        assert_eq!(model["guidance"], full["guidance"]["message"]);
        assert!(model.get("recovery").is_none());
        assert!(model.get("tool").is_none());
    }
}
