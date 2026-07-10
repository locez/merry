use super::*;

#[test]
fn read_file_success_returns_stable_json_without_host_root() {
    let temp = TempWorkspace::new("read-success");
    temp.write_text("dir/note.txt", "alpha\nbeta\n");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "dir/note.txt");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(
        payload,
        json!({
            "ok": true,
            "tool": WORKSPACE_READ_FILE_TOOL,
            "path": "dir/note.txt",
            "bytes": 11,
            "truncated": false,
            "content": "alpha\nbeta\n"
        })
    );
    assert!(
        !outcome
            .content()
            .as_text()
            .expect("json content")
            .contains(temp.path().to_str().expect("temp path utf8")),
        "tool output must not include absolute host roots"
    );
    for forbidden in [
        "fingerprint",
        "fnv1a64",
        "preimage_bytes",
        "replacement_bytes",
    ] {
        assert!(
            !outcome
                .content()
                .as_text()
                .expect("json content")
                .contains(forbidden),
            "provider-visible patch output leaked {forbidden}"
        );
    }
}

#[test]
fn read_file_allows_empty_utf8_file() {
    let temp = TempWorkspace::new("empty-file");
    temp.write_text("empty.txt", "");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "empty.txt");
    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(payload["bytes"], 0);
    assert_eq!(payload["content"], "");
}

#[test]
fn read_file_rejects_absolute_parent_hidden_and_control_paths() {
    let temp = TempWorkspace::new("path-denied");
    temp.write_text("visible.txt", "ok\n");
    let tools = tools_for(temp.path());

    for denied in [
        "/etc/passwd".to_owned(),
        "../outside.txt".to_owned(),
        ".secret".to_owned(),
        format!("bad{}name", char::from(7)),
    ] {
        let outcome = read_outcome(&tools, &denied);
        let expected_path = if denied.starts_with('/') || denied.chars().any(char::is_control) {
            None
        } else {
            Some(denied.as_str())
        };
        assert_failed_json(&outcome, ERROR_PATH_DENIED, expected_path, temp.path());
    }
}

#[test]
fn read_file_reports_missing_non_utf8_and_limit_failures() {
    let temp = TempWorkspace::new("domain-failures");
    temp.write_bytes("binary.bin", &[0xff, 0xfe, 0xfd]);
    temp.write_text("large.txt", "abcdef");
    let tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_read_bytes: 5,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");

    let missing = read_outcome(&tools, "missing.txt");
    assert_failed_json(
        &missing,
        ERROR_FILE_NOT_FOUND,
        Some("missing.txt"),
        temp.path(),
    );

    let not_utf8 = read_outcome(&tools, "binary.bin");
    assert_failed_json(&not_utf8, ERROR_NOT_UTF8, Some("binary.bin"), temp.path());

    let too_large = read_outcome(&tools, "large.txt");
    assert_failed_json(
        &too_large,
        ERROR_FILE_TOO_LARGE,
        Some("large.txt"),
        temp.path(),
    );
}

#[test]
fn list_dir_allows_exact_root_dot_and_truncates_as_success() {
    let temp = TempWorkspace::new("list-root-limit");
    temp.write_text("c.txt", "c\n");
    temp.write_text("a.txt", "a\n");
    temp.write_text("b.txt", "b\n");
    temp.write_text("aa.txt", "aa\n");
    let tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_list_entries: 2,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");

    let outcome = list_outcome(&tools, ".");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(payload["path"], ".");
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["guidance"]["kind"], "workspace_list_truncated");
    assert_eq!(
        payload["entries"],
        json!([
            { "name": "a.txt", "path": "a.txt", "kind": "file" },
            { "name": "aa.txt", "path": "aa.txt", "kind": "file" }
        ])
    );
}

#[test]
fn list_dir_rejects_bad_paths_and_non_directory_domain_failure() {
    let temp = TempWorkspace::new("list-domain-failures");
    temp.write_text("file.txt", "content\n");
    let tools = tools_for(temp.path());

    let dot_component = list_outcome(&tools, "dir/./file");
    assert_failed_json_for_tool(
        &dot_component,
        WORKSPACE_LIST_DIR_TOOL,
        ERROR_PATH_DENIED,
        Some("dir/./file"),
        temp.path(),
    );

    let file = list_outcome(&tools, "file.txt");
    assert_failed_json_for_tool(
        &file,
        WORKSPACE_LIST_DIR_TOOL,
        ERROR_NOT_DIRECTORY,
        Some("file.txt"),
        temp.path(),
    );

    let missing = list_outcome(&tools, "missing");
    assert_failed_json_for_tool(
        &missing,
        WORKSPACE_LIST_DIR_TOOL,
        ERROR_PATH_NOT_FOUND,
        Some("missing"),
        temp.path(),
    );

    let absolute_parent = list_outcome(&tools, "/../outside");
    assert_failed_json_for_tool(
        &absolute_parent,
        WORKSPACE_LIST_DIR_TOOL,
        ERROR_PATH_DENIED,
        None,
        temp.path(),
    );
}

#[cfg(unix)]
#[test]
fn list_dir_rejects_requested_symlink_without_following_it() {
    let temp = TempWorkspace::new("list-symlink");
    fs::create_dir_all(temp.path().join("target")).expect("target directory should be created");
    symlink(temp.path().join("target"), temp.path().join("link"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = list_outcome(&tools, "link");

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_LIST_DIR_TOOL,
        ERROR_PATH_DENIED,
        Some("link"),
        temp.path(),
    );
}

#[test]
fn search_text_finds_case_sensitive_matches_in_stable_order() {
    let temp = TempWorkspace::new("search-success");
    temp.write_text("b.txt", "needle in b\nNeedle uppercase\n");
    temp.write_text("a.txt", "first\nneedle in a\n");
    temp.write_text("dir/c.txt", "needle in c\n");
    let tools = tools_for(temp.path());

    let outcome = search_outcome(&tools, "needle", None, None);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["tool"], WORKSPACE_SEARCH_TEXT_TOOL);
    assert_eq!(payload["query"], "needle");
    assert!(payload.get("path").is_none());
    assert_eq!(payload["searched_files"], 3);
    assert_eq!(payload["truncated"], false);
    assert_eq!(
        payload["matches"],
        json!([
            { "path": "a.txt", "line_number": 2, "line": "needle in a", "truncated": false },
            { "path": "b.txt", "line_number": 1, "line": "needle in b", "truncated": false },
            { "path": "dir/c.txt", "line_number": 1, "line": "needle in c", "truncated": false }
        ])
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
fn search_text_query_is_a_regular_expression() {
    let temp = TempWorkspace::new("search-regex");
    temp.write_text(
        "runtime.rs",
        "hard_watermark\nsoft_watermark\nauto_compaction\ncompaction\n",
    );
    let tools = tools_for(temp.path());

    let outcome = search_outcome(
        &tools,
        "^(hard_watermark|auto_compaction)$",
        Some("runtime.rs"),
        None,
    );

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(
        payload["matches"],
        json!([
            { "path": "runtime.rs", "line_number": 1, "line": "hard_watermark", "truncated": false },
            { "path": "runtime.rs", "line_number": 3, "line": "auto_compaction", "truncated": false }
        ])
    );
}

#[test]
fn search_text_rejects_invalid_regular_expressions() {
    let temp = TempWorkspace::new("search-invalid-regex");
    temp.write_text("runtime.rs", "auto_compaction\n");
    let tools = tools_for(temp.path());

    let outcome = search_outcome(&tools, "(auto_compaction", Some("runtime.rs"), None);

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_INVALID_ARGUMENTS,
        None,
        temp.path(),
    );
}

#[test]
fn search_text_returns_success_for_no_match() {
    let temp = TempWorkspace::new("search-no-match");
    temp.write_text("note.txt", "alpha\n");
    let tools = tools_for(temp.path());

    let outcome = search_outcome(&tools, "needle", Some("note.txt"), None);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(payload["path"], "note.txt");
    assert_eq!(payload["matches"], json!([]));
    assert_eq!(payload["truncated"], false);
}

#[test]
fn search_text_respects_match_file_query_and_line_limits() {
    let temp = TempWorkspace::new("search-limits");
    temp.write_text("a.txt", "needle abcdef\nneedle again\n");
    temp.write_text("b.txt", "needle in b\n");
    let tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_search_matches: 5,
                max_search_files: 1,
                max_search_line_bytes: 10,
                max_search_query_bytes: 6,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");

    let limited = search_outcome(&tools, "needle", None, Some(1));
    assert_eq!(limited.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&limited);
    assert_eq!(payload["searched_files"], 1);
    assert_eq!(payload["truncated"], true);
    assert_eq!(
        payload["matches"],
        json!([
            { "path": "a.txt", "line_number": 1, "line": "needle abc", "truncated": true }
        ])
    );

    let file_limited = search_outcome(&tools, "absent", None, None);
    assert_eq!(file_limited.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&file_limited);
    assert_eq!(payload["searched_files"], 1);
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["matches"], json!([]));

    let too_long = search_outcome(&tools, "needles", None, None);
    assert_failed_json_for_tool(
        &too_long,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_INVALID_ARGUMENTS,
        None,
        temp.path(),
    );
}

#[test]
fn search_text_total_byte_limit_truncates_without_scanning_next_file() {
    let temp = TempWorkspace::new("search-total-bytes");
    temp.write_text("a.txt", "abc\n");
    temp.write_text("b.txt", "needle\n");
    let tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_search_bytes: 4,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");

    let outcome = search_outcome(&tools, "needle", None, None);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(payload["searched_files"], 1);
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["matches"], json!([]));
    assert_eq!(payload["guidance"]["kind"], "workspace_search_limited");
}

#[test]
fn search_text_entry_limit_truncates_recursive_enumeration() {
    let temp = TempWorkspace::new("search-entry-limit");
    temp.write_text("a.txt", "absent\n");
    temp.write_text("b.txt", "needle\n");
    let tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_search_entries: 1,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");

    let outcome = search_outcome(&tools, "needle", None, None);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(payload["searched_files"], 0);
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["matches"], json!([]));
}

#[test]
fn search_text_counts_skipped_hidden_non_utf8_symlink_and_too_large() {
    let temp = TempWorkspace::new("search-skips");
    temp.write_text("visible.txt", "needle\n");
    temp.write_text(".hidden.txt", "needle hidden\n");
    temp.write_bytes("binary.bin", &[0xff, 0xfe, 0xfd]);
    temp.write_text("large.txt", "needle large\n");
    #[cfg(unix)]
    symlink(
        temp.path().join("visible.txt"),
        temp.path().join("linked.txt"),
    )
    .expect("symlink should be created");
    let tools = ReadOnlyWorkspaceTools::new(
        WorkspaceToolsConfig::new(vec![temp.path().to_path_buf()]).with_limits(
            WorkspaceToolLimits {
                max_read_bytes: 6,
                ..WorkspaceToolLimits::default()
            },
        ),
    )
    .expect("workspace tools should construct");

    let outcome = search_outcome(&tools, "needle", None, None);

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(payload["matches"], json!([]));
    assert_eq!(payload["skipped"]["hidden"], 1);
    assert_eq!(payload["skipped"]["non_utf8"], 1);
    assert_eq!(payload["skipped"]["too_large"], 2);
    assert_eq!(payload["guidance"]["kind"], "workspace_search_limited");
    #[cfg(unix)]
    assert_eq!(payload["skipped"]["symlink"], 1);
}

#[test]
fn search_text_rejects_bad_path_and_missing_path_domain_failure() {
    let temp = TempWorkspace::new("search-domain-failures");
    let tools = tools_for(temp.path());

    let denied = search_outcome(&tools, "needle", Some("../outside"), None);
    assert_failed_json_for_tool(
        &denied,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_PATH_DENIED,
        Some("../outside"),
        temp.path(),
    );

    let missing = search_outcome(&tools, "needle", Some("missing.txt"), None);
    assert_failed_json_for_tool(
        &missing,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_PATH_NOT_FOUND,
        Some("missing.txt"),
        temp.path(),
    );

    let empty_query = search_outcome(&tools, "", None, None);
    assert_failed_json_for_tool(
        &empty_query,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_INVALID_ARGUMENTS,
        None,
        temp.path(),
    );

    let multiline_query = search_outcome(&tools, "need\nle", None, None);
    assert_failed_json_for_tool(
        &multiline_query,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_INVALID_ARGUMENTS,
        None,
        temp.path(),
    );

    let control_query = search_outcome(&tools, &format!("bad{}query", char::from(7)), None, None);
    assert_failed_json_for_tool(
        &control_query,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_INVALID_ARGUMENTS,
        None,
        temp.path(),
    );

    let absolute_parent = search_outcome(&tools, "needle", Some("/../outside"), None);
    assert_failed_json_for_tool(
        &absolute_parent,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_PATH_DENIED,
        None,
        temp.path(),
    );
}

#[cfg(unix)]
#[test]
fn search_text_rejects_requested_symlink_without_following_it() {
    let temp = TempWorkspace::new("search-symlink");
    temp.write_text("target.txt", "needle\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = search_outcome(&tools, "needle", Some("link.txt"), None);

    assert_failed_json_for_tool(
        &outcome,
        WORKSPACE_SEARCH_TEXT_TOOL,
        ERROR_PATH_DENIED,
        Some("link.txt"),
        temp.path(),
    );
}

#[test]
fn read_file_executor_returns_cancelled_when_token_is_cancelled() {
    let temp = TempWorkspace::new("cancelled");
    temp.write_text("note.txt", "ok\n");
    let tools = tools_for(temp.path());
    let executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let call = pending_call(json!({ "path": "note.txt" }));
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime should build");

    let err = runtime
        .block_on(executor.execute(call, ToolExecutionContext::new(token)))
        .expect_err("cancelled execution should return cancellation error");

    assert!(matches!(err, ToolExecutionError::Cancelled));
}

#[test]
fn list_and_search_executors_return_cancelled_when_token_is_cancelled() {
    let temp = TempWorkspace::new("list-search-cancelled");
    temp.write_text("note.txt", "needle\n");
    let tools = tools_for(temp.path());
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime should build");

    let list_executor = ListDirExecutor {
        state: Arc::clone(&tools.state),
    };
    let list_call = pending_call_for(WORKSPACE_LIST_DIR_TOOL, json!({ "path": "." }));
    let list_err = runtime
        .block_on(list_executor.execute(list_call, ToolExecutionContext::new(token.clone())))
        .expect_err("cancelled list execution should return cancellation error");
    assert!(matches!(list_err, ToolExecutionError::Cancelled));

    let search_executor = SearchTextExecutor {
        state: Arc::clone(&tools.state),
    };
    let search_call = pending_call_for(WORKSPACE_SEARCH_TEXT_TOOL, json!({ "query": "needle" }));
    let search_err = runtime
        .block_on(search_executor.execute(search_call, ToolExecutionContext::new(token)))
        .expect_err("cancelled search execution should return cancellation error");
    assert!(matches!(search_err, ToolExecutionError::Cancelled));
}

#[cfg(unix)]
#[test]
fn unix_open_file_for_read_rejects_trailing_symlink() {
    let temp = TempWorkspace::new("open-nofollow");
    temp.write_text("target.txt", "secret\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");

    let error = open_file_for_read(temp.path().join("link.txt").as_path())
        .expect_err("O_NOFOLLOW open should reject trailing symlink");

    assert_eq!(error.code, ERROR_PATH_DENIED);
}

#[test]
fn read_file_argument_validation_returns_domain_failure() {
    let temp = TempWorkspace::new("bad-args");
    let tools = tools_for(temp.path());
    let executor = ReadFileExecutor {
        state: Arc::clone(&tools.state),
    };
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::Number(1.into()));
    let call = pending_call(Value::Object(args));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime should build");

    let outcome = runtime
        .block_on(executor.execute(call, ToolExecutionContext::default()))
        .expect("invalid args should be domain failure");

    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_failed_json(&outcome, ERROR_INVALID_ARGUMENTS, None, temp.path());
}

#[test]
fn list_dir_success_returns_sorted_non_hidden_entries_and_symlink_kind() {
    let temp = TempWorkspace::new("list-success");
    temp.write_text("root/b.txt", "b\n");
    temp.write_text("root/a.txt", "a\n");
    fs::create_dir_all(temp.path().join("root/dir")).expect("directory should be created");
    temp.write_text("root/.secret", "hidden\n");
    #[cfg(unix)]
    symlink(
        temp.path().join("root/a.txt"),
        temp.path().join("root/link.txt"),
    )
    .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = list_outcome(&tools, "root");

    assert_eq!(outcome.status(), ToolCallResultStatus::Succeeded);
    let payload = json_content(&outcome);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["tool"], WORKSPACE_LIST_DIR_TOOL);
    assert_eq!(payload["path"], "root");
    assert_eq!(payload["truncated"], false);

    #[cfg(unix)]
    assert_eq!(
        payload["entries"],
        json!([
            { "name": "a.txt", "path": "root/a.txt", "kind": "file" },
            { "name": "b.txt", "path": "root/b.txt", "kind": "file" },
            { "name": "dir", "path": "root/dir", "kind": "directory" },
            { "name": "link.txt", "path": "root/link.txt", "kind": "symlink" }
        ])
    );

    #[cfg(not(unix))]
    assert_eq!(
        payload["entries"],
        json!([
            { "name": "a.txt", "path": "root/a.txt", "kind": "file" },
            { "name": "b.txt", "path": "root/b.txt", "kind": "file" },
            { "name": "dir", "path": "root/dir", "kind": "directory" }
        ])
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

#[cfg(unix)]
#[test]
fn read_file_rejects_symlink_without_following_it() {
    let temp = TempWorkspace::new("symlink");
    temp.write_text("target.txt", "secret\n");
    symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
        .expect("symlink should be created");
    let tools = tools_for(temp.path());

    let outcome = read_outcome(&tools, "link.txt");
    assert_eq!(outcome.status(), ToolCallResultStatus::Failed);
    assert_eq!(
        outcome.diagnostic().expect("diagnostic").code(),
        ERROR_PATH_DENIED
    );
}
