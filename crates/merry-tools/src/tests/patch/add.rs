//! Coverage for `*** Add File:` sections: creation, parent directories, and the limits that stop a bad add before it creates anything.

use super::*;

#[test]
fn apply_patch_executor_adds_new_utf8_file() {
    let temp = TempWorkspace::new("patch-add-success");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("dir/nested/new.txt", &["alpha", "beta"]));

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        read_text(&temp.path().join("dir/nested/new.txt")),
        "alpha\nbeta\n"
    );
    assert_eq!(
        json_content(&outcome)["changes"][0],
        json!({
            "path": "dir/nested/new.txt",
            "op": "add",
            "hunks": 1,
            "lines_before": 0,
            "lines_after": 2,
            "bytes_before": 0,
            "bytes_after": "alpha\nbeta\n".len(),
            "lines": [
                { "kind": "add", "new_line": 1, "text": "alpha" },
                { "kind": "add", "new_line": 2, "text": "beta" }
            ]
        })
    );

    let evidence = match outcome
        .execution_evidence()
        .expect("successful add should include execution evidence")
    {
        ActionExecutionEvidence::WorkspacePatch(evidence) => evidence,
        ActionExecutionEvidence::ProcessAction(_) => {
            panic!("workspace patch execution must not produce process action evidence")
        }
    };
    assert_eq!(evidence.preimage_bytes(), 0);
    assert_eq!(evidence.replacement_bytes(), "alpha\nbeta\n".len());
    assert_eq!(evidence.file_bytes_before(), 0);
    assert_eq!(evidence.file_bytes_after(), "alpha\nbeta\n".len());
    assert_eq!(
        evidence.file_fingerprint_before(),
        &stable_content_fingerprint(b"")
    );
    assert_eq!(
        evidence.file_fingerprint_after(),
        &stable_content_fingerprint(b"alpha\nbeta\n")
    );
}

#[test]
fn apply_patch_add_file_proposal_and_execution_match() {
    let temp = TempWorkspace::new("patch-add-proposal");
    fs::create_dir_all(temp.path().join("dir")).expect("parent directory should be created");
    let tools = tools_for(temp.path());
    let patch = add_patch("dir/new.txt", &["alpha"]);
    let proposal = match add_patch_preflight(&tools, "dir/new.txt", &["alpha"]) {
        ToolActionPreflight::Proposal(proposal) => proposal,
        ToolActionPreflight::NoProposal | ToolActionPreflight::Outcome(_) => {
            panic!("new file patch should produce a proposal")
        }
    };
    let proposed_patch = match proposal.evidence() {
        ActionProposalEvidence::WorkspacePatch(patch) => patch,
        ActionProposalEvidence::ProcessAction(_) => {
            panic!("workspace patch proposal must not produce process action evidence")
        }
    };
    assert_eq!(proposed_patch.preimage_bytes(), 0);
    assert_eq!(proposed_patch.file_bytes_before(), 0);
    assert_eq!(proposed_patch.file_bytes_after(), "alpha\n".len());

    let outcome = apply_patch_blocking_checked(
        &tools.state,
        ApplyPatchInput { patch },
        Some(proposed_patch),
        &|| false,
    )
    .expect("uncancelled workspace patch should not return cancellation");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&temp.path().join("dir/new.txt")), "alpha\n");
}

#[test]
fn apply_patch_add_file_does_not_overwrite_existing_file() {
    let temp = TempWorkspace::new("patch-add-existing");
    temp.write_text("note.txt", "old\n");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("note.txt", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_FILE_ALREADY_EXISTS,
        Some("note.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("note.txt")), "old\n");
}

#[test]
fn apply_patch_add_file_requires_plus_lines() {
    let temp = TempWorkspace::new("patch-add-invalid");
    let tools = tools_for(temp.path());
    let patch =
        "*** Begin Workspace Patch\n*** Add File: new.txt\ncontent\n*** End Workspace Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATCH_SYNTAX,
        Some("new.txt"),
        temp.path(),
    );
    assert!(!temp.path().join("new.txt").exists());
}

#[test]
fn apply_patch_add_file_tolerates_structural_blank_lines() {
    let temp = TempWorkspace::new("patch-add-blank-lines");
    let tools = tools_for(temp.path());
    let patch =
        "*** Begin Workspace Patch\n*** Add File: new.txt\n\n+created\n\n*** End Workspace Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&temp.path().join("new.txt")), "created\n");
}

#[test]
fn apply_patch_add_file_rejects_non_directory_parent() {
    let temp = TempWorkspace::new("patch-add-parent-file");
    temp.write_text("parent", "not a directory\n");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("parent/new.txt", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_NOT_DIRECTORY,
        Some("parent/new.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("parent")), "not a directory\n");
}

#[test]
fn apply_patch_add_file_rejects_existing_directory() {
    let temp = TempWorkspace::new("patch-add-directory");
    fs::create_dir(temp.path().join("dir")).expect("directory should be created");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("dir", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_FILE_ALREADY_EXISTS,
        Some("dir"),
        temp.path(),
    );
}

#[test]
fn apply_patch_add_file_rejects_write_limit_without_creating_file() {
    let temp = TempWorkspace::new("patch-add-write-limit");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_write_bytes: 4,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");

    let outcome = patch_text_outcome(&tools, &add_patch("new.txt", &["too large"]));

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_FILE_TOO_LARGE,
        Some("new.txt"),
        temp.path(),
    );
    assert!(!temp.path().join("new.txt").exists());
}
