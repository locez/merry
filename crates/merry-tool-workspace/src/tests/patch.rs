use super::*;

#[test]
fn workspace_patch_executor_replaces_one_hunk_in_existing_utf8_file() {
    let temp = TempWorkspace::new("patch-success");
    temp.write_text("dir/note.txt", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());
    let executor = WorkspacePatchExecutor {
        state: Arc::clone(&tools.state),
    };
    let patch = update_patch("dir/note.txt", "old value", "new value");
    let call = pending_call_for(WORKSPACE_PATCH_TOOL, json!({ "patch": patch }));
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
            "tool": WORKSPACE_PATCH_TOOL,
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
fn workspace_patch_executor_adds_new_utf8_file() {
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
fn workspace_patch_add_file_proposal_and_execution_match() {
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

    let outcome = workspace_patch_blocking_checked(
        &tools.state,
        WorkspacePatchArgs { patch },
        Some(proposed_patch),
        &|| false,
    )
    .expect("uncancelled workspace patch should not return cancellation");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&temp.path().join("dir/new.txt")), "alpha\n");
}

#[test]
fn workspace_patch_executor_combines_add_and_update_operations() {
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
fn workspace_patch_add_file_does_not_overwrite_existing_file() {
    let temp = TempWorkspace::new("patch-add-existing");
    temp.write_text("note.txt", "old\n");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("note.txt", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_FILE_ALREADY_EXISTS,
        Some("note.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("note.txt")), "old\n");
}

#[test]
fn workspace_patch_add_file_requires_plus_lines() {
    let temp = TempWorkspace::new("patch-add-invalid");
    let tools = tools_for(temp.path());
    let patch =
        "*** Begin Workspace Patch\n*** Add File: new.txt\ncontent\n*** End Workspace Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_INVALID_ARGUMENTS,
        Some("new.txt"),
        temp.path(),
    );
    assert!(!temp.path().join("new.txt").exists());
}

#[test]
fn workspace_patch_add_file_tolerates_structural_blank_lines() {
    let temp = TempWorkspace::new("patch-add-blank-lines");
    let tools = tools_for(temp.path());
    let patch =
        "*** Begin Workspace Patch\n*** Add File: new.txt\n\n+created\n\n*** End Workspace Patch";

    let outcome = patch_text_outcome(&tools, patch);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&temp.path().join("new.txt")), "created\n");
}

#[test]
fn workspace_patch_add_file_rejects_non_directory_parent() {
    let temp = TempWorkspace::new("patch-add-parent-file");
    temp.write_text("parent", "not a directory\n");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("parent/new.txt", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_NOT_DIRECTORY,
        Some("parent/new.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("parent")), "not a directory\n");
}

#[test]
fn workspace_patch_add_file_rejects_existing_directory() {
    let temp = TempWorkspace::new("patch-add-directory");
    fs::create_dir(temp.path().join("dir")).expect("directory should be created");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("dir", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_FILE_ALREADY_EXISTS,
        Some("dir"),
        temp.path(),
    );
}

#[test]
fn workspace_patch_add_file_rejects_write_limit_without_creating_file() {
    let temp = TempWorkspace::new("patch-add-write-limit");
    let tools = ReadOnlyWorkspaceTools::new(
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
        WORKSPACE_PATCH_TOOL,
        ERROR_FILE_TOO_LARGE,
        Some("new.txt"),
        temp.path(),
    );
    assert!(!temp.path().join("new.txt").exists());
}

#[test]
fn workspace_patch_respects_configured_write_scope() {
    let temp = TempWorkspace::new("patch-write-scope");
    temp.write_text("allowed/note.txt", "alpha\nold\nomega\n");
    temp.write_text("denied/note.txt", "alpha\nold\nomega\n");
    let tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
            .with_patch_write_scope(Some(vec![PathBuf::from("allowed")])),
    )
    .expect("workspace tools should construct");

    let allowed = patch_outcome(&tools, "allowed/note.txt", "old", "new");
    assert_eq!(allowed.status(), ToolCallResultStatus::Succeeded);

    let denied = patch_outcome(&tools, "denied/note.txt", "old", "new");
    assert_failed_json_for_tool(
        &denied,
        WORKSPACE_PATCH_TOOL,
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
fn workspace_patch_forbidden_paths_override_write_scope() {
    let temp = TempWorkspace::new("patch-forbidden-scope");
    temp.write_text("allowed/public.txt", "alpha\nold\nomega\n");
    temp.write_text("allowed/secret.txt", "alpha\nold\nomega\n");
    let tools = ReadOnlyWorkspaceTools::new(
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
        WORKSPACE_PATCH_TOOL,
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
fn workspace_patch_executor_accepts_standard_patch_envelope_alias() {
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
fn workspace_patch_executor_ignores_context_only_hunks_when_editing() {
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
fn workspace_patch_executor_rejects_an_update_with_only_context_hunks() {
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
        WORKSPACE_PATCH_TOOL,
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
fn workspace_patch_executor_rejects_duplicate_begin_marker_without_mutation() {
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
        WORKSPACE_PATCH_TOOL,
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
fn workspace_patch_executor_reports_old_and_new_line_numbers_after_prior_hunk_delta() {
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
fn workspace_patch_executor_applies_multi_file_patch_and_records_each_change() {
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
    assert_eq!(payload["tool"], WORKSPACE_PATCH_TOOL);
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
fn workspace_patch_proposal_reads_preimage_metadata_without_mutation() {
    let temp = TempWorkspace::new("patch-proposal");
    temp.write_text("dir/note.txt", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());

    let proposal = patch_proposal(&tools, "dir/note.txt", "old value", "newer value")
        .expect("valid patch should produce proposal");

    assert_eq!(proposal.tool_call_id().as_str(), "call-1");
    assert_eq!(proposal.tool_name().as_str(), WORKSPACE_PATCH_TOOL);
    assert_eq!(proposal.action_kind(), ToolActionKind::WorkspaceWrite);
    assert_eq!(proposal.label(), "workspace patch");
    assert_eq!(proposal.subject(), "dir/note.txt");
    assert!(
        proposal
            .summary()
            .contains("Apply 1 hunk(s) in dir/note.txt")
    );
    let patch = match proposal.evidence() {
        ActionProposalEvidence::WorkspacePatch(patch) => patch,
        ActionProposalEvidence::ProcessAction(_) => {
            panic!("workspace patch proposal must not produce process action evidence")
        }
    };
    assert_eq!(patch.relative_path(), "dir/note.txt");
    assert_eq!(patch.preimage_bytes(), "old value\n".len());
    assert_eq!(patch.replacement_bytes(), "newer value\n".len());
    assert_eq!(patch.file_bytes_before(), 22);
    assert_eq!(patch.file_bytes_after(), 24);
    assert_eq!(
        patch.file_fingerprint_before(),
        &stable_content_fingerprint("alpha\nold value\nomega\n".as_bytes())
    );
    assert_eq!(
        patch.file_fingerprint_after(),
        &stable_content_fingerprint("alpha\nnewer value\nomega\n".as_bytes())
    );
    assert_eq!(
        read_text(&temp.path().join("dir/note.txt")),
        "alpha\nold value\nomega\n"
    );
}

#[test]
fn workspace_patch_execute_after_stale_proposal_mismatch_fails_without_mutation() {
    let temp = TempWorkspace::new("patch-proposal-stale-fail-closed");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());

    let proposal = patch_proposal(&tools, "note.txt", "old", "replacement")
        .expect("initial valid patch should produce proposal");
    let proposed_patch = match proposal.evidence() {
        ActionProposalEvidence::WorkspacePatch(proposed_patch) => proposed_patch,
        ActionProposalEvidence::ProcessAction(_) => {
            panic!("workspace patch proposal must not produce process action evidence")
        }
    };
    assert_eq!(proposed_patch.file_bytes_before(), 16);
    assert_eq!(proposed_patch.file_bytes_after(), 24);
    assert_eq!(
        proposed_patch.file_fingerprint_before(),
        &stable_content_fingerprint("alpha\nold\nomega\n".as_bytes())
    );

    temp.write_text("note.txt", "intro\nalpha\nold\nomega\n");
    let outcome = workspace_patch_blocking_checked(
        &tools.state,
        WorkspacePatchArgs {
            patch: update_patch("note.txt", "old", "replacement"),
        },
        Some(proposed_patch),
        &|| false,
    )
    .expect("uncancelled workspace patch should not return cancellation");

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_PROPOSAL_MISMATCH,
        Some("note.txt"),
        temp.path(),
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "intro\nalpha\nold\nomega\n"
    );
    assert!(outcome.execution_evidence().is_none());
}

#[test]
fn workspace_patch_same_size_stale_proposal_fingerprint_mismatch_fails_without_leak_or_mutation() {
    let temp = TempWorkspace::new("patch-proposal-same-size-stale-fail-closed");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());

    let proposal = patch_proposal(&tools, "note.txt", "old", "new")
        .expect("initial valid patch should produce proposal");
    let proposed_patch = match proposal.evidence() {
        ActionProposalEvidence::WorkspacePatch(proposed_patch) => proposed_patch,
        ActionProposalEvidence::ProcessAction(_) => {
            panic!("workspace patch proposal must not produce process action evidence")
        }
    };
    assert_eq!(proposed_patch.file_bytes_before(), 16);
    assert_eq!(proposed_patch.file_bytes_after(), 16);

    temp.write_text("note.txt", "bravo\nold\nomega\n");
    let outcome = workspace_patch_blocking_checked(
        &tools.state,
        WorkspacePatchArgs {
            patch: update_patch("note.txt", "old", "new"),
        },
        Some(proposed_patch),
        &|| false,
    )
    .expect("same-size proposal mismatch should resolve as failed JSON");

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_PROPOSAL_MISMATCH,
        Some("note.txt"),
        temp.path(),
    );
    assert_no_provider_visible_patch_metadata(&outcome);
    assert_eq!(
        json_content(&outcome)["error"]["message"],
        WORKSPACE_PATCH_PLAN_CHANGED_MESSAGE
    );
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "bravo\nold\nomega\n"
    );
    assert!(outcome.execution_evidence().is_none());
}

#[test]
fn workspace_patch_preflight_returns_failed_outcome_for_invalid_or_stale_patch_without_mutation() {
    let temp = TempWorkspace::new("patch-proposal-none");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());

    let stale = patch_preflight(&tools, "note.txt", "missing", "new");
    let ToolActionPreflight::Outcome(stale) = stale else {
        panic!("stale preimage should produce a failed preflight outcome");
    };
    assert_failed_json_for_tool(
        &stale,
        WORKSPACE_PATCH_TOOL,
        ERROR_PREIMAGE_ABSENT,
        Some("note.txt"),
        temp.path(),
    );
    assert!(stale.execution_evidence().is_none());

    let invalid = patch_preflight(&tools, "../note.txt", "old", "new");
    let ToolActionPreflight::Outcome(invalid) = invalid else {
        panic!("invalid path should produce a failed preflight outcome");
    };
    assert_failed_json_for_tool(
        &invalid,
        WORKSPACE_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("../note.txt"),
        temp.path(),
    );
    assert!(invalid.execution_evidence().is_none());
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nold\nomega\n"
    );
}

#[test]
fn workspace_patch_stale_and_ambiguous_preimages_fail_without_mutation() {
    let temp = TempWorkspace::new("patch-preimage-failures");
    temp.write_text("stale.txt", "alpha\nbeta\n");
    temp.write_text("ambiguous.txt", "repeat\nmiddle\nrepeat\n");
    let tools = tools_for(temp.path());

    let stale = patch_outcome(&tools, "stale.txt", "gamma", "delta");
    assert_failed_json_for_tool(
        &stale,
        WORKSPACE_PATCH_TOOL,
        ERROR_PREIMAGE_ABSENT,
        Some("stale.txt"),
        temp.path(),
    );
    assert!(stale.execution_evidence().is_none());
    assert_eq!(read_text(&temp.path().join("stale.txt")), "alpha\nbeta\n");

    let ambiguous = patch_outcome(&tools, "ambiguous.txt", "repeat", "single");
    assert_failed_json_for_tool(
        &ambiguous,
        WORKSPACE_PATCH_TOOL,
        ERROR_PREIMAGE_AMBIGUOUS,
        Some("ambiguous.txt"),
        temp.path(),
    );
    assert!(ambiguous.execution_evidence().is_none());
    assert_eq!(
        read_text(&temp.path().join("ambiguous.txt")),
        "repeat\nmiddle\nrepeat\n"
    );

    temp.write_text(
        "ambiguous-status.txt",
        "const ENTRIES: &[Entry] = &[\n    Entry { key: \"status\", value: \"todo\" },\n    Entry { key: \"status\", value: \"todo\" },\n];\n",
    );
    let ambiguous_status = patch_outcome(
        &tools,
        "ambiguous-status.txt",
        "    Entry { key: \"status\", value: \"todo\" },",
        "    Entry { key: \"status\", value: \"done\" },",
    );
    assert_failed_json_for_tool(
        &ambiguous_status,
        WORKSPACE_PATCH_TOOL,
        ERROR_PREIMAGE_AMBIGUOUS,
        Some("ambiguous-status.txt"),
        temp.path(),
    );
    assert!(ambiguous_status.execution_evidence().is_none());
    assert_eq!(
        read_text(&temp.path().join("ambiguous-status.txt")),
        "const ENTRIES: &[Entry] = &[\n    Entry { key: \"status\", value: \"todo\" },\n    Entry { key: \"status\", value: \"todo\" },\n];\n"
    );
}

#[test]
fn workspace_patch_missing_or_ambiguous_after_proposal_still_does_not_write() {
    let temp = TempWorkspace::new("patch-post-proposal-failures");
    temp.write_text("missing-after-proposal.txt", "alpha\nold\nomega\n");
    temp.write_text("ambiguous-after-proposal.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());

    assert!(
        patch_proposal(&tools, "missing-after-proposal.txt", "old", "new").is_some(),
        "initial patch should be proposable"
    );
    fs::remove_file(temp.path().join("missing-after-proposal.txt"))
        .expect("workspace file should be removable");
    let missing = patch_outcome(&tools, "missing-after-proposal.txt", "old", "new");
    assert_failed_json_for_tool(
        &missing,
        WORKSPACE_PATCH_TOOL,
        ERROR_FILE_NOT_FOUND,
        Some("missing-after-proposal.txt"),
        temp.path(),
    );
    assert!(missing.execution_evidence().is_none());
    assert!(
        !temp.path().join("missing-after-proposal.txt").exists(),
        "missing preimage path should not be recreated"
    );

    assert!(
        patch_proposal(&tools, "ambiguous-after-proposal.txt", "old", "new").is_some(),
        "initial patch should be proposable"
    );
    temp.write_text("ambiguous-after-proposal.txt", "old\nmiddle\nold\n");
    let ambiguous = patch_outcome(&tools, "ambiguous-after-proposal.txt", "old", "new");
    assert_failed_json_for_tool(
        &ambiguous,
        WORKSPACE_PATCH_TOOL,
        ERROR_PREIMAGE_AMBIGUOUS,
        Some("ambiguous-after-proposal.txt"),
        temp.path(),
    );
    assert!(ambiguous.execution_evidence().is_none());
    assert_eq!(
        read_text(&temp.path().join("ambiguous-after-proposal.txt")),
        "old\nmiddle\nold\n"
    );
}

#[test]
fn workspace_patch_rejects_bad_hidden_missing_and_directory_paths_without_mutation() {
    let temp = TempWorkspace::new("patch-path-denied");
    temp.write_text("visible.txt", "old\n");
    temp.write_text(".secret", "old\n");
    fs::create_dir_all(temp.path().join("dir")).expect("directory should be created");
    let tools = tools_for(temp.path());

    for denied in [
        "/etc/passwd".to_owned(),
        "../outside.txt".to_owned(),
        ".secret".to_owned(),
        "dir/./file.txt".to_owned(),
    ] {
        let outcome = patch_outcome(&tools, &denied, "old", "new");
        let expected_path = if denied.starts_with('/') {
            None
        } else {
            Some(denied.as_str())
        };
        assert_failed_json_for_tool(
            &outcome,
            WORKSPACE_PATCH_TOOL,
            ERROR_PATH_DENIED,
            expected_path,
            temp.path(),
        );
    }
    assert_eq!(read_text(&temp.path().join(".secret")), "old\n");

    let missing = patch_outcome(&tools, "missing.txt", "old", "new");
    assert_failed_json_for_tool(
        &missing,
        WORKSPACE_PATCH_TOOL,
        ERROR_FILE_NOT_FOUND,
        Some("missing.txt"),
        temp.path(),
    );
    assert!(!temp.path().join("missing.txt").exists());

    let directory = patch_outcome(&tools, "dir", "old", "new");
    assert_failed_json_for_tool(
        &directory,
        WORKSPACE_PATCH_TOOL,
        ERROR_NOT_FILE,
        Some("dir"),
        temp.path(),
    );
}

#[cfg(unix)]
#[test]
fn workspace_patch_rejects_symlink_path_without_following_it() {
    let temp = TempWorkspace::new("patch-symlink");
    temp.write_text("target.txt", "old\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = patch_outcome(&tools, "link.txt", "old", "new");

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("link.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("target.txt")), "old\n");
}

#[cfg(unix)]
#[test]
fn workspace_patch_add_rejects_leaf_symlink_without_following_it() {
    let temp = TempWorkspace::new("patch-add-leaf-symlink");
    temp.write_text("target.txt", "old\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("link.txt", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("link.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("target.txt")), "old\n");
}

#[cfg(unix)]
#[test]
fn workspace_patch_add_rejects_intermediate_symlink_without_following_it() {
    let temp = TempWorkspace::new("patch-add-intermediate-symlink");
    fs::create_dir(temp.path().join("real")).expect("real directory should be created");
    symlink(temp.path().join("real"), temp.path().join("link-dir"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("link-dir/new.txt", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("link-dir/new.txt"),
        temp.path(),
    );
    assert!(!temp.path().join("real/new.txt").exists());
}

#[test]
fn workspace_patch_rejects_binary_and_limit_failures_without_mutation() {
    let temp = TempWorkspace::new("patch-binary-limits");
    temp.write_bytes("binary.txt", b"old\0value\n");
    temp.write_text("large-read.txt", "abcdef\n");
    temp.write_text("large-write.txt", "b\n");
    temp.write_text("large-payload.txt", "abc\n");

    let binary_tools = tools_for(temp.path());
    let binary = patch_outcome(&binary_tools, "binary.txt", "old", "new");
    assert_failed_json_for_tool(
        &binary,
        WORKSPACE_PATCH_TOOL,
        ERROR_NOT_UTF8,
        Some("binary.txt"),
        temp.path(),
    );
    assert_eq!(
        fs::read(temp.path().join("binary.txt")).expect("binary file should be readable"),
        b"old\0value\n"
    );

    let read_limited = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_read_bytes: 3,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");
    let too_large_read = patch_outcome(&read_limited, "large-read.txt", "abc", "x");
    assert_failed_json_for_tool(
        &too_large_read,
        WORKSPACE_PATCH_TOOL,
        ERROR_FILE_TOO_LARGE,
        Some("large-read.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("large-read.txt")), "abcdef\n");

    let payload_limited = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_patch_bytes: 3,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");
    let too_large_payload = patch_outcome(&payload_limited, "large-payload.txt", "ab", "cd");
    assert_failed_json_for_tool(
        &too_large_payload,
        WORKSPACE_PATCH_TOOL,
        ERROR_INVALID_ARGUMENTS,
        None,
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("large-payload.txt")), "abc\n");

    let write_limited = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_write_bytes: 4,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");
    let too_large_write = patch_outcome(&write_limited, "large-write.txt", "b", "bcdef");
    assert_failed_json_for_tool(
        &too_large_write,
        WORKSPACE_PATCH_TOOL,
        ERROR_FILE_TOO_LARGE,
        Some("large-write.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("large-write.txt")), "b\n");
}

#[test]
fn workspace_patch_cancellation_before_write_keeps_file_unchanged() {
    let temp = TempWorkspace::new("patch-cancel-before-write");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());
    let args = WorkspacePatchArgs {
        patch: update_patch("note.txt", "old", "new"),
    };
    let checks = Cell::new(0);
    let is_cancelled = || {
        let next = checks.get() + 1;
        checks.set(next);
        next >= 6
    };

    let err = workspace_patch_blocking_checked(&tools.state, args, None, &is_cancelled)
        .expect_err("cancellation before write should abort patch execution");

    assert!(matches!(err, ToolExecutionError::Cancelled));
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nold\nomega\n"
    );
}

fn mark_patch_cancelled_after_write(path: &Path) {
    fs::write(
        path.with_file_name("cancel-after-write.marker"),
        "cancelled",
    )
    .expect("post-write cancellation marker should be written");
}

#[test]
fn workspace_patch_cancellation_after_write_returns_durable_outcome() {
    let temp = TempWorkspace::new("patch-cancel-after-write");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());
    let note_path =
        fs::canonicalize(temp.path().join("note.txt")).expect("note path should canonicalize");
    install_patch_test_after_write_hook(note_path, mark_patch_cancelled_after_write);
    let args = WorkspacePatchArgs {
        patch: update_patch("note.txt", "old", "new"),
    };
    let cancel_marker = temp.path().join("cancel-after-write.marker");
    let is_cancelled = || cancel_marker.exists();

    let outcome = workspace_patch_blocking_checked(&tools.state, args, None, &is_cancelled)
        .expect("cancellation after write must return durable patch outcome");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(
        read_text(&temp.path().join("note.txt")),
        "alpha\nnew\nomega\n"
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
    assert_eq!(evidence.relative_path(), "note.txt");
    assert_eq!(evidence.file_bytes_after(), "alpha\nnew\nomega\n".len());
}
