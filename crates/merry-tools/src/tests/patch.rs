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
                "hunks": 1,
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
            "hunks": 1,
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
        ERROR_INVALID_ARGUMENTS,
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

#[test]
fn apply_patch_respects_configured_write_scope() {
    let temp = TempWorkspace::new("patch-write-scope");
    temp.write_text("allowed/note.txt", "alpha\nold\nomega\n");
    temp.write_text("denied/note.txt", "alpha\nold\nomega\n");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
            .with_patch_write_scope(Some(vec![PathBuf::from("allowed")])),
    )
    .expect("workspace tools should construct");

    let allowed = patch_outcome(&tools, "allowed/note.txt", "old", "new");
    assert_eq!(allowed.status(), ToolCallResultStatus::Succeeded);

    let denied = patch_outcome(&tools, "denied/note.txt", "old", "new");
    assert_failed_json_for_tool(
        &denied,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("denied/note.txt"),
        temp.path(),
    );
    assert_eq!(
        read_text(&temp.path().join("denied/note.txt")),
        "alpha\nold\nomega\n"
    );
}

#[test]
fn apply_patch_forbidden_paths_override_write_scope() {
    let temp = TempWorkspace::new("patch-forbidden-scope");
    temp.write_text("allowed/public.txt", "alpha\nold\nomega\n");
    temp.write_text("allowed/secret.txt", "alpha\nold\nomega\n");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
            .with_patch_write_scope(Some(vec![PathBuf::from("allowed")]))
            .with_forbidden_paths(vec![PathBuf::from("allowed/secret.txt")]),
    )
    .expect("workspace tools should construct");

    let public = patch_outcome(&tools, "allowed/public.txt", "old", "new");
    assert_eq!(public.status(), ToolCallResultStatus::Succeeded);

    let forbidden = patch_outcome(&tools, "allowed/secret.txt", "old", "new");
    assert_failed_json_for_tool(
        &forbidden,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("allowed/secret.txt"),
        temp.path(),
    );
    assert_eq!(
        read_text(&temp.path().join("allowed/secret.txt")),
        "alpha\nold\nomega\n"
    );
}

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
}

#[test]
fn apply_patch_executor_rejects_an_update_with_only_context_hunks() {
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
        ERROR_INVALID_ARGUMENTS,
        Some("note.txt"),
        temp.path(),
    );
    assert!(
        outcome
            .diagnostic()
            .expect("diagnostic")
            .message()
            .contains("at least one edited hunk")
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
        ERROR_INVALID_ARGUMENTS,
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
