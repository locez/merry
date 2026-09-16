//! Coverage for `*** Delete File:` sections: removal, proposal/execution agreement, and the content lines a delete must reject.

use super::*;

#[test]
fn apply_patch_executor_deletes_existing_file() {
    let temp = TempWorkspace::new("patch-delete-success");
    temp.write_text("dir/gone.txt", "alpha\nbeta\n");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &delete_patch("dir/gone.txt"));

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert!(!temp.path().join("dir/gone.txt").exists());
    assert!(
        temp.path().join("dir").is_dir(),
        "deleting a file must leave its parent directory in place"
    );
    assert_eq!(
        json_content(&outcome),
        json!({
            "ok": true,
            "tool": APPLY_PATCH_TOOL,
            "changes": [{
                "path": "dir/gone.txt",
                "op": "delete",
                "hunks": 0,
                "lines_before": 2,
                "lines_after": 0,
                "bytes_before": "alpha\nbeta\n".len(),
                "bytes_after": 0,
                "lines": []
            }]
        })
    );
    let evidence = match outcome
        .execution_evidence()
        .expect("successful delete should include execution evidence")
    {
        ActionExecutionEvidence::WorkspacePatch(evidence) => evidence,
        ActionExecutionEvidence::ProcessAction(_) => {
            panic!("workspace patch execution must not produce process action evidence")
        }
    };
    assert_eq!(evidence.preimage_bytes(), "alpha\nbeta\n".len());
    assert_eq!(evidence.replacement_bytes(), 0);
    assert_eq!(evidence.file_bytes_before(), "alpha\nbeta\n".len());
    assert_eq!(evidence.file_bytes_after(), 0);
    assert_eq!(
        evidence.file_fingerprint_after(),
        &stable_content_fingerprint(b"")
    );
}

#[test]
fn apply_patch_delete_proposal_and_execution_match() {
    let temp = TempWorkspace::new("patch-delete-proposal");
    temp.write_text("note.txt", "alpha\n");
    let tools = tools_for(temp.path());
    let patch = delete_patch("note.txt");
    let proposal = match delete_preflight(&tools, "note.txt") {
        ToolActionPreflight::Proposal(proposal) => proposal,
        ToolActionPreflight::NoProposal | ToolActionPreflight::Outcome(_) => {
            panic!("delete patch should produce a proposal")
        }
    };
    let proposed = match proposal.evidence() {
        ActionProposalEvidence::WorkspacePatch(patch) => patch,
        ActionProposalEvidence::ProcessAction(_) => {
            panic!("workspace patch proposal must not produce process action evidence")
        }
    };
    assert_eq!(proposed.preimage_bytes(), "alpha\n".len());
    assert_eq!(proposed.replacement_bytes(), 0);
    assert_eq!(proposed.file_bytes_before(), "alpha\n".len());
    assert_eq!(proposed.file_bytes_after(), 0);

    let outcome = apply_patch_blocking_checked(
        &tools.state,
        ApplyPatchInput { patch },
        Some(proposed),
        &|| false,
    )
    .expect("uncancelled workspace patch should not return cancellation");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert!(!temp.path().join("note.txt").exists());
}

#[test]
fn apply_patch_delete_requires_an_existing_file() {
    let temp = TempWorkspace::new("patch-delete-missing");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &delete_patch("note.txt"));

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_FILE_NOT_FOUND,
        Some("note.txt"),
        temp.path(),
    );
}

#[test]
fn apply_patch_delete_rejects_content_lines() {
    let temp = TempWorkspace::new("patch-delete-content");
    temp.write_text("note.txt", "alpha\n");
    let tools = tools_for(temp.path());
    let patch = "*** Begin Patch
*** Delete File: note.txt
+unexpected
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
            .contains("must not contain content lines")
    );
    assert_eq!(read_text(&temp.path().join("note.txt")), "alpha\n");
}
