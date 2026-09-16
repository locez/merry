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
    assert_eq!(
        json_content(&outcome)["changes"][0]["ignored_context_hunks"],
        1,
        "dropped context-only hunks should stay visible in the success envelope"
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
