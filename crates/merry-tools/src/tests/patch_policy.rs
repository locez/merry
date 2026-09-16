use super::*;

#[test]
fn apply_patch_proposal_reads_preimage_metadata_without_mutation() {
    let temp = TempWorkspace::new("patch-proposal");
    temp.write_text("dir/note.txt", "alpha\nold value\nomega\n");
    let tools = tools_for(temp.path());

    let proposal = patch_proposal(&tools, "dir/note.txt", "old value", "newer value")
        .expect("valid patch should produce proposal");

    assert_eq!(proposal.tool_call_id().as_str(), "call-1");
    assert_eq!(proposal.tool_name().as_str(), APPLY_PATCH_TOOL);
    assert_eq!(proposal.action_kind(), ToolActionKind::WorkspaceWrite);
    assert_eq!(proposal.label(), "workspace patch");
    assert_eq!(proposal.subject(), "dir/note.txt");
    assert_eq!(
        proposal.summary(),
        "Apply 1 hunk(s) in dir/note.txt (3 -> 3 lines, 22 -> 24 bytes).",
        "reviewers see line counts first, then byte counts"
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
fn apply_patch_execute_after_stale_proposal_mismatch_fails_without_mutation() {
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
    let outcome = apply_patch_blocking_checked(
        &tools.state,
        ApplyPatchInput {
            patch: update_patch("note.txt", "old", "replacement"),
        },
        Some(proposed_patch),
        &|| false,
    )
    .expect("uncancelled workspace patch should not return cancellation");

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
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
fn apply_patch_same_size_stale_proposal_fingerprint_mismatch_fails_without_leak_or_mutation() {
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
    let outcome = apply_patch_blocking_checked(
        &tools.state,
        ApplyPatchInput {
            patch: update_patch("note.txt", "old", "new"),
        },
        Some(proposed_patch),
        &|| false,
    )
    .expect("same-size proposal mismatch should resolve as failed JSON");

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
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
fn apply_patch_preflight_returns_failed_outcome_for_invalid_or_stale_patch_without_mutation() {
    let temp = TempWorkspace::new("patch-proposal-none");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());

    let stale = patch_preflight(&tools, "note.txt", "missing", "new");
    let ToolActionPreflight::Outcome(stale) = stale else {
        panic!("stale preimage should produce a failed preflight outcome");
    };
    assert_failed_json_for_tool(
        &stale,
        APPLY_PATCH_TOOL,
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
        APPLY_PATCH_TOOL,
        ERROR_FILE_NOT_FOUND,
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
fn apply_patch_stale_and_ambiguous_preimages_fail_without_mutation() {
    let temp = TempWorkspace::new("patch-preimage-failures");
    temp.write_text("stale.txt", "alpha\nbeta\n");
    temp.write_text("ambiguous.txt", "repeat\nmiddle\nrepeat\n");
    let tools = tools_for(temp.path());

    let stale = patch_outcome(&tools, "stale.txt", "gamma", "delta");
    assert_failed_json_for_tool(
        &stale,
        APPLY_PATCH_TOOL,
        ERROR_PREIMAGE_ABSENT,
        Some("stale.txt"),
        temp.path(),
    );
    assert!(stale.execution_evidence().is_none());
    assert_eq!(read_text(&temp.path().join("stale.txt")), "alpha\nbeta\n");

    let ambiguous = patch_outcome(&tools, "ambiguous.txt", "repeat", "single");
    assert_failed_json_for_tool(
        &ambiguous,
        APPLY_PATCH_TOOL,
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
        APPLY_PATCH_TOOL,
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
fn apply_patch_missing_or_ambiguous_after_proposal_still_does_not_write() {
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
        APPLY_PATCH_TOOL,
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
        APPLY_PATCH_TOOL,
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
fn apply_patch_reports_missing_and_directory_targets_without_mutation() {
    let temp = TempWorkspace::new("patch-path-denied");
    temp.write_text("visible.txt", "old\n");
    temp.write_text(".secret", "old\n");
    fs::create_dir_all(temp.path().join("dir")).expect("directory should be created");
    let tools = tools_for(temp.path());

    // A dot-prefixed name is ordinary spelling, so a patch may edit it, and a
    // path outside the workspace is resolved as the caller named it rather than
    // being rejected by a second path policy inside the tool.
    let hidden = patch_outcome(&tools, ".secret", "old", "new");
    assert_eq!(hidden.status(), ToolCallResultStatus::Succeeded);
    assert_eq!(read_text(&temp.path().join(".secret")), "new\n");

    let outside = patch_outcome(&tools, "../outside.txt", "old", "new");
    assert_failed_json_for_tool(
        &outside,
        APPLY_PATCH_TOOL,
        ERROR_FILE_NOT_FOUND,
        Some("../outside.txt"),
        temp.path(),
    );

    // A redundant `.` segment is spelling, not a path error: the section is
    // normalized and the reported path is the workspace-relative form.
    let normalized = patch_outcome(&tools, "dir/./file.txt", "old", "new");
    assert_failed_json_for_tool(
        &normalized,
        APPLY_PATCH_TOOL,
        ERROR_FILE_NOT_FOUND,
        Some("dir/file.txt"),
        temp.path(),
    );

    let missing = patch_outcome(&tools, "missing.txt", "old", "new");
    assert_failed_json_for_tool(
        &missing,
        APPLY_PATCH_TOOL,
        ERROR_FILE_NOT_FOUND,
        Some("missing.txt"),
        temp.path(),
    );
    assert!(!temp.path().join("missing.txt").exists());

    let directory = patch_outcome(&tools, "dir", "old", "new");
    assert_failed_json_for_tool(
        &directory,
        APPLY_PATCH_TOOL,
        ERROR_NOT_FILE,
        Some("dir"),
        temp.path(),
    );
}

#[cfg(unix)]
#[test]
fn apply_patch_rejects_symlink_path_without_following_it() {
    let temp = TempWorkspace::new("patch-symlink");
    temp.write_text("target.txt", "old\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = patch_outcome(&tools, "link.txt", "old", "new");

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("link.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("target.txt")), "old\n");
}

#[cfg(unix)]
#[test]
fn apply_patch_add_rejects_leaf_symlink_without_following_it() {
    let temp = TempWorkspace::new("patch-add-leaf-symlink");
    temp.write_text("target.txt", "old\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("link.txt", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("link.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("target.txt")), "old\n");
}

#[cfg(unix)]
#[test]
fn apply_patch_add_rejects_intermediate_symlink_without_following_it() {
    let temp = TempWorkspace::new("patch-add-intermediate-symlink");
    fs::create_dir(temp.path().join("real")).expect("real directory should be created");
    symlink(temp.path().join("real"), temp.path().join("link-dir"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = patch_text_outcome(&tools, &add_patch("link-dir/new.txt", &["new"]));

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("link-dir/new.txt"),
        temp.path(),
    );
    assert!(!temp.path().join("real/new.txt").exists());
}

#[test]
fn apply_patch_rejects_binary_and_limit_failures_without_mutation() {
    let temp = TempWorkspace::new("patch-binary-limits");
    temp.write_bytes("binary.txt", b"old\0value\n");
    temp.write_text("large-read.txt", "abcdef\n");
    temp.write_text("large-write.txt", "b\n");
    temp.write_text("large-payload.txt", "abc\n");

    let binary_tools = tools_for(temp.path());
    let binary = patch_outcome(&binary_tools, "binary.txt", "old", "new");
    assert_failed_json_for_tool(
        &binary,
        APPLY_PATCH_TOOL,
        ERROR_NOT_UTF8,
        Some("binary.txt"),
        temp.path(),
    );
    assert_eq!(
        fs::read(temp.path().join("binary.txt")).expect("binary file should be readable"),
        b"old\0value\n"
    );

    let read_limited = WorkspaceTools::new(
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
        APPLY_PATCH_TOOL,
        ERROR_FILE_TOO_LARGE,
        Some("large-read.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("large-read.txt")), "abcdef\n");

    let payload_limited = WorkspaceTools::new(
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
        APPLY_PATCH_TOOL,
        ERROR_INVALID_ARGUMENTS,
        None,
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("large-payload.txt")), "abc\n");

    let write_limited = WorkspaceTools::new(
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
        APPLY_PATCH_TOOL,
        ERROR_FILE_TOO_LARGE,
        Some("large-write.txt"),
        temp.path(),
    );
    assert_eq!(read_text(&temp.path().join("large-write.txt")), "b\n");
}

#[test]
fn apply_patch_cancellation_before_write_keeps_file_unchanged() {
    let temp = TempWorkspace::new("patch-cancel-before-write");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());
    let args = ApplyPatchInput {
        patch: update_patch("note.txt", "old", "new"),
    };
    let checks = Cell::new(0);
    let is_cancelled = || {
        let next = checks.get() + 1;
        checks.set(next);
        next >= 6
    };

    let err = apply_patch_blocking_checked(&tools.state, args, None, &is_cancelled)
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
fn apply_patch_delete_respects_write_scope_and_forbidden_paths() {
    let temp = TempWorkspace::new("patch-delete-scope");
    temp.write_text("allowed/note.txt", "alpha\n");
    temp.write_text("denied/note.txt", "alpha\n");
    temp.write_text("allowed/secret.txt", "alpha\n");
    let tools = WorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()])
            .with_patch_write_scope(Some(vec![PathBuf::from("allowed")]))
            .with_forbidden_paths(vec![PathBuf::from("allowed/secret.txt")]),
    )
    .expect("workspace tools should construct");

    let allowed = patch_text_outcome(&tools, &delete_patch("allowed/note.txt"));
    assert_eq!(allowed.status(), ToolCallResultStatus::Succeeded);
    assert!(!temp.path().join("allowed/note.txt").exists());

    let outside_scope = patch_text_outcome(&tools, &delete_patch("denied/note.txt"));
    assert_failed_json_for_tool(
        &outside_scope,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("denied/note.txt"),
        temp.path(),
    );
    assert!(temp.path().join("denied/note.txt").exists());

    let forbidden = patch_text_outcome(&tools, &delete_patch("allowed/secret.txt"));
    assert_failed_json_for_tool(
        &forbidden,
        APPLY_PATCH_TOOL,
        ERROR_PATH_DENIED,
        Some("allowed/secret.txt"),
        temp.path(),
    );
    assert!(temp.path().join("allowed/secret.txt").exists());
}

/// Rewrites a planned target between planning and mutation.
fn rewrite_file_before_mutation(path: &Path) {
    fs::write(path, "changed after planning\n").expect("test hook should rewrite the target file");
}

#[test]
fn apply_patch_delete_refuses_file_changed_before_mutation() {
    let temp = TempWorkspace::new("patch-delete-stale-mutation");
    temp.write_text("note.txt", "alpha\nbeta\n");
    let tools = tools_for(temp.path());
    let note_path =
        fs::canonicalize(temp.path().join("note.txt")).expect("note path should canonicalize");
    install_patch_test_before_mutation_hook(note_path.clone(), rewrite_file_before_mutation);

    let outcome = patch_text_outcome(&tools, &delete_patch("note.txt"));

    assert_failed_json_for_tool(
        &outcome,
        APPLY_PATCH_TOOL,
        ERROR_WRITE_FAILED,
        Some("note.txt"),
        temp.path(),
    );
    assert_eq!(
        read_text(&note_path),
        "changed after planning\n",
        "a delete must not remove content that replaced the planned preimage"
    );
    assert!(
        outcome.execution_evidence().is_none(),
        "a refused delete must not report execution evidence for content it did not remove"
    );
}

#[cfg(unix)]
#[test]
fn apply_patch_delete_removes_write_protected_file() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempWorkspace::new("patch-delete-write-protected");
    temp.write_text("note.txt", "alpha\n");
    let tools = tools_for(temp.path());
    let note_path = temp.path().join("note.txt");
    fs::set_permissions(&note_path, fs::Permissions::from_mode(0o444))
        .expect("note file should be made write-protected");

    let outcome = patch_text_outcome(&tools, &delete_patch("note.txt"));

    assert_eq!(
        outcome.status(),
        ToolCallResultStatus::Succeeded,
        "removal needs write permission on the parent directory, not the file"
    );
    assert!(!note_path.exists());
}

#[test]
fn apply_patch_cancellation_after_write_returns_durable_outcome() {
    let temp = TempWorkspace::new("patch-cancel-after-write");
    temp.write_text("note.txt", "alpha\nold\nomega\n");
    let tools = tools_for(temp.path());
    let note_path =
        fs::canonicalize(temp.path().join("note.txt")).expect("note path should canonicalize");
    install_patch_test_after_write_hook(note_path, mark_patch_cancelled_after_write);
    let args = ApplyPatchInput {
        patch: update_patch("note.txt", "old", "new"),
    };
    let cancel_marker = temp.path().join("cancel-after-write.marker");
    let is_cancelled = || cancel_marker.exists();

    let outcome = apply_patch_blocking_checked(&tools.state, args, None, &is_cancelled)
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
